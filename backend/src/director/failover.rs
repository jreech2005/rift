//! Bounded transport failover across LLM providers.
//!
//! ```text
//! primary model -- transient failure --> short retry (bounded)
//!       | still failing, or a failure that will not pass
//!       v
//! next leg (fallback model, then the secondary provider), one attempt each
//!       | every leg failed, or the time budget ran out
//!       v
//! error -> DirectorEngine -> FallbackDirector (deterministic)
//! ```
//!
//! [`FailoverProvider`] is itself a [`DirectorProvider`], so the engine is
//! unchanged: it still makes one call plus at most one repair call, and each
//! of those is one pass down this chain. Retrying here is about transport
//! (timeout, 429, 5xx, connection); an answer that arrives but fails
//! validation is never retried here, only repaired once by the engine.
//!
//! Every pass is bounded: a fixed list of legs visited once in order, a capped
//! retry count per leg, a per-call timeout, a capped backoff and a total time
//! budget. There is no loop without a bound and no recursion.

use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use tokio::time::Instant;
use tracing::{info, warn};

use super::anthropic::{self, AnthropicConfig, AnthropicDirector};
use super::engine::DirectorEngine;
use super::fallback::FallbackDirector;
use super::gemini::{GeminiConfig, GeminiDirector};
use super::provider::{
    DirectorProvider, ProviderError, ProviderErrorKind, ProviderOutput, ProviderRequest,
};

/// Hard cap on transient retries per leg, whatever the configuration says.
pub const MAX_TRANSIENT_RETRIES: u8 = 2;
pub const DEFAULT_TRANSIENT_RETRIES: u8 = 1;
/// Per-request timeout for Gemini (`GEMINI_TIMEOUT_MS`).
pub const DEFAULT_GEMINI_TIMEOUT: Duration = Duration::from_secs(8);
/// Wait before retry `n` is `n * DEFAULT_BACKOFF`, capped at [`MAX_BACKOFF`].
pub const DEFAULT_BACKOFF: Duration = Duration::from_millis(250);
pub const MAX_BACKOFF: Duration = Duration::from_secs(1);
/// Total time for one pass down the chain (`DIRECTOR_BUDGET_MS`).
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(20);

const MIN_TIMEOUT_MS: u64 = 1_000;
const MAX_TIMEOUT_MS: u64 = 30_000;
const MAX_BUDGET_MS: u64 = 60_000;

/// One provider in the chain.
#[derive(Clone)]
pub struct Leg {
    provider: Arc<dyn DirectorProvider>,
    label: String,
    retries: u8,
    timeout: Duration,
}

impl Leg {
    /// A leg that is tried once. `label` is for logs, e.g. `gemini:model-id`.
    pub fn new(
        provider: Arc<dyn DirectorProvider>,
        label: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            provider,
            label: label.into(),
            retries: 0,
            timeout,
        }
    }

    /// Retry transient failures up to `retries` times (at most
    /// [`MAX_TRANSIENT_RETRIES`]).
    pub fn with_retries(mut self, retries: u8) -> Self {
        self.retries = retries.min(MAX_TRANSIENT_RETRIES);
        self
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailoverPolicy {
    /// Wait before the first retry; retry `n` waits `n` times this.
    pub backoff: Duration,
    pub max_backoff: Duration,
    /// Total time for one `propose`, across every leg, retry and backoff.
    pub budget: Duration,
}

impl Default for FailoverPolicy {
    fn default() -> Self {
        Self {
            backoff: DEFAULT_BACKOFF,
            max_backoff: MAX_BACKOFF,
            budget: DEFAULT_BUDGET,
        }
    }
}

#[derive(Clone)]
pub struct FailoverProvider {
    legs: Vec<Leg>,
    policy: FailoverPolicy,
}

impl std::fmt::Debug for FailoverProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FailoverProvider")
            .field("legs", &self.labels())
            .field("policy", &self.policy)
            .finish()
    }
}

impl FailoverProvider {
    pub const NAME: &'static str = "failover";

    pub fn new(legs: Vec<Leg>) -> Self {
        Self {
            legs,
            policy: FailoverPolicy::default(),
        }
    }

    pub fn with_policy(mut self, policy: FailoverPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn labels(&self) -> Vec<&str> {
        self.legs.iter().map(Leg::label).collect()
    }

    /// The most requests one `propose` can make.
    pub fn max_requests(&self) -> usize {
        self.legs
            .iter()
            .map(|leg| 1 + usize::from(leg.retries))
            .sum()
    }

    async fn run(&self, request: ProviderRequest<'_>) -> Result<ProviderOutput, ProviderError> {
        let deadline = Instant::now() + self.policy.budget;
        let remaining = || deadline.saturating_duration_since(Instant::now());
        let mut last: Option<ProviderError> = None;

        'legs: for leg in &self.legs {
            for attempt in 0..=leg.retries {
                if attempt > 0 {
                    let backoff = (self.policy.backoff * u32::from(attempt))
                        .min(self.policy.max_backoff)
                        .min(remaining());
                    tokio::time::sleep(backoff).await;
                }
                let timeout = leg.timeout.min(remaining());
                if timeout.is_zero() {
                    warn!(leg = %leg.label, "director failover budget exhausted");
                    break 'legs;
                }
                let error = match tokio::time::timeout(timeout, leg.provider.propose(request)).await
                {
                    Ok(Ok(mut output)) => {
                        output.provider.get_or_insert(leg.provider.name());
                        return Ok(output);
                    }
                    Ok(Err(error)) => error,
                    Err(_) => ProviderError::new(
                        leg.provider.name(),
                        ProviderErrorKind::Timeout,
                        format!("no response within {} ms", timeout.as_millis()),
                    ),
                };
                let retry = error.kind.is_transient() && attempt < leg.retries;
                warn!(leg = %leg.label, attempt = attempt + 1, error = %error, retry, "director provider call failed");
                last = Some(error);
                if !retry {
                    break;
                }
            }
        }

        Err(last.unwrap_or_else(|| {
            ProviderError::new(
                Self::NAME,
                ProviderErrorKind::NotConfigured,
                "no provider could be called",
            )
        }))
    }
}

impl DirectorProvider for FailoverProvider {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn propose<'a>(
        &'a self,
        request: ProviderRequest<'a>,
    ) -> BoxFuture<'a, Result<ProviderOutput, ProviderError>> {
        Box::pin(self.run(request))
    }
}

/// A configured LLM, before it becomes a leg. Also what the preflight pings.
#[derive(Debug, Clone)]
pub enum Llm {
    Gemini(GeminiDirector),
    Anthropic(AnthropicDirector),
}

impl Llm {
    /// `provider:model`. Never contains a secret.
    pub fn label(&self) -> String {
        match self {
            Self::Gemini(p) => format!("{}:{}", GeminiDirector::NAME, p.model()),
            Self::Anthropic(p) => format!("{}:{}", AnthropicDirector::NAME, p.model()),
        }
    }

    /// One tiny request to see whether the model answers. Not for gameplay.
    pub async fn ping(&self) -> Result<(), ProviderError> {
        match self {
            Self::Gemini(p) => p.ping().await,
            Self::Anthropic(p) => p.ping().await,
        }
    }

    fn into_provider(self) -> Arc<dyn DirectorProvider> {
        match self {
            Self::Gemini(p) => Arc::new(p),
            Self::Anthropic(p) => Arc::new(p),
        }
    }
}

/// The Director settings read from the environment. Building it does no I/O.
#[derive(Debug, Clone)]
pub struct DirectorSettings {
    /// LLMs in failover order with their per-request timeout.
    pub llms: Vec<(Llm, Duration)>,
    /// Transient retries for the first LLM. The others are tried once.
    pub transient_retries: u8,
    pub budget: Duration,
}

impl DirectorSettings {
    /// Read the process environment (`main` loads `.env` first).
    pub fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Never fails: a provider that is not configured is left out, and a bad
    /// value falls back to its default with a warning.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let value = |key: &str| {
            lookup(key)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let millis = |key: &str, default: Duration, max_ms: u64| match value(key) {
            None => default,
            Some(raw) => match raw.parse::<u64>() {
                Ok(ms) => Duration::from_millis(ms.clamp(MIN_TIMEOUT_MS, max_ms)),
                Err(_) => {
                    warn!(key, "not a number of milliseconds; using the default");
                    default
                }
            },
        };

        let primary = value("DIRECTOR_PRIMARY_PROVIDER")
            .map_or_else(|| GeminiDirector::NAME.to_owned(), |v| v.to_lowercase());
        let secondary = value("DIRECTOR_SECONDARY_PROVIDER")
            .map(|v| v.to_lowercase())
            .filter(|v| v != "none" && *v != primary);

        let mut llms = Vec::new();
        for name in std::iter::once(primary).chain(secondary) {
            match name.as_str() {
                GeminiDirector::NAME => {
                    let timeout =
                        millis("GEMINI_TIMEOUT_MS", DEFAULT_GEMINI_TIMEOUT, MAX_TIMEOUT_MS);
                    let built = GeminiConfig::from_lookup(&lookup).and_then(|mut config| {
                        config.timeout = timeout;
                        GeminiDirector::new(config)
                    });
                    match built {
                        Ok(gemini) => {
                            let other = value("GEMINI_FALLBACK_MODEL")
                                .filter(|model| model != gemini.model())
                                .map(|model| gemini.with_model(model));
                            llms.push((Llm::Gemini(gemini), timeout));
                            llms.extend(other.map(|g| (Llm::Gemini(g), timeout)));
                        }
                        Err(error) => skipped(&error),
                    }
                }
                AnthropicDirector::NAME => {
                    let timeout = millis(
                        "ANTHROPIC_TIMEOUT_MS",
                        anthropic::DEFAULT_TIMEOUT,
                        MAX_TIMEOUT_MS,
                    );
                    let built = AnthropicConfig::from_lookup(&lookup).and_then(|mut config| {
                        config.timeout = timeout;
                        AnthropicDirector::new(config)
                    });
                    match built {
                        Ok(anthropic) => llms.push((Llm::Anthropic(anthropic), timeout)),
                        Err(error) => skipped(&error),
                    }
                }
                other => warn!(provider = other, "unknown director provider; ignored"),
            }
        }

        let transient_retries = match value("GEMINI_TRANSIENT_RETRIES") {
            None => DEFAULT_TRANSIENT_RETRIES,
            Some(raw) => match raw.parse::<u8>() {
                Ok(retries) => retries.min(MAX_TRANSIENT_RETRIES),
                Err(_) => {
                    warn!("GEMINI_TRANSIENT_RETRIES is not a small number; using the default");
                    DEFAULT_TRANSIENT_RETRIES
                }
            },
        };
        Self {
            llms,
            transient_retries,
            budget: millis("DIRECTOR_BUDGET_MS", DEFAULT_BUDGET, MAX_BUDGET_MS),
        }
    }

    /// `provider:model` of every configured LLM, in failover order.
    pub fn labels(&self) -> Vec<String> {
        self.llms.iter().map(|(llm, _)| llm.label()).collect()
    }

    /// The LLM chain with the deterministic rules behind it, or the
    /// deterministic rules alone when no LLM is configured.
    pub fn into_engine(self) -> DirectorEngine {
        if self.llms.is_empty() {
            return DirectorEngine::deterministic();
        }
        let retries = self.transient_retries;
        let legs = self
            .llms
            .into_iter()
            .enumerate()
            .map(|(index, (llm, timeout))| {
                let leg = Leg::new(llm.clone().into_provider(), llm.label(), timeout);
                leg.with_retries(if index == 0 { retries } else { 0 })
            })
            .collect();
        let chain = FailoverProvider::new(legs).with_policy(FailoverPolicy {
            budget: self.budget,
            ..FailoverPolicy::default()
        });
        DirectorEngine::new(Arc::new(chain))
            .with_fallback(Arc::new(FallbackDirector))
            .with_call_timeout(self.budget)
    }
}

fn skipped(error: &ProviderError) {
    if error.kind == ProviderErrorKind::NotConfigured {
        info!(reason = %error, "director provider not configured; skipped");
    } else {
        warn!(error = %error, "director provider unavailable; skipped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::director::engine::DirectorError;
    use crate::director::provider::{ScriptedProvider, ScriptedResponse};
    use crate::director::testing::{sample_context, sample_proposal};

    const TIMEOUT: Duration = Duration::from_millis(60);

    /// A scripted provider under a provider name, as a leg sees it.
    struct Named(&'static str, Arc<ScriptedProvider>);

    impl DirectorProvider for Named {
        fn name(&self) -> &'static str {
            self.0
        }

        fn propose<'a>(
            &'a self,
            request: ProviderRequest<'a>,
        ) -> BoxFuture<'a, Result<ProviderOutput, ProviderError>> {
            self.1.propose(request)
        }
    }

    fn valid() -> ScriptedResponse {
        ScriptedResponse::Text(serde_json::to_string(&sample_proposal()).unwrap())
    }

    fn failing(kind: ProviderErrorKind, status: u16) -> ScriptedResponse {
        ScriptedResponse::Error(ProviderError::new("gemini", kind, "down").with_status(status))
    }

    fn unavailable() -> ScriptedResponse {
        failing(ProviderErrorKind::Unavailable, 503)
    }

    fn scripted(responses: impl IntoIterator<Item = ScriptedResponse>) -> Arc<ScriptedProvider> {
        Arc::new(ScriptedProvider::new(responses))
    }

    fn policy() -> FailoverPolicy {
        FailoverPolicy {
            backoff: Duration::from_millis(20),
            max_backoff: Duration::from_millis(40),
            budget: Duration::from_secs(2),
        }
    }

    /// primary (one retry) -> secondary model -> other provider, then the
    /// deterministic rules, wired the way `into_engine` wires them.
    fn engine(
        primary: &Arc<ScriptedProvider>,
        secondary: &Arc<ScriptedProvider>,
        other: &Arc<ScriptedProvider>,
        policy: FailoverPolicy,
    ) -> DirectorEngine {
        let leg = |name, provider: &Arc<ScriptedProvider>| {
            Leg::new(Arc::new(Named(name, provider.clone())), name, TIMEOUT)
        };
        let chain = FailoverProvider::new(vec![
            leg("gemini", primary).with_retries(1),
            leg("gemini", secondary),
            leg("anthropic", other),
        ])
        .with_policy(policy);
        assert_eq!(chain.max_requests(), 4);
        DirectorEngine::new(Arc::new(chain))
            .with_fallback(Arc::new(FallbackDirector))
            .with_call_timeout(policy.budget)
    }

    #[tokio::test]
    async fn a_healthy_primary_is_the_only_provider_called() {
        let (primary, secondary, other) = (scripted([valid()]), scripted([]), scripted([]));
        let decision = engine(&primary, &secondary, &other, policy())
            .decide(&sample_context())
            .await
            .unwrap();

        assert_eq!(decision.metadata.provider, "gemini");
        assert_eq!(decision.metadata.fallback_reason, None);
        assert_eq!(primary.call_count(), 1);
        assert_eq!(secondary.call_count(), 0);
        assert_eq!(other.call_count(), 0);
    }

    #[tokio::test]
    async fn a_503_is_retried_once_after_a_bounded_backoff() {
        let (primary, secondary, other) = (
            scripted([unavailable(), valid()]),
            scripted([]),
            scripted([]),
        );
        let started = Instant::now();
        let decision = engine(&primary, &secondary, &other, policy())
            .decide(&sample_context())
            .await
            .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(decision.metadata.provider, "gemini");
        assert_eq!(decision.metadata.attempts, 1, "a retry is not a repair");
        assert!(!decision.metadata.repaired);
        assert_eq!(primary.call_count(), 2);
        assert!(!primary.calls()[1].is_repair());
        assert_eq!(secondary.call_count(), 0);
        assert!(elapsed >= policy().backoff, "waited {elapsed:?}");
        assert!(elapsed < policy().budget, "waited {elapsed:?}");
    }

    #[tokio::test]
    async fn an_exhausted_primary_fails_over_to_the_secondary_model() {
        let (primary, secondary, other) = (
            scripted([unavailable(), unavailable(), valid()]),
            scripted([valid()]),
            scripted([]),
        );
        let decision = engine(&primary, &secondary, &other, policy())
            .decide(&sample_context())
            .await
            .unwrap();

        assert_eq!(decision.metadata.provider, "gemini");
        assert_eq!(decision.metadata.fallback_reason, None);
        assert_eq!(primary.call_count(), 2, "one call and one retry, no more");
        assert_eq!(secondary.call_count(), 1);
        assert_eq!(other.call_count(), 0);
    }

    #[tokio::test]
    async fn the_independent_provider_answers_when_every_gemini_model_is_down() {
        let (primary, secondary, other) = (
            scripted([unavailable(), unavailable()]),
            scripted([unavailable()]),
            scripted([valid()]),
        );
        let decision = engine(&primary, &secondary, &other, policy())
            .decide(&sample_context())
            .await
            .unwrap();

        assert_eq!(decision.metadata.provider, "anthropic");
        assert_eq!(
            (
                primary.call_count(),
                secondary.call_count(),
                other.call_count()
            ),
            (2, 1, 1)
        );
    }

    #[tokio::test]
    async fn the_deterministic_rules_answer_when_every_llm_fails() {
        // More 503s than the chain may consume: the surplus must stay unused.
        let (primary, secondary, other) = (
            scripted(vec![unavailable(); 6]),
            scripted(vec![unavailable(); 6]),
            scripted(vec![unavailable(); 6]),
        );
        let decision = engine(&primary, &secondary, &other, policy())
            .decide(&sample_context())
            .await
            .unwrap();

        assert_eq!(decision.metadata.provider, FallbackDirector::NAME);
        assert_eq!(
            decision.metadata.fallback_reason.as_deref(),
            Some("gemini unavailable: down")
        );
        assert_eq!(
            (
                primary.call_count(),
                secondary.call_count(),
                other.call_count()
            ),
            (2, 1, 1),
            "a provider failure is not repaired, so the chain runs once"
        );
    }

    #[tokio::test]
    async fn a_429_is_retried_once_then_fails_over() {
        let rate_limited = || failing(ProviderErrorKind::RateLimited, 429);
        let (primary, secondary, other) = (
            scripted([rate_limited(), rate_limited()]),
            scripted([valid()]),
            scripted([]),
        );
        engine(&primary, &secondary, &other, policy())
            .decide(&sample_context())
            .await
            .unwrap();

        assert_eq!(primary.call_count(), 2);
        assert_eq!(secondary.call_count(), 1);
    }

    #[tokio::test]
    async fn a_failure_that_will_not_pass_is_not_retried() {
        for kind in [
            ProviderErrorKind::Auth,
            ProviderErrorKind::Http,
            ProviderErrorKind::Malformed,
            ProviderErrorKind::Incomplete,
            ProviderErrorKind::NotConfigured,
        ] {
            assert!(!kind.is_transient());
            let (primary, secondary, other) = (
                scripted([failing(kind, 400), valid()]),
                scripted([valid()]),
                scripted([]),
            );
            engine(&primary, &secondary, &other, policy())
                .decide(&sample_context())
                .await
                .unwrap();
            assert_eq!(primary.call_count(), 1, "{kind}");
            assert_eq!(secondary.call_count(), 1, "{kind}");
        }
    }

    #[tokio::test]
    async fn a_timeout_is_transient_and_bounded_per_call() {
        let (primary, secondary, other) = (
            scripted([ScriptedResponse::Hang, ScriptedResponse::Hang]),
            scripted([valid()]),
            scripted([]),
        );
        let started = Instant::now();
        let decision = engine(&primary, &secondary, &other, policy())
            .decide(&sample_context())
            .await
            .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(decision.metadata.provider, "gemini");
        assert_eq!(primary.call_count(), 2);
        assert_eq!(secondary.call_count(), 1);
        assert!(elapsed >= TIMEOUT * 2, "waited {elapsed:?}");
        assert!(elapsed < policy().budget, "waited {elapsed:?}");
    }

    #[tokio::test]
    async fn the_budget_stops_the_chain_and_the_rules_still_answer() {
        let hang = || scripted([ScriptedResponse::Hang, ScriptedResponse::Hang]);
        let (primary, secondary, other) = (hang(), hang(), hang());
        let tight = FailoverPolicy {
            budget: Duration::from_millis(90),
            ..policy()
        };
        let started = Instant::now();
        let decision = engine(&primary, &secondary, &other, tight)
            .decide(&sample_context())
            .await
            .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(decision.metadata.provider, FallbackDirector::NAME);
        assert!(
            decision
                .metadata
                .fallback_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("timeout")),
            "{:?}",
            decision.metadata.fallback_reason
        );
        assert_eq!(other.call_count(), 0, "no time left for the last leg");
        assert!(elapsed < Duration::from_millis(600), "waited {elapsed:?}");
    }

    #[tokio::test]
    async fn an_invalid_decision_gets_exactly_one_repair_and_no_transport_retry() {
        let garbage = || ScriptedResponse::Text("not json".into());
        // Repaired on the second pass.
        let (primary, secondary, other) =
            (scripted([garbage(), valid()]), scripted([]), scripted([]));
        let decision = engine(&primary, &secondary, &other, policy())
            .decide(&sample_context())
            .await
            .unwrap();
        assert_eq!(decision.metadata.attempts, 2);
        assert!(decision.metadata.repaired);
        assert_eq!(primary.call_count(), 2);
        assert!(primary.calls()[1].is_repair());
        assert_eq!(
            secondary.call_count(),
            0,
            "invalid output is not a failover"
        );

        // Still invalid after the repair: the rules answer, nothing loops.
        let (primary, secondary, other) = (
            scripted([garbage(), garbage(), valid()]),
            scripted([valid()]),
            scripted([valid()]),
        );
        let decision = engine(&primary, &secondary, &other, policy())
            .decide(&sample_context())
            .await
            .unwrap();
        assert_eq!(decision.metadata.provider, FallbackDirector::NAME);
        assert_eq!(primary.call_count(), 2);
        assert_eq!(secondary.call_count(), 0);
        assert_eq!(other.call_count(), 0);
    }

    #[tokio::test]
    async fn the_repair_pass_may_fail_over_but_stays_a_single_pass() {
        // Pass 1: primary answers garbage. Pass 2 (repair): primary is down
        // twice, the secondary model repairs it.
        let (primary, secondary, other) = (
            scripted([
                ScriptedResponse::Text("not json".into()),
                unavailable(),
                unavailable(),
            ]),
            scripted([valid()]),
            scripted([]),
        );
        let decision = engine(&primary, &secondary, &other, policy())
            .decide(&sample_context())
            .await
            .unwrap();

        assert!(decision.metadata.repaired);
        assert_eq!(primary.call_count(), 3);
        assert_eq!(secondary.call_count(), 1);
        assert!(secondary.calls()[0].is_repair());
    }

    #[tokio::test]
    async fn without_a_fallback_the_last_provider_error_surfaces() {
        let chain = FailoverProvider::new(vec![
            Leg::new(scripted([unavailable()]), "a", TIMEOUT),
            Leg::new(
                scripted([failing(ProviderErrorKind::RateLimited, 429)]),
                "b",
                TIMEOUT,
            ),
        ])
        .with_policy(policy());
        let error = DirectorEngine::new(Arc::new(chain))
            .decide(&sample_context())
            .await
            .unwrap_err();
        let DirectorError::Provider(error) = error else {
            panic!("expected a provider error, got {error:?}");
        };
        assert_eq!(error.kind, ProviderErrorKind::RateLimited);

        let empty = FailoverProvider::new(Vec::new());
        let error = DirectorEngine::new(Arc::new(empty))
            .decide(&sample_context())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no provider could be called"));
    }

    #[test]
    fn retries_are_capped_whatever_is_asked() {
        let leg = Leg::new(scripted([]), "a", TIMEOUT).with_retries(200);
        assert_eq!(leg.retries, MAX_TRANSIENT_RETRIES);
    }

    const KEY: &str = "test-key-do-not-leak-0123456789";

    fn settings(vars: &[(&str, &str)]) -> DirectorSettings {
        DirectorSettings::from_lookup(|key| {
            vars.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    #[test]
    fn no_key_means_the_deterministic_rules_alone() {
        let settings = settings(&[("DIRECTOR_SECONDARY_PROVIDER", "anthropic")]);
        assert!(settings.labels().is_empty());
        let engine = format!("{:?}", settings.into_engine());
        assert!(engine.contains("fallback"), "{engine}");
        assert!(!engine.contains("failover"), "{engine}");
    }

    #[test]
    fn gemini_alone_keeps_the_existing_defaults() {
        let settings = settings(&[("GEMINI_API_KEY", KEY)]);
        assert_eq!(settings.labels(), ["gemini:gemini-3.8-flash"]);
        assert_eq!(settings.transient_retries, DEFAULT_TRANSIENT_RETRIES);
        assert_eq!(settings.budget, DEFAULT_BUDGET);
        assert_eq!(settings.llms[0].1, DEFAULT_GEMINI_TIMEOUT);
    }

    #[test]
    fn the_full_chain_is_built_in_order_without_leaking_keys() {
        let settings = settings(&[
            ("GEMINI_API_KEY", KEY),
            ("GEMINI_MODEL", "gemini-a"),
            ("GEMINI_FALLBACK_MODEL", "gemini-b"),
            ("GEMINI_TIMEOUT_MS", "4000"),
            ("GEMINI_TRANSIENT_RETRIES", "9"),
            ("DIRECTOR_SECONDARY_PROVIDER", "Anthropic"),
            ("ANTHROPIC_API_KEY", KEY),
            ("ANTHROPIC_MODEL", "claude-x"),
            ("DIRECTOR_BUDGET_MS", "12000"),
        ]);
        assert_eq!(
            settings.labels(),
            ["gemini:gemini-a", "gemini:gemini-b", "anthropic:claude-x"]
        );
        assert_eq!(settings.transient_retries, MAX_TRANSIENT_RETRIES);
        assert_eq!(settings.budget, Duration::from_millis(12_000));
        assert_eq!(settings.llms[1].1, Duration::from_millis(4_000));
        assert_eq!(settings.llms[2].1, anthropic::DEFAULT_TIMEOUT);
        assert!(!format!("{settings:?}").contains(KEY));

        let engine = format!("{:?}", settings.into_engine());
        assert!(engine.contains("failover"), "{engine}");
        assert!(engine.contains("fallback"), "{engine}");
        assert!(!engine.contains(KEY));
    }

    #[test]
    fn providers_can_be_swapped_and_bad_values_fall_back_to_defaults() {
        let swapped = settings(&[
            ("DIRECTOR_PRIMARY_PROVIDER", "anthropic"),
            ("DIRECTOR_SECONDARY_PROVIDER", "gemini"),
            ("GEMINI_API_KEY", KEY),
            ("ANTHROPIC_API_KEY", KEY),
            ("GEMINI_FALLBACK_MODEL", "gemini-3.8-flash"),
            ("GEMINI_TIMEOUT_MS", "soon"),
            ("GEMINI_TRANSIENT_RETRIES", "-1"),
            ("DIRECTOR_BUDGET_MS", "5"),
        ]);
        assert_eq!(
            swapped.labels(),
            ["anthropic:claude-opus-5-5", "gemini:gemini-3.8-flash"],
            "a fallback model equal to the primary adds no leg"
        );
        assert_eq!(swapped.llms[1].1, DEFAULT_GEMINI_TIMEOUT);
        assert_eq!(swapped.transient_retries, DEFAULT_TRANSIENT_RETRIES);
        assert_eq!(swapped.budget, Duration::from_millis(MIN_TIMEOUT_MS));

        let unknown = settings(&[
            ("DIRECTOR_PRIMARY_PROVIDER", "skynet"),
            ("DIRECTOR_SECONDARY_PROVIDER", "gemini"),
            ("GEMINI_API_KEY", KEY),
        ]);
        assert_eq!(unknown.labels(), ["gemini:gemini-3.8-flash"]);

        let same = settings(&[
            ("DIRECTOR_SECONDARY_PROVIDER", "gemini"),
            ("GEMINI_API_KEY", KEY),
        ]);
        assert_eq!(same.labels().len(), 1);
    }
}
