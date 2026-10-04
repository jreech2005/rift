//! The provider interface: anything that can propose a decision for a context.
//!
//! A provider returns raw text. It is never trusted: the engine parses and
//! validates the output of every provider, including the deterministic ones.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::Mutex;

use futures_util::future::BoxFuture;

use super::context::DirectorContext;
use super::validate::ValidationIssue;

/// Present on the second (and last) call for a decision.
#[derive(Debug, Clone, Copy)]
pub struct RepairRequest<'a> {
    /// The output that failed validation.
    pub rejected_output: &'a str,
    pub issues: &'a [ValidationIssue],
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderRequest<'a> {
    pub context: &'a DirectorContext,
    pub repair: Option<RepairRequest<'a>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderOutput {
    /// The proposal as JSON text. Untrusted.
    pub text: String,
    /// Model (or rule set) that produced it.
    pub model: String,
    pub usage: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// A required setting (the API key) is missing.
    NotConfigured,
    /// No response within the explicit timeout.
    Timeout,
    /// Connection-level failure.
    Network,
    /// Credentials rejected (401/403).
    Auth,
    /// HTTP 429.
    RateLimited,
    /// HTTP 5xx, or the provider reported a failed job.
    Unavailable,
    /// Any other non-success status.
    Http,
    /// The response was not the expected shape.
    Malformed,
    /// Generation stopped before the output ended.
    Incomplete,
}

impl ProviderErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::Timeout => "timeout",
            Self::Network => "network",
            Self::Auth => "auth",
            Self::RateLimited => "rate_limited",
            Self::Unavailable => "unavailable",
            Self::Http => "http",
            Self::Malformed => "malformed",
            Self::Incomplete => "incomplete",
        }
    }
}

impl fmt::Display for ProviderErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A provider failed. `detail` is safe to log and show: it never contains a
/// request URL, a header or a secret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{provider} {kind}: {detail}")]
pub struct ProviderError {
    pub provider: &'static str,
    pub kind: ProviderErrorKind,
    /// HTTP status, when there was one.
    pub status: Option<u16>,
    pub detail: String,
}

impl ProviderError {
    pub fn new(provider: &'static str, kind: ProviderErrorKind, detail: impl Into<String>) -> Self {
        Self {
            provider,
            kind,
            status: None,
            detail: detail.into(),
        }
    }

    pub fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }
}

pub trait DirectorProvider: Send + Sync {
    /// Short stable name recorded in decision metadata, e.g. `gemini`.
    fn name(&self) -> &'static str;

    /// Propose a decision for `request.context`. When `request.repair` is
    /// set, the previous output was rejected and this is the only retry.
    fn propose<'a>(
        &'a self,
        request: ProviderRequest<'a>,
    ) -> BoxFuture<'a, Result<ProviderOutput, ProviderError>>;
}

/// One canned provider response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptedResponse {
    Text(String),
    Error(ProviderError),
    /// Never answers; for exercising timeouts.
    Hang,
}

/// What a [`ScriptedProvider`] was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCall {
    pub rejected_output: Option<String>,
    pub issues: Vec<ValidationIssue>,
}

impl RecordedCall {
    pub fn is_repair(&self) -> bool {
        self.rejected_output.is_some()
    }
}

/// Fixture-based provider: replays canned responses in order and records every
/// call. For tests and offline development of the systems around the Director.
#[derive(Debug, Default)]
pub struct ScriptedProvider {
    responses: Mutex<VecDeque<ScriptedResponse>>,
    calls: Mutex<Vec<RecordedCall>>,
}

impl ScriptedProvider {
    pub const NAME: &'static str = "scripted";

    pub fn new(responses: impl IntoIterator<Item = ScriptedResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// A provider that answers with these texts, in order.
    pub fn texts<S: Into<String>>(texts: impl IntoIterator<Item = S>) -> Self {
        Self::new(texts.into_iter().map(|t| ScriptedResponse::Text(t.into())))
    }

    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls
            .lock()
            .expect("scripted provider lock poisoned")
            .clone()
    }

    pub fn call_count(&self) -> usize {
        self.calls
            .lock()
            .expect("scripted provider lock poisoned")
            .len()
    }
}

impl DirectorProvider for ScriptedProvider {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn propose<'a>(
        &'a self,
        request: ProviderRequest<'a>,
    ) -> BoxFuture<'a, Result<ProviderOutput, ProviderError>> {
        // Locks are released before the future is awaited.
        self.calls
            .lock()
            .expect("scripted provider lock poisoned")
            .push(RecordedCall {
                rejected_output: request.repair.map(|r| r.rejected_output.to_owned()),
                issues: request
                    .repair
                    .map(|r| r.issues.to_vec())
                    .unwrap_or_default(),
            });
        let next = self
            .responses
            .lock()
            .expect("scripted provider lock poisoned")
            .pop_front();
        Box::pin(async move {
            match next {
                Some(ScriptedResponse::Text(text)) => Ok(ProviderOutput {
                    text,
                    model: "fixture".to_owned(),
                    usage: BTreeMap::new(),
                }),
                Some(ScriptedResponse::Error(error)) => Err(error),
                Some(ScriptedResponse::Hang) => std::future::pending().await,
                None => Err(ProviderError::new(
                    Self::NAME,
                    ProviderErrorKind::Unavailable,
                    "no scripted response left",
                )),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::director::testing::sample_context;
    use crate::director::validate::IssueCode;

    #[test]
    fn error_display_is_kind_and_detail() {
        let error = ProviderError::new(
            "gemini",
            ProviderErrorKind::RateLimited,
            "HTTP 429: slow down",
        )
        .with_status(429);
        assert_eq!(
            error.to_string(),
            "gemini rate_limited: HTTP 429: slow down"
        );
        assert_eq!(error.status, Some(429));
    }

    #[tokio::test]
    async fn scripted_provider_replays_and_records() {
        let ctx = sample_context();
        let provider = ScriptedProvider::new([
            ScriptedResponse::Text("one".into()),
            ScriptedResponse::Error(ProviderError::new(
                "x",
                ProviderErrorKind::Unavailable,
                "down",
            )),
        ]);
        let first = provider
            .propose(ProviderRequest {
                context: &ctx,
                repair: None,
            })
            .await
            .unwrap();
        assert_eq!(first.text, "one");

        let issues = [ValidationIssue::new("$", IssueCode::Malformed, "bad")];
        let second = provider
            .propose(ProviderRequest {
                context: &ctx,
                repair: Some(RepairRequest {
                    rejected_output: "one",
                    issues: &issues,
                }),
            })
            .await
            .unwrap_err();
        assert_eq!(second.kind, ProviderErrorKind::Unavailable);

        let exhausted = provider
            .propose(ProviderRequest {
                context: &ctx,
                repair: None,
            })
            .await
            .unwrap_err();
        assert_eq!(exhausted.detail, "no scripted response left");

        let calls = provider.calls();
        assert_eq!(provider.call_count(), 3);
        assert!(!calls[0].is_repair());
        assert!(calls[1].is_repair());
        assert_eq!(calls[1].rejected_output.as_deref(), Some("one"));
        assert_eq!(calls[1].issues, issues);
    }
}
