//! Anthropic Director provider: one schema-constrained generation per call
//! through the Messages API.
//!
//! An independent second LLM behind Gemini (see `failover.rs`). Same contract
//! as [`GeminiDirector`](super::gemini::GeminiDirector): exactly one HTTP
//! request per `propose`, no retry, raw text out. The prompt and the response
//! schema are the ones Gemini gets.
//!
//! The API key stays server-side. It is never logged, never part of an error
//! and never sent anywhere but the request header.

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::future::BoxFuture;
use reqwest::header::HeaderValue;
use serde_json::{Value, json};

use super::gemini::Secret;
use super::prompt::{SYSTEM_INSTRUCTION, build_prompt, build_repair_prompt};
use super::provider::{
    DirectorProvider, ProviderError, ProviderErrorKind, ProviderOutput, ProviderRequest,
};
use super::schema::gemini_response_schema;
use super::truncate_chars;

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
/// Low effort keeps an interactive decision fast. Empty `ANTHROPIC_EFFORT`
/// omits the field, for models that do not accept it.
pub const DEFAULT_EFFORT: &str = "low";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

const PROVIDER: &str = "anthropic";
const API_VERSION: &str = "2023-06-01";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// A decision is a few hundred tokens; the rest is headroom for thinking.
const MAX_TOKENS: u32 = 8_192;
const PING_MAX_TOKENS: u32 = 16;
const MAX_PROVIDER_MESSAGE_CHARS: usize = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicConfig {
    pub api_key: Secret,
    pub model: String,
    /// `output_config.effort`; `None` leaves it out of the request.
    pub effort: Option<String>,
    /// Overridable so tests can point at a local server.
    pub base_url: String,
    /// Total time allowed for one request.
    pub timeout: Duration,
}

impl AnthropicConfig {
    pub fn new(api_key: Secret) -> Self {
        Self {
            api_key,
            model: DEFAULT_MODEL.to_owned(),
            effort: Some(DEFAULT_EFFORT.to_owned()),
            base_url: DEFAULT_BASE_URL.to_owned(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Read `ANTHROPIC_API_KEY`, `ANTHROPIC_MODEL` and `ANTHROPIC_EFFORT`
    /// from the process environment (`main` loads `.env`).
    pub fn from_env() -> Result<Self, ProviderError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    pub(crate) fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ProviderError> {
        let trimmed = |key: &str| lookup(key).map(|v| v.trim().to_owned());
        // Unset and blank values both count as missing.
        let api_key = trimmed("ANTHROPIC_API_KEY")
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                ProviderError::new(
                    PROVIDER,
                    ProviderErrorKind::NotConfigured,
                    "ANTHROPIC_API_KEY is not set",
                )
            })?;
        let mut config = Self::new(Secret::new(api_key));
        if let Some(model) = trimmed("ANTHROPIC_MODEL").filter(|v| !v.is_empty()) {
            config.model = model;
        }
        // Set but blank means "send no effort".
        if let Some(effort) = trimmed("ANTHROPIC_EFFORT") {
            config.effort = Some(effort).filter(|v| !v.is_empty());
        }
        Ok(config)
    }
}

#[derive(Debug, Clone)]
pub struct AnthropicDirector {
    client: reqwest::Client,
    config: AnthropicConfig,
}

impl AnthropicDirector {
    pub const NAME: &'static str = PROVIDER;

    pub fn new(config: AnthropicConfig) -> Result<Self, ProviderError> {
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
        Self::new(AnthropicConfig::from_env()?)
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }

    /// The request body for one decision. Contains no secret.
    pub fn request_body(&self, request: ProviderRequest<'_>) -> Value {
        let prompt = build_prompt(request.context);
        let input = match request.repair {
            Some(repair) => build_repair_prompt(&prompt, repair.rejected_output, repair.issues),
            None => prompt,
        };
        let mut output_config = json!({
            "format": {
                "type": "json_schema",
                "schema": gemini_response_schema(request.context),
            },
        });
        if let Some(effort) = &self.config.effort {
            output_config["effort"] = json!(effort);
        }
        json!({
            "model": self.config.model,
            "max_tokens": MAX_TOKENS,
            "system": SYSTEM_INSTRUCTION,
            "messages": [{"role": "user", "content": input}],
            "output_config": output_config,
        })
    }

    /// Health check: one tiny generation, no schema. `Ok` means the model
    /// answered with HTTP 200. Never part of gameplay.
    pub async fn ping(&self) -> Result<(), ProviderError> {
        let body = json!({
            "model": self.config.model,
            "max_tokens": PING_MAX_TOKENS,
            "messages": [{"role": "user", "content": "Reply with the single word OK."}],
        });
        self.post(body).await.map(|_| ())
    }

    async fn generate(&self, body: Value) -> Result<ProviderOutput, ProviderError> {
        let data = self.post(body).await?;
        parse_message(&data, &self.config.model)
    }

    /// One HTTP request. Returns the JSON body of a successful response.
    async fn post(&self, body: Value) -> Result<Value, ProviderError> {
        let error = |kind, detail: &str| ProviderError::new(PROVIDER, kind, detail);

        let mut key = HeaderValue::from_str(self.config.api_key.expose()).map_err(|_| {
            error(
                ProviderErrorKind::NotConfigured,
                "ANTHROPIC_API_KEY contains characters that cannot be sent in a header",
            )
        })?;
        key.set_sensitive(true);

        let url = format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'));
        let response = self
            .client
            .post(url)
            .header("x-api-key", key)
            .header("anthropic-version", API_VERSION)
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
                408 => ProviderErrorKind::Timeout,
                429 => ProviderErrorKind::RateLimited,
                // Includes 529 `overloaded_error`.
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

impl DirectorProvider for AnthropicDirector {
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

/// Anthropic's own error text (`error.message`), if the body has one.
fn provider_message(body: &Value) -> String {
    body.get("error")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Extract the generated text from a Messages response. Thinking blocks are
/// skipped.
pub fn parse_message(data: &Value, requested_model: &str) -> Result<ProviderOutput, ProviderError> {
    let error = |kind, detail: &str| ProviderError::new(PROVIDER, kind, detail);
    if !data.is_object() {
        return Err(error(
            ProviderErrorKind::Malformed,
            "response is not a JSON object",
        ));
    }
    match data.get("stop_reason").and_then(Value::as_str) {
        Some("refusal") => {
            return Err(error(
                ProviderErrorKind::Incomplete,
                "the model declined the request",
            ));
        }
        Some("max_tokens") => {
            return Err(error(
                ProviderErrorKind::Incomplete,
                "generation stopped before the output ended",
            ));
        }
        _ => {}
    }
    let text: String = data
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect();
    if text.trim().is_empty() {
        return Err(error(
            ProviderErrorKind::Malformed,
            "response contained no text output",
        ));
    }
    let usage: BTreeMap<String, u64> = data
        .get("usage")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(key, _)| key.ends_with("_tokens"))
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
    use crate::director::testing::sample_context;

    const KEY: &str = "test-key-do-not-leak-0123456789";

    fn lookup<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            vars.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn config_reads_key_model_and_effort() {
        let config = AnthropicConfig::from_lookup(lookup(&[
            ("ANTHROPIC_API_KEY", "  test-key-do-not-leak-0123456789  "),
            ("ANTHROPIC_MODEL", "claude-test"),
        ]))
        .unwrap();
        assert_eq!(config.api_key.expose(), KEY);
        assert_eq!(config.model, "claude-test");
        assert_eq!(config.effort.as_deref(), Some(DEFAULT_EFFORT));
        assert!(!format!("{config:?}").contains(KEY));

        let defaults = AnthropicConfig::from_lookup(lookup(&[("ANTHROPIC_API_KEY", KEY)])).unwrap();
        assert_eq!(defaults.model, DEFAULT_MODEL);

        let no_effort = AnthropicConfig::from_lookup(lookup(&[
            ("ANTHROPIC_API_KEY", KEY),
            ("ANTHROPIC_EFFORT", " "),
        ]))
        .unwrap();
        assert_eq!(no_effort.effort, None);
    }

    #[test]
    fn a_missing_or_blank_key_is_not_configured() {
        for vars in [&[][..], &[("ANTHROPIC_API_KEY", "  ")][..]] {
            let error = AnthropicConfig::from_lookup(lookup(vars)).unwrap_err();
            assert_eq!(error.kind, ProviderErrorKind::NotConfigured);
            assert_eq!(
                error.to_string(),
                "anthropic not_configured: ANTHROPIC_API_KEY is not set"
            );
        }
    }

    #[test]
    fn request_body_is_schema_constrained_and_holds_no_secret() {
        let ctx = sample_context();
        let mut config = AnthropicConfig::new(Secret::new(KEY));
        let director = AnthropicDirector::new(config.clone()).unwrap();
        let body = director.request_body(ProviderRequest {
            context: &ctx,
            repair: None,
        });
        assert_eq!(body["model"], DEFAULT_MODEL);
        assert_eq!(body["system"], SYSTEM_INSTRUCTION);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["output_config"]["effort"], "low");
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(
            body["output_config"]["format"]["schema"],
            gemini_response_schema(&ctx)
        );
        assert!(!body.to_string().contains(KEY));

        config.effort = None;
        let body = AnthropicDirector::new(config)
            .unwrap()
            .request_body(ProviderRequest {
                context: &ctx,
                repair: None,
            });
        assert!(body["output_config"].get("effort").is_none());
    }

    #[test]
    fn parses_text_blocks_and_skips_thinking() {
        let output = parse_message(
            &json!({
                "model": "claude-live",
                "stop_reason": "end_turn",
                "content": [
                    {"type": "thinking", "thinking": ""},
                    {"type": "text", "text": "{\"a\":"},
                    {"type": "text", "text": "1}"}
                ],
                "usage": {"input_tokens": 300, "output_tokens": 21, "service_tier": "standard"}
            }),
            "requested",
        )
        .unwrap();
        assert_eq!(output.text, "{\"a\":1}");
        assert_eq!(output.model, "claude-live");
        assert_eq!(output.usage["input_tokens"], 300);
        assert_eq!(output.usage["output_tokens"], 21);
        assert_eq!(output.usage.len(), 2);
    }

    #[test]
    fn refusals_truncation_and_empty_output_are_errors() {
        let cases = [
            (
                json!({"stop_reason": "refusal", "content": []}),
                ProviderErrorKind::Incomplete,
            ),
            (
                json!({"stop_reason": "max_tokens", "content": [{"type": "text", "text": "{"}]}),
                ProviderErrorKind::Incomplete,
            ),
            (
                json!({"stop_reason": "end_turn", "content": []}),
                ProviderErrorKind::Malformed,
            ),
            (json!([]), ProviderErrorKind::Malformed),
        ];
        for (body, kind) in cases {
            assert_eq!(parse_message(&body, "m").unwrap_err().kind, kind, "{body}");
        }
    }
}
