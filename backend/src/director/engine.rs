//! `DirectorEngine`: context in, validated decision out.
//!
//! ```text
//! validate context -> provider -> parse + validate
//!                        ^             | rejected
//!                        +-- one repair attempt
//! ```
//!
//! The engine is async, holds no lock and touches no session state, so it can
//! run in a spawned task while gameplay continues. It makes at most
//! [`MAX_ATTEMPTS`] provider calls per decision and never retries a provider
//! failure.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use tracing::{debug, warn};

use super::context::DirectorContext;
use super::decision::{DecisionMetadata, DirectorDecision};
use super::fallback::FallbackDirector;
use super::prompt::MAX_REPAIR_OUTPUT_CHARS;
use super::provider::{
    DirectorProvider, ProviderError, ProviderErrorKind, ProviderRequest, RepairRequest,
};
use super::truncate_chars;
use super::validate::{ValidationIssue, parse_proposal};

/// The first attempt plus exactly one repair. Never a loop.
pub const MAX_ATTEMPTS: u8 = 2;

/// Upper bound on one provider call, enforced by the engine whatever the
/// provider does. A decision therefore takes at most twice this.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);

fn summarize(issues: &[ValidationIssue]) -> String {
    let shown: Vec<String> = issues.iter().take(3).map(ToString::to_string).collect();
    match issues.len().saturating_sub(shown.len()) {
        0 => shown.join("; "),
        more => format!("{}; and {more} more", shown.join("; ")),
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DirectorError {
    /// The context itself is invalid. No provider was called.
    #[error("invalid director context: {}", summarize(.0))]
    InvalidContext(Vec<ValidationIssue>),
    /// The provider failed (timeout, 429, 503, auth...). Not retried.
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// The provider answered, but its output failed validation on every
    /// allowed attempt. Nothing from it was accepted.
    #[error("director output rejected after {attempts} attempt(s): {}", summarize(.issues))]
    InvalidDecision {
        attempts: u8,
        issues: Vec<ValidationIssue>,
    },
}

#[derive(Clone)]
pub struct DirectorEngine {
    provider: Arc<dyn DirectorProvider>,
    fallback: Option<Arc<dyn DirectorProvider>>,
    call_timeout: Duration,
}

impl std::fmt::Debug for DirectorEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectorEngine")
            .field("provider", &self.provider.name())
            .field("fallback", &self.fallback.as_ref().map(|p| p.name()))
            .field("call_timeout", &self.call_timeout)
            .finish()
    }
}

impl DirectorEngine {
    pub fn new(provider: Arc<dyn DirectorProvider>) -> Self {
        Self {
            provider,
            fallback: None,
            call_timeout: DEFAULT_CALL_TIMEOUT,
        }
    }

    /// An engine that never calls a model: the deterministic rules only.
    pub fn deterministic() -> Self {
        Self::new(Arc::new(FallbackDirector))
    }

    /// Use `fallback` when the primary provider fails or its output is
    /// rejected. The decision records why in `metadata.fallback_reason`.
    pub fn with_fallback(mut self, fallback: Arc<dyn DirectorProvider>) -> Self {
        self.fallback = Some(fallback);
        self
    }

    pub fn with_call_timeout(mut self, call_timeout: Duration) -> Self {
        self.call_timeout = call_timeout;
        self
    }

    /// Decide how the world reacts to `ctx`.
    ///
    /// Returns a decision whose every action passed validation, or an error.
    /// Nothing is ever partially accepted.
    pub async fn decide(&self, ctx: &DirectorContext) -> Result<DirectorDecision, DirectorError> {
        ctx.validate().map_err(DirectorError::InvalidContext)?;

        let primary = match self.run(self.provider.as_ref(), ctx, MAX_ATTEMPTS).await {
            Ok(decision) => return Ok(decision),
            Err(error) => error,
        };
        let Some(fallback) = &self.fallback else {
            return Err(primary);
        };
        warn!(
            session_id = %ctx.session_id,
            provider = self.provider.name(),
            fallback = fallback.name(),
            error = %primary,
            "director provider failed; using fallback"
        );
        match self.run(fallback.as_ref(), ctx, 1).await {
            Ok(mut decision) => {
                decision.metadata.fallback_reason = Some(truncate_chars(&primary.to_string(), 300));
                Ok(decision)
            }
            Err(error) => {
                warn!(session_id = %ctx.session_id, error = %error, "director fallback failed");
                Err(primary)
            }
        }
    }

    async fn run(
        &self,
        provider: &dyn DirectorProvider,
        ctx: &DirectorContext,
        max_attempts: u8,
    ) -> Result<DirectorDecision, DirectorError> {
        let started = Instant::now();
        let mut usage: BTreeMap<String, u64> = BTreeMap::new();
        let mut rejected: Option<(String, Vec<ValidationIssue>)> = None;

        for attempt in 1..=max_attempts {
            let request = ProviderRequest {
                context: ctx,
                repair: rejected.as_ref().map(|(output, issues)| RepairRequest {
                    rejected_output: output,
                    issues,
                }),
            };
            let output = tokio::time::timeout(self.call_timeout, provider.propose(request))
                .await
                .map_err(|_| {
                    ProviderError::new(
                        provider.name(),
                        ProviderErrorKind::Timeout,
                        format!("no response within {} ms", self.call_timeout.as_millis()),
                    )
                })??;
            for (key, value) in &output.usage {
                *usage.entry(key.clone()).or_default() += value;
            }

            match parse_proposal(ctx, &output.text) {
                Ok(proposal) => {
                    let metadata = DecisionMetadata {
                        provider: provider.name().to_owned(),
                        model: output.model,
                        attempts: attempt,
                        repaired: attempt > 1,
                        latency_ms: u64::try_from(started.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                        usage,
                        created_at: Utc::now(),
                        based_on_event_count: ctx.event_count,
                        fallback_reason: None,
                    };
                    return Ok(DirectorDecision::from_validated(ctx, proposal, metadata));
                }
                Err(issues) => {
                    debug!(
                        session_id = %ctx.session_id,
                        provider = provider.name(),
                        attempt,
                        issues = issues.len(),
                        "director output rejected"
                    );
                    let output: String =
                        output.text.chars().take(MAX_REPAIR_OUTPUT_CHARS).collect();
                    rejected = Some((output, issues));
                }
            }
        }

        Err(DirectorError::InvalidDecision {
            attempts: max_attempts,
            issues: rejected.map(|(_, issues)| issues).unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::director::context::Trigger;
    use crate::director::decision::ReasonCode;
    use crate::director::provider::{ScriptedProvider, ScriptedResponse};
    use crate::director::testing::{sample_context, sample_proposal};
    use crate::director::validate::IssueCode;
    use serde_json::json;

    fn valid_output() -> String {
        serde_json::to_string(&sample_proposal()).unwrap()
    }

    fn engine(provider: &Arc<ScriptedProvider>) -> DirectorEngine {
        DirectorEngine::new(provider.clone())
    }

    fn failing(kind: ProviderErrorKind, status: u16, detail: &str) -> ScriptedResponse {
        ScriptedResponse::Error(ProviderError::new("gemini", kind, detail).with_status(status))
    }

    #[tokio::test]
    async fn accepts_a_valid_proposal_on_the_first_attempt() {
        let ctx = sample_context();
        let provider = Arc::new(ScriptedProvider::texts([valid_output()]));
        let decision = engine(&provider).decide(&ctx).await.unwrap();

        assert_eq!(decision.proposal(), sample_proposal());
        assert_eq!(decision.session_id, ctx.session_id);
        assert_eq!(decision.universe_id, ctx.universe_id);
        assert_eq!(decision.trigger, ctx.trigger);
        assert_eq!(decision.schema_version, 1);
        assert_eq!(decision.metadata.provider, "scripted");
        assert_eq!(decision.metadata.model, "fixture");
        assert_eq!(decision.metadata.attempts, 1);
        assert!(!decision.metadata.repaired);
        assert_eq!(decision.metadata.based_on_event_count, ctx.event_count);
        assert_eq!(decision.metadata.fallback_reason, None);
        assert_eq!(provider.call_count(), 1);
        assert!(!provider.calls()[0].is_repair());
    }

    #[tokio::test]
    async fn decision_ids_are_unique() {
        let ctx = sample_context();
        let provider = Arc::new(ScriptedProvider::texts([valid_output(), valid_output()]));
        let engine = engine(&provider);
        let first = engine.decide(&ctx).await.unwrap();
        let second = engine.decide(&ctx).await.unwrap();
        assert_ne!(first.decision_id, second.decision_id);
        assert_eq!(first.actions, second.actions);
    }

    #[tokio::test]
    async fn malformed_output_gets_exactly_one_repair_attempt() {
        let ctx = sample_context();
        let provider = Arc::new(ScriptedProvider::texts([
            "Sure! Here is what should happen next...".to_owned(),
            valid_output(),
        ]));
        let decision = engine(&provider).decide(&ctx).await.unwrap();

        assert_eq!(decision.metadata.attempts, 2);
        assert!(decision.metadata.repaired);
        assert_eq!(decision.proposal(), sample_proposal());

        let calls = provider.calls();
        assert_eq!(calls.len(), 2);
        assert!(!calls[0].is_repair());
        assert_eq!(
            calls[1].rejected_output.as_deref(),
            Some("Sure! Here is what should happen next...")
        );
        assert_eq!(calls[1].issues.len(), 1);
        assert_eq!(calls[1].issues[0].code, IssueCode::Malformed);
    }

    #[tokio::test]
    async fn repair_request_carries_the_semantic_errors() {
        let ctx = sample_context();
        let invalid = json!({
            "reason_code": "world_reaction",
            "actions": [{"type": "activate_npc", "action_id": "a1", "npc_id": "old_marlow",
                         "location_id": "lighthouse"}],
            "confidence": 0.7
        })
        .to_string();
        let provider = Arc::new(ScriptedProvider::texts([invalid, valid_output()]));
        let decision = engine(&provider).decide(&ctx).await.unwrap();
        assert!(decision.metadata.repaired);
        let issues = &provider.calls()[1].issues;
        assert_eq!(issues[0].path, "actions[0].npc_id");
        assert_eq!(issues[0].code, IssueCode::DeadCharacter);
    }

    #[tokio::test]
    async fn output_that_stays_invalid_is_rejected_without_a_third_call() {
        let ctx = sample_context();
        let script = json!({
            "reason_code": "world_reaction",
            "actions": [{"type": "execute_script", "action_id": "a1", "script": "kill_all()"}],
            "confidence": 1
        })
        .to_string();
        let provider = Arc::new(ScriptedProvider::texts([
            script.clone(),
            script,
            valid_output(), // must never be requested
        ]));
        let error = engine(&provider).decide(&ctx).await.unwrap_err();

        let DirectorError::InvalidDecision { attempts, issues } = &error else {
            panic!("expected InvalidDecision, got {error:?}");
        };
        assert_eq!(*attempts, 2);
        assert_eq!(issues[0].code, IssueCode::UnknownActionType);
        assert_eq!(provider.call_count(), 2);
        assert!(error.to_string().contains("rejected after 2 attempt(s)"));
        assert!(error.to_string().contains("actions[0]"));
    }

    #[tokio::test]
    async fn provider_errors_surface_and_are_not_retried() {
        let ctx = sample_context();
        for (kind, status, detail) in [
            (
                ProviderErrorKind::RateLimited,
                429,
                "HTTP 429: quota exceeded",
            ),
            (ProviderErrorKind::Unavailable, 503, "HTTP 503: high demand"),
        ] {
            let provider = Arc::new(ScriptedProvider::new([
                failing(kind, status, detail),
                ScriptedResponse::Text(valid_output()), // must never be requested
            ]));
            let error = engine(&provider).decide(&ctx).await.unwrap_err();
            let DirectorError::Provider(provider_error) = &error else {
                panic!("expected Provider, got {error:?}");
            };
            assert_eq!(provider_error.kind, kind);
            assert_eq!(provider_error.status, Some(status));
            assert_eq!(provider.call_count(), 1, "{kind} must not be retried");
            assert_eq!(error.to_string(), format!("gemini {kind}: {detail}"));
        }
    }

    #[tokio::test]
    async fn a_failure_during_repair_also_surfaces() {
        let ctx = sample_context();
        let provider = Arc::new(ScriptedProvider::new([
            ScriptedResponse::Text("not json".into()),
            failing(ProviderErrorKind::Unavailable, 503, "HTTP 503"),
        ]));
        let error = engine(&provider).decide(&ctx).await.unwrap_err();
        assert!(matches!(
            error,
            DirectorError::Provider(ProviderError {
                kind: ProviderErrorKind::Unavailable,
                ..
            })
        ));
        assert_eq!(provider.call_count(), 2);
    }

    #[tokio::test]
    async fn a_hung_provider_times_out() {
        let ctx = sample_context();
        let provider = Arc::new(ScriptedProvider::new([
            ScriptedResponse::Hang,
            ScriptedResponse::Text(valid_output()), // must never be requested
        ]));
        let started = Instant::now();
        let error = engine(&provider)
            .with_call_timeout(Duration::from_millis(40))
            .decide(&ctx)
            .await
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5));
        let DirectorError::Provider(provider_error) = &error else {
            panic!("expected Provider, got {error:?}");
        };
        assert_eq!(provider_error.kind, ProviderErrorKind::Timeout);
        assert_eq!(provider_error.provider, "scripted");
        assert_eq!(provider.call_count(), 1);
    }

    #[tokio::test]
    async fn an_invalid_context_never_reaches_the_provider() {
        let mut ctx = sample_context();
        ctx.schema_version = 99;
        let provider = Arc::new(ScriptedProvider::texts([valid_output()]));
        let error = engine(&provider).decide(&ctx).await.unwrap_err();
        let DirectorError::InvalidContext(issues) = &error else {
            panic!("expected InvalidContext, got {error:?}");
        };
        assert_eq!(issues[0].code, IssueCode::UnsupportedVersion);
        assert_eq!(provider.call_count(), 0);
    }

    #[tokio::test]
    async fn fallback_answers_when_the_provider_fails() {
        let ctx = sample_context();
        let provider = Arc::new(ScriptedProvider::new([failing(
            ProviderErrorKind::Unavailable,
            503,
            "HTTP 503: high demand",
        )]));
        let decision = engine(&provider)
            .with_fallback(Arc::new(FallbackDirector))
            .decide(&ctx)
            .await
            .unwrap();

        assert_eq!(decision.metadata.provider, "fallback");
        assert_eq!(decision.metadata.model, "rules-v1");
        assert_eq!(decision.metadata.attempts, 1);
        assert_eq!(
            decision.metadata.fallback_reason.as_deref(),
            Some("gemini unavailable: HTTP 503: high demand")
        );
        assert_eq!(decision.reason_code, ReasonCode::PlayerDivergence);
        assert!(
            decision
                .actions
                .iter()
                .any(|a| a.type_name() == "invalidate_mission")
        );
        assert_eq!(provider.call_count(), 1);
    }

    #[tokio::test]
    async fn fallback_answers_when_output_stays_invalid() {
        let ctx = sample_context();
        let provider = Arc::new(ScriptedProvider::texts(["nope", "still nope"]));
        let decision = engine(&provider)
            .with_fallback(Arc::new(FallbackDirector))
            .decide(&ctx)
            .await
            .unwrap();
        assert_eq!(decision.metadata.provider, "fallback");
        assert!(
            decision
                .metadata
                .fallback_reason
                .as_deref()
                .unwrap()
                .starts_with("director output rejected after 2 attempt(s)")
        );
        assert_eq!(provider.call_count(), 2);
    }

    #[tokio::test]
    async fn a_failing_fallback_reports_the_primary_error() {
        let ctx = sample_context();
        let provider = Arc::new(ScriptedProvider::new([failing(
            ProviderErrorKind::RateLimited,
            429,
            "HTTP 429",
        )]));
        let broken_fallback = Arc::new(ScriptedProvider::texts(["garbage"]));
        let error = engine(&provider)
            .with_fallback(broken_fallback.clone())
            .decide(&ctx)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            DirectorError::Provider(ProviderError {
                kind: ProviderErrorKind::RateLimited,
                ..
            })
        ));
        assert_eq!(
            broken_fallback.call_count(),
            1,
            "the fallback gets no repair attempt"
        );
    }

    #[tokio::test]
    async fn deterministic_engine_needs_no_model() {
        let ctx = sample_context();
        let engine = DirectorEngine::deterministic();
        let first = engine.decide(&ctx).await.unwrap();
        let second = engine.decide(&ctx).await.unwrap();
        assert_eq!(first.actions, second.actions);
        assert_eq!(first.reason_code, second.reason_code);
        assert_eq!(first.metadata.provider, "fallback");
        assert_eq!(first.metadata.fallback_reason, None);
        assert_eq!(first.actions.len(), 8);

        let mut quiet = sample_context();
        quiet.trigger = Trigger::Idle;
        let decision = engine.decide(&quiet).await.unwrap();
        assert!(decision.is_empty());
        assert_eq!(decision.reason_code, ReasonCode::NoChange);
    }

    #[tokio::test]
    async fn engine_can_run_in_a_spawned_task() {
        let engine = DirectorEngine::deterministic();
        let ctx = sample_context();
        let decision = tokio::spawn(async move { engine.decide(&ctx).await })
            .await
            .unwrap()
            .unwrap();
        assert!(decision.actions.iter().all(|a| !a.action_id().is_empty()));
        assert!(format!("{:?}", DirectorEngine::deterministic()).contains("fallback"));
    }
}
