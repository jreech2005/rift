//! The LLM failover chain over real HTTP, against local stand-ins for the
//! Gemini Interactions API and the Anthropic Messages API. No test here
//! contacts Google or Anthropic or needs an API key.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use common::{DECISION, hank_context};
use rift_backend::director::{
    AnthropicConfig, AnthropicDirector, DirectorEngine, DirectorProvider, FailoverPolicy,
    FailoverProvider, FallbackDirector, GeminiConfig, GeminiDirector, Leg, ProviderErrorKind,
    ProviderRequest, Secret,
};
use serde_json::{Value, json};
use tokio::net::TcpListener;

const GEMINI_KEY: &str = "itest-gemini-key-do-not-leak-9f8e7d6c";
const ANTHROPIC_KEY: &str = "itest-anthropic-key-do-not-leak-1a2b3c";
const TIMEOUT: Duration = Duration::from_secs(5);

struct Recorded {
    path: &'static str,
    headers: HeaderMap,
    body: Value,
}

/// One server for both APIs: canned `(status, body)` replies in order.
#[derive(Default)]
struct Mock {
    responses: Mutex<VecDeque<(u16, String)>>,
    requests: Mutex<Vec<Recorded>>,
}

impl Mock {
    /// `(path, model)` of every request, in order.
    fn calls(&self) -> Vec<(&'static str, String)> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| (r.path, r.body["model"].as_str().unwrap_or("").to_owned()))
            .collect()
    }
}

fn record(mock: &Mock, path: &'static str, headers: HeaderMap, body: &str) -> (StatusCode, String) {
    mock.requests.lock().unwrap().push(Recorded {
        path,
        headers,
        body: serde_json::from_str(body).unwrap_or(Value::Null),
    });
    match mock.responses.lock().unwrap().pop_front() {
        Some((status, body)) => (StatusCode::from_u16(status).unwrap(), body),
        None => (StatusCode::IM_A_TEAPOT, "no canned response left".into()),
    }
}

async fn interactions(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
    body: String,
) -> (StatusCode, String) {
    record(&mock, "gemini", headers, &body)
}

async fn messages(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
    body: String,
) -> (StatusCode, String) {
    record(&mock, "anthropic", headers, &body)
}

async fn spawn_mock(responses: Vec<(u16, String)>) -> (Arc<Mock>, String) {
    let mock = Arc::new(Mock {
        responses: Mutex::new(responses.into()),
        requests: Mutex::default(),
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Router::new()
        .route("/interactions", post(interactions))
        .route("/v1/messages", post(messages))
        .with_state(mock.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (mock, format!("http://{addr}"))
}

fn gemini_ok(text: &str) -> (u16, String) {
    let body = json!({
        "status": "completed",
        "steps": [{"type": "model_output", "content": [{"type": "text", "text": text}]}],
        "usage": {"total_tokens": 321}
    });
    (200, body.to_string())
}

fn anthropic_ok(text: &str) -> (u16, String) {
    let body = json!({
        "type": "message",
        "model": "claude-mock",
        "stop_reason": "end_turn",
        "content": [{"type": "thinking", "thinking": ""}, {"type": "text", "text": text}],
        "usage": {"input_tokens": 300, "output_tokens": 21}
    });
    (200, body.to_string())
}

fn error(status: u16, message: &str) -> (u16, String) {
    (
        status,
        json!({"type": "error", "error": {"type": "api_error", "message": message}}).to_string(),
    )
}

fn gemini(base_url: &str) -> GeminiDirector {
    let mut config = GeminiConfig::new(Secret::new(GEMINI_KEY));
    config.model = "gemini-primary".to_owned();
    config.base_url = base_url.to_owned();
    config.timeout = TIMEOUT;
    GeminiDirector::new(config).unwrap()
}

fn anthropic(base_url: &str) -> AnthropicDirector {
    let mut config = AnthropicConfig::new(Secret::new(ANTHROPIC_KEY));
    config.model = "claude-secondary".to_owned();
    config.base_url = base_url.to_owned();
    config.timeout = TIMEOUT;
    AnthropicDirector::new(config).unwrap()
}

/// gemini-primary (one retry) -> gemini-backup -> claude-secondary -> rules.
fn engine(base_url: &str) -> DirectorEngine {
    let primary = gemini(base_url);
    let backup = primary.with_model("gemini-backup");
    let chain = FailoverProvider::new(vec![
        Leg::new(Arc::new(primary), "gemini:gemini-primary", TIMEOUT).with_retries(1),
        Leg::new(Arc::new(backup), "gemini:gemini-backup", TIMEOUT),
        Leg::new(
            Arc::new(anthropic(base_url)),
            "anthropic:claude-secondary",
            TIMEOUT,
        ),
    ])
    .with_policy(FailoverPolicy {
        backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(20),
        budget: TIMEOUT,
    });
    DirectorEngine::new(Arc::new(chain)).with_fallback(Arc::new(FallbackDirector))
}

fn call(path: &'static str, model: &str) -> (&'static str, String) {
    (path, model.to_owned())
}

#[tokio::test]
async fn a_healthy_primary_makes_one_request() {
    let (mock, url) = spawn_mock(vec![gemini_ok(DECISION)]).await;
    let decision = engine(&url).decide(&hank_context()).await.unwrap();

    assert_eq!(decision.metadata.provider, "gemini");
    assert_eq!(decision.metadata.model, "gemini-primary");
    assert_eq!(mock.calls(), [call("gemini", "gemini-primary")]);
}

#[tokio::test]
async fn a_503_then_success_is_one_retry_on_the_same_model() {
    let (mock, url) = spawn_mock(vec![error(503, "overloaded"), gemini_ok(DECISION)]).await;
    let decision = engine(&url).decide(&hank_context()).await.unwrap();

    assert_eq!(decision.metadata.model, "gemini-primary");
    assert_eq!(decision.metadata.attempts, 1);
    assert_eq!(
        mock.calls(),
        [
            call("gemini", "gemini-primary"),
            call("gemini", "gemini-primary")
        ]
    );
}

#[tokio::test]
async fn an_exhausted_primary_model_fails_over_to_the_backup_model() {
    let (mock, url) = spawn_mock(vec![
        error(503, "overloaded"),
        error(429, "quota"),
        gemini_ok(DECISION),
    ])
    .await;
    let decision = engine(&url).decide(&hank_context()).await.unwrap();

    assert_eq!(decision.metadata.provider, "gemini");
    assert_eq!(decision.metadata.model, "gemini-backup");
    assert_eq!(decision.metadata.fallback_reason, None);
    assert_eq!(
        mock.calls(),
        [
            call("gemini", "gemini-primary"),
            call("gemini", "gemini-primary"),
            call("gemini", "gemini-backup")
        ]
    );
}

#[tokio::test]
async fn anthropic_answers_when_gemini_is_down() {
    let (mock, url) = spawn_mock(vec![
        error(503, "overloaded"),
        error(503, "overloaded"),
        error(503, "overloaded"),
        anthropic_ok(DECISION),
    ])
    .await;
    let ctx = hank_context();
    let decision = engine(&url).decide(&ctx).await.unwrap();

    assert_eq!(decision.metadata.provider, "anthropic");
    assert_eq!(decision.metadata.model, "claude-mock");
    assert_eq!(decision.metadata.usage["output_tokens"], 21);
    assert_eq!(decision.actions.len(), 7);
    assert_eq!(mock.calls().len(), 4);
    assert_eq!(mock.calls()[3], call("anthropic", "claude-secondary"));

    let requests = mock.requests.lock().unwrap();
    let request = &requests[3];
    assert_eq!(request.headers["x-api-key"], ANTHROPIC_KEY);
    assert_eq!(request.headers["anthropic-version"], "2023-06-01");
    let body = request.body.to_string();
    assert!(!body.contains(ANTHROPIC_KEY) && !body.contains(GEMINI_KEY));
    assert_eq!(request.body["messages"][0]["role"], "user");
    assert_eq!(
        request.body["output_config"]["format"]["type"],
        "json_schema"
    );
    assert_eq!(
        request.body["output_config"]["format"]["schema"],
        rift_backend::director::schema::gemini_response_schema(&ctx)
    );
    // The Gemini requests never saw the Anthropic key, and vice versa.
    assert_eq!(requests[0].headers["x-goog-api-key"], GEMINI_KEY);
    assert!(requests[0].headers.get("x-api-key").is_none());
    assert!(request.headers.get("x-goog-api-key").is_none());
}

#[tokio::test]
async fn the_deterministic_rules_answer_when_every_llm_is_down() {
    let (mock, url) = spawn_mock(vec![
        error(503, "overloaded"),
        error(503, "overloaded"),
        error(503, "overloaded"),
        error(529, "Overloaded"),
        gemini_ok(DECISION),
        anthropic_ok(DECISION),
    ])
    .await;
    let decision = engine(&url).decide(&hank_context()).await.unwrap();

    assert_eq!(decision.metadata.provider, "fallback");
    assert_eq!(
        decision.metadata.fallback_reason.as_deref(),
        Some("anthropic unavailable: HTTP 529: Overloaded")
    );
    assert_eq!(mock.calls().len(), 4, "every leg once, the primary twice");
}

#[tokio::test]
async fn anthropic_failures_are_classified_without_leaking_the_key() {
    let cases = [
        (429, ProviderErrorKind::RateLimited),
        (529, ProviderErrorKind::Unavailable),
        (500, ProviderErrorKind::Unavailable),
        (408, ProviderErrorKind::Timeout),
        (401, ProviderErrorKind::Auth),
        (400, ProviderErrorKind::Http),
    ];
    let ctx = hank_context();
    for (status, kind) in cases {
        let message = format!("rejected key {ANTHROPIC_KEY}");
        let (mock, url) = spawn_mock(vec![error(status, &message)]).await;
        let error = anthropic(&url)
            .propose(ProviderRequest {
                context: &ctx,
                repair: None,
            })
            .await
            .unwrap_err();
        assert_eq!(error.kind, kind, "HTTP {status}");
        assert_eq!(error.status, Some(status));
        assert_eq!(
            error.to_string(),
            format!("anthropic {kind}: HTTP {status}: rejected key [redacted]")
        );
        assert!(!format!("{error:?}").contains(ANTHROPIC_KEY));
        assert_eq!(mock.calls().len(), 1, "the provider itself never retries");
    }
}

#[tokio::test]
async fn preflight_pings_are_one_tiny_request_each() {
    let (mock, url) = spawn_mock(vec![
        gemini_ok("OK"),
        anthropic_ok("OK"),
        error(503, "overloaded"),
        error(529, "Overloaded"),
    ])
    .await;
    gemini(&url).ping().await.unwrap();
    anthropic(&url).ping().await.unwrap();
    let down = gemini(&url).ping().await.unwrap_err();
    assert_eq!(down.kind, ProviderErrorKind::Unavailable);
    let down = anthropic(&url).ping().await.unwrap_err();
    assert_eq!(down.status, Some(529));

    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].body["store"], false);
    assert!(requests[0].body.get("response_format").is_none());
    assert_eq!(requests[1].body["max_tokens"], 16);
    assert!(requests.iter().all(|r| r.body.to_string().len() < 400));
}
