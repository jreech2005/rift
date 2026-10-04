//! Gemini Director provider: one schema-constrained generation per call
//! through the Interactions API.
//!
//! Mirrors the Phase 1 Python provider (`canon/.../gemini_structured.py`):
//! same endpoint, same request shape, key in the `x-goog-api-key` header,
//! `store: false`. This module makes exactly one HTTP request per `propose`
//! and never retries; the engine owns the single repair attempt.
//!
//! The API key stays server-side. It is never logged, never part of an error
//! and never sent anywhere but the request header.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use futures_util::future::BoxFuture;
use reqwest::header::HeaderValue;
use serde_json::{Value, json};

use super::engine::DEFAULT_CALL_TIMEOUT;
use super::prompt::{SYSTEM_INSTRUCTION, build_prompt, build_repair_prompt};
use super::provider::{
    DirectorProvider, ProviderError, ProviderErrorKind, ProviderOutput, ProviderRequest,
};
use super::schema::gemini_response_schema;
use super::truncate_chars;

pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
/// Same default as the universe compiler (`GEMINI_MODEL`).
pub const DEFAULT_MODEL: &str = "gemini-3.8-flash";

const PROVIDER: &str = "gemini";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// A decision is a few hundred tokens; the rest is headroom for thinking.
const MAX_OUTPUT_TOKENS: u32 = 8_192;
const THINKING_LEVEL: &str = "low";
const PING_MAX_OUTPUT_TOKENS: u32 = 64;
const MAX_PROVIDER_MESSAGE_CHARS: usize = 300;
const REDACTED: &str = "[redacted]";

/// A secret value. Its `Debug` output is redacted and it implements neither
/// `Display` nor `Serialize`, so it cannot be logged or sent by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The secret itself. Only for building the request header.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Replace every occurrence of the secret in `text`.
    pub fn redact(&self, text: &str) -> String {
        if self.0.is_empty() {
            text.to_owned()
        } else {
            text.replace(&self.0, REDACTED)
        }
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeminiConfig {
    pub api_key: Secret,
    pub model: String,
    /// Overridable so tests can point at a local server.
    pub base_url: String,
    /// Total time allowed for one request.
    pub timeout: Duration,
}

impl GeminiConfig {
    pub fn new(api_key: Secret) -> Self {
        Self {
            api_key,
            model: DEFAULT_MODEL.to_owned(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            timeout: DEFAULT_CALL_TIMEOUT,
        }
    }

    /// Read `GEMINI_API_KEY` and `GEMINI_MODEL` from the process environment
    /// (the same variables the Python pipeline uses; `main` loads `.env`).
    pub fn from_env() -> Result<Self, ProviderError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    pub(crate) fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ProviderError> {
        // Unset and blank values both count as missing.
        let non_empty = |key: &str| {
            lookup(key)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let api_key = non_empty("GEMINI_API_KEY").ok_or_else(|| {
            ProviderError::new(
                PROVIDER,
                ProviderErrorKind::NotConfigured,
                "GEMINI_API_KEY is not set",
            )
        })?;
        let mut config = Self::new(Secret::new(api_key));
        if let Some(model) = non_empty("GEMINI_MODEL") {
            config.model = model;
        }
        Ok(config)
    }
}

#[derive(Debug, Clone)]
pub struct GeminiDirector {
    client: reqwest::Client,
    config: GeminiConfig,
}

impl GeminiDirector {
    pub const NAME: &'static str = PROVIDER;

    pub fn new(config: GeminiConfig) -> Result<Self, ProviderError> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|_| {
                ProviderError::new(
                    PROVIDER,
                    ProviderErrorKind::Network,
                    "HTTP client could not be initialised",
                )
            })?;
        Ok(Self { client, config })
    }

    pub fn from_env() -> Result<Self, ProviderError> {
        Self::new(GeminiConfig::from_env()?)
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }

    /// The same provider (key, endpoint, timeout, connection pool) asking a
    /// different model. Used for the `GEMINI_FALLBACK_MODEL` failover leg.
    pub fn with_model(&self, model: impl Into<String>) -> Self {
        let mut other = self.clone();
        other.config.model = model.into();
        other
    }

    /// Health check: one tiny generation, no schema, nothing stored. `Ok`
    /// means the model answered with HTTP 200. Never part of gameplay.
    pub async fn ping(&self) -> Result<(), ProviderError> {
        let body = json!({
            "model": self.config.model,
            "input": "Reply with the single word OK.",
            "generation_config": {
                "max_output_tokens": PING_MAX_OUTPUT_TOKENS,
                "thinking_level": THINKING_LEVEL,
            },
            "store": false,
        });
        self.post(body).await.map(|_| ())
    }

    /// The request body for one decision. Contains no secret.
    pub fn request_body(&self, request: ProviderRequest<'_>) -> Value {
        let prompt = build_prompt(request.context);
        let input = match request.repair {
            Some(repair) => build_repair_prompt(&prompt, repair.rejected_output, repair.issues),
            None => prompt,
        };
        json!({
            "model": self.config.model,
            "system_instruction": SYSTEM_INSTRUCTION,
            "input": input,
            "response_format": {
                "type": "text",
                "mime_type": "application/json",
                "schema": gemini_response_schema(request.context),
            },
            "generation_config": {
                "max_output_tokens": MAX_OUTPUT_TOKENS,
                "thinking_level": THINKING_LEVEL,
            },
            "store": false,
        })
    }

    async fn generate(&self, body: Value) -> Result<ProviderOutput, ProviderError> {
        let data = self.post(body).await?;
        parse_interaction(&data, &self.config.model)
    }

    /// One HTTP request. Returns the JSON body of a successful response.
    async fn post(&self, body: Value) -> Result<Value, ProviderError> {
        let error = |kind, detail: &str| ProviderError::new(PROVIDER, kind, detail);

        let mut key = HeaderValue::from_str(self.config.api_key.expose()).map_err(|_| {
            error(
                ProviderErrorKind::NotConfigured,
                "GEMINI_API_KEY contains characters that cannot be sent in a header",
            )
        })?;
        key.set_sensitive(true);

        let url = format!(
            "{}/interactions",
            self.config.base_url.trim_end_matches('/')
        );
        let response = self
            .client
            .post(url)
            .header("x-goog-api-key", key)
            .json(&body)
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status();
        let text = response.text().await.map_err(transport_error)?;
        let parsed: Option<Value> = serde_json::from_str(&text).ok();

        if !status.is_success() {
            let kind = match status.as_u16() {
                401 | 403 => ProviderErrorKind::Auth,
                429 => ProviderErrorKind::RateLimited,
                500.. => ProviderErrorKind::Unavailable,
                _ => ProviderErrorKind::Http,
            };
            let message = parsed.as_ref().map(provider_message).unwrap_or_default();
            let message = truncate_chars(
                &self.config.api_key.redact(&message),
                MAX_PROVIDER_MESSAGE_CHARS,
            );
            let detail = if message.is_empty() {
                format!("HTTP {}", status.as_u16())
            } else {
                format!("HTTP {}: {message}", status.as_u16())
            };
            return Err(error(kind, &detail).with_status(status.as_u16()));
        }

        parsed.ok_or_else(|| {
            error(ProviderErrorKind::Malformed, "response was not JSON")
                .with_status(status.as_u16())
        })
    }
}

impl DirectorProvider for GeminiDirector {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn propose<'a>(
        &'a self,
        request: ProviderRequest<'a>,
    ) -> BoxFuture<'a, Result<ProviderOutput, ProviderError>> {
        let body = self.request_body(request);
        Box::pin(self.generate(body))
    }
}

/// Classify a transport failure. Only a category is reported: reqwest's own
/// message can include the request URL.
fn transport_error(err: reqwest::Error) -> ProviderError {
    if err.is_timeout() {
        ProviderError::new(
            PROVIDER,
            ProviderErrorKind::Timeout,
            "no response within the timeout",
        )
    } else if err.is_connect() {
        ProviderError::new(PROVIDER, ProviderErrorKind::Network, "connection failed")
    } else {
        ProviderError::new(PROVIDER, ProviderErrorKind::Network, "request failed")
    }
}

/// Google's own error text (`error.message`), if the body has one.
fn provider_message(body: &Value) -> String {
    body.get("error")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn texts(parts: Option<&Value>) -> impl Iterator<Item = &str> {
    parts
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
}

/// Model text only; thought steps are skipped.
fn output_text(data: &Value) -> String {
    let from_steps: String = data
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|step| step.get("type").and_then(Value::as_str) == Some("model_output"))
        .flat_map(|step| texts(step.get("content")))
        .collect();
    if from_steps.is_empty() {
        // Flat `outputs` list used by earlier Interactions responses.
        texts(data.get("outputs")).collect()
    } else {
        from_steps
    }
}

/// Extract the generated text from an Interactions response.
pub fn parse_interaction(
    data: &Value,
    requested_model: &str,
) -> Result<ProviderOutput, ProviderError> {
    let error = |kind, detail: String| ProviderError::new(PROVIDER, kind, detail);
    if !data.is_object() {
        return Err(error(
            ProviderErrorKind::Malformed,
            "response is not a JSON object".to_owned(),
        ));
    }
    match data.get("status").and_then(Value::as_str) {
        None | Some("completed") => {}
        Some("incomplete") => {
            return Err(error(
                ProviderErrorKind::Incomplete,
                "generation stopped before the output ended".to_owned(),
            ));
        }
        Some(other) => {
            return Err(error(
                ProviderErrorKind::Unavailable,
                format!("interaction status {:?}", truncate_chars(other, 40)),
            ));
        }
    }
    let text = output_text(data);
    if text.trim().is_empty() {
        return Err(error(
            ProviderErrorKind::Malformed,
            "response contained no text output".to_owned(),
        ));
    }
    let usage: BTreeMap<String, u64> = data
        .get("usage")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(key, _)| key.starts_with("total_"))
        .filter_map(|(key, value)| Some((key.clone(), value.as_u64()?)))
        .collect();
    let model = data
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .unwrap_or(requested_model);
    Ok(ProviderOutput {
        text,
        model: model.to_owned(),
        usage,
        provider: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::director::provider::RepairRequest;
    use crate::director::testing::sample_context;
    use crate::director::validate::{IssueCode, ValidationIssue};

    const KEY: &str = "test-key-do-not-leak-0123456789";

    fn director() -> GeminiDirector {
        GeminiDirector::new(GeminiConfig::new(Secret::new(KEY))).unwrap()
    }

    #[test]
    fn secret_is_redacted_everywhere_it_could_be_printed() {
        let secret = Secret::new(KEY);
        assert_eq!(format!("{secret:?}"), "Secret([redacted])");
        assert_eq!(secret.expose(), KEY);
        assert_eq!(
            secret.redact(&format!("key {KEY} rejected ({KEY})")),
            "key [redacted] rejected ([redacted])"
        );
        assert_eq!(Secret::new("").redact("nothing to hide"), "nothing to hide");

        let config = GeminiConfig::new(secret);
        assert!(!format!("{config:?}").contains(KEY));
        assert!(!format!("{:?}", director()).contains(KEY));
    }

    #[test]
    fn config_reads_the_shared_environment_variables() {
        let config = GeminiConfig::from_lookup(|key| match key {
            "GEMINI_API_KEY" => Some(format!("  {KEY}  ")),
            "GEMINI_MODEL" => Some("gemini-test".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(config.api_key.expose(), KEY);
        assert_eq!(config.model, "gemini-test");
        assert_eq!(config.base_url, DEFAULT_BASE_URL);

        let config =
            GeminiConfig::from_lookup(|key| (key == "GEMINI_API_KEY").then(|| KEY.into())).unwrap();
        assert_eq!(config.model, DEFAULT_MODEL);
    }

    #[test]
    fn missing_or_blank_key_is_not_configured() {
        for value in [None, Some(String::new()), Some("   ".to_owned())] {
            let error = GeminiConfig::from_lookup(|key| {
                (key == "GEMINI_API_KEY").then(|| value.clone()).flatten()
            })
            .unwrap_err();
            assert_eq!(error.kind, ProviderErrorKind::NotConfigured);
            assert_eq!(
                error.to_string(),
                "gemini not_configured: GEMINI_API_KEY is not set"
            );
        }
    }

    #[test]
    fn request_body_is_schema_constrained_and_carries_no_secret() {
        let ctx = sample_context();
        let body = director().request_body(ProviderRequest {
            context: &ctx,
            repair: None,
        });

        assert_eq!(body["model"], DEFAULT_MODEL);
        assert_eq!(body["store"], false);
        assert_eq!(body["system_instruction"], SYSTEM_INSTRUCTION);
        assert_eq!(body["response_format"]["type"], "text");
        assert_eq!(body["response_format"]["mime_type"], "application/json");
        assert_eq!(
            body["response_format"]["schema"],
            gemini_response_schema(&ctx)
        );
        assert_eq!(body["generation_config"]["thinking_level"], "low");
        assert!(body["input"].as_str().unwrap().contains("captain_ines"));
        assert!(!body["input"].as_str().unwrap().contains("previous output"));
        assert!(!body.to_string().contains(KEY));
    }

    #[test]
    fn repair_request_body_includes_the_rejection() {
        let ctx = sample_context();
        let issues = [ValidationIssue::new(
            "actions[0]",
            IssueCode::UnknownActionType,
            "nope",
        )];
        let body = director().request_body(ProviderRequest {
            context: &ctx,
            repair: Some(RepairRequest {
                rejected_output: "{\"x\": 1}",
                issues: &issues,
            }),
        });
        let input = body["input"].as_str().unwrap();
        assert!(input.contains("Your previous output was rejected by validation."));
        assert!(input.contains("{\"x\": 1}"));
        assert!(input.contains("- actions[0]: nope"));
    }

    #[test]
    fn parses_interaction_steps_and_skips_thoughts() {
        let data = json!({
            "status": "completed",
            "model": "gemini-live",
            "steps": [
                {"type": "thought", "content": [{"type": "text", "text": "hmm"}]},
                {"type": "model_output", "content": [
                    {"type": "text", "text": "{\"a\":"},
                    {"type": "image", "data": "..."},
                    {"type": "text", "text": "1}"}
                ]}
            ],
            "usage": {"total_tokens": 120, "total_input_tokens": 100, "input_tokens": 5, "total_note": "x"}
        });
        let output = parse_interaction(&data, "requested").unwrap();
        assert_eq!(output.text, "{\"a\":1}");
        assert_eq!(output.model, "gemini-live");
        assert_eq!(
            output.usage,
            BTreeMap::from([
                ("total_input_tokens".to_owned(), 100),
                ("total_tokens".to_owned(), 120)
            ])
        );
    }

    #[test]
    fn parses_the_flat_outputs_shape_and_defaults_the_model() {
        let data = json!({"outputs": [{"type": "text", "text": "{}"}]});
        let output = parse_interaction(&data, "requested").unwrap();
        assert_eq!(output.text, "{}");
        assert_eq!(output.model, "requested");
        assert!(output.usage.is_empty());
    }

    #[test]
    fn classifies_unusable_interactions() {
        let kind = |data: Value| parse_interaction(&data, "m").unwrap_err().kind;
        assert_eq!(kind(json!([])), ProviderErrorKind::Malformed);
        assert_eq!(
            kind(json!({"status": "completed", "steps": []})),
            ProviderErrorKind::Malformed
        );
        assert_eq!(
            kind(
                json!({"steps": [{"type": "model_output", "content": [{"type": "text", "text": "  "}]}]})
            ),
            ProviderErrorKind::Malformed
        );
        assert_eq!(
            kind(json!({"status": "incomplete"})),
            ProviderErrorKind::Incomplete
        );
        assert_eq!(
            kind(json!({"status": "failed"})),
            ProviderErrorKind::Unavailable
        );
    }

    #[test]
    fn reads_the_provider_error_message() {
        assert_eq!(
            provider_message(&json!({"error": {"message": "quota"}})),
            "quota"
        );
        assert_eq!(provider_message(&json!({"error": "nope"})), "");
        assert_eq!(provider_message(&json!([])), "");
    }
}
