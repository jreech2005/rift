//! The Gemini Director provider over real HTTP, against a local stand-in for
//! the Interactions API. No test here contacts Google or needs an API key.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use common::{DECISION, hank_context};
use rift_backend::director::{
    DirectorEngine, DirectorError, FallbackDirector, GeminiConfig, GeminiDirector, IssueCode,
    ProviderError, ProviderErrorKind, Secret,
};
use serde_json::{Value, json};
use tokio::net::TcpListener;

const KEY: &str = "itest-key-do-not-leak-9f8e7d6c5b4a";
const TIMEOUT: Duration = Duration::from_secs(5);

struct Canned {
    status: u16,
    body: String,
    delay: Duration,
}

fn reply(status: u16, body: impl Into<String>) -> Canned {
    Canned {
        status,
        body: body.into(),
        delay: Duration::ZERO,
    }
}

/// A successful Interactions response whose model output is `text`.
fn interaction(text: &str) -> Canned {
    let body = json!({
        "status": "completed",
        "model": "gemini-mock",
        "steps": [
            {"type": "thought", "content": [{"type": "text", "text": "thinking..."}]},
            {"type": "model_output", "content": [{"type": "text", "text": text}]}
        ],
        "usage": {"total_tokens": 321, "total_input_tokens": 300, "total_output_tokens": 21}
    });
    reply(200, body.to_string())
}

fn google_error(status: u16, message: &str) -> Canned {
    reply(
        status,
        json!({"error": {"code": status, "message": message, "status": "ERROR"}}).to_string(),
    )
}

struct Recorded {
    api_key: Option<String>,
    body: Value,
}

#[derive(Default)]
struct Mock {
    responses: Mutex<VecDeque<Canned>>,
    requests: Mutex<Vec<Recorded>>,
}

impl Mock {
    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

async fn interactions(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
    body: String,
) -> (StatusCode, String) {
    mock.requests.lock().unwrap().push(Recorded {
        api_key: headers
            .get("x-goog-api-key")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        body: serde_json::from_str(&body).unwrap_or(Value::Null),
    });
    let canned = mock.responses.lock().unwrap().pop_front();
    match canned {
        Some(canned) => {
            tokio::time::sleep(canned.delay).await;
            (StatusCode::from_u16(canned.status).unwrap(), canned.body)
        }
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "no canned response left".to_owned(),
        ),
    }
}

async fn spawn_mock(responses: Vec<Canned>) -> (Arc<Mock>, String) {
    let mock = Arc::new(Mock {
        responses: Mutex::new(responses.into()),
        requests: Mutex::default(),
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Router::new()
        .route("/interactions", post(interactions))
        .with_state(mock.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (mock, format!("http://{addr}"))
}

fn engine(base_url: &str, timeout: Duration) -> DirectorEngine {
    let mut config = GeminiConfig::new(Secret::new(KEY));
    config.model = "test-model".to_owned();
    config.base_url = base_url.to_owned();
    config.timeout = timeout;
    DirectorEngine::new(Arc::new(GeminiDirector::new(config).unwrap()))
}

/// The provider error behind a failed decision; also proves the key is in
/// neither the message nor the debug output.
#[track_caller]
fn provider_error(error: DirectorError) -> ProviderError {
    assert!(!error.to_string().contains(KEY), "key leaked: {error}");
    assert!(!format!("{error:?}").contains(KEY), "key leaked: {error:?}");
    match error {
        DirectorError::Provider(error) => error,
        other => panic!("expected a provider error, got {other:?}"),
    }
}

#[tokio::test]
async fn structured_output_becomes_a_validated_decision() {
    let (mock, url) = spawn_mock(vec![interaction(DECISION)]).await;
    let ctx = hank_context();
    let decision = engine(&url, TIMEOUT).decide(&ctx).await.unwrap();

    assert_eq!(decision.metadata.provider, "gemini");
    assert_eq!(decision.metadata.model, "gemini-mock");
    assert_eq!(decision.metadata.attempts, 1);
    assert!(!decision.metadata.repaired);
    assert_eq!(decision.metadata.usage["total_tokens"], 321);
    assert_eq!(decision.actions.len(), 7);
    assert_eq!(decision.actions[1].type_name(), "invalidate_mission");

    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(
        request.api_key.as_deref(),
        Some(KEY),
        "key travels in the header"
    );
    assert!(
        !request.body.to_string().contains(KEY),
        "and never in the body"
    );
    assert_eq!(request.body["model"], "test-model");
    assert_eq!(request.body["store"], false);
    assert_eq!(
        request.body["response_format"]["mime_type"],
        "application/json"
    );
    let schema = &request.body["response_format"]["schema"];
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["actions"]["items"]["anyOf"].is_array());
    assert!(
        request.body["system_instruction"]
            .as_str()
            .unwrap()
            .contains("Never resurrect a dead character")
    );
    let input = request.body["input"].as_str().unwrap();
    assert!(input.contains("hank_schrader"));
    assert!(input.contains("check under his mattress"));
}

#[tokio::test]
async fn malformed_structured_output_is_repaired_exactly_once() {
    let bad = json!({
        "reason_code": "player_divergence",
        "actions": [{"type": "execute_script", "action_id": "a1", "script": "arrest(walter)"}],
        "confidence": 0.9
    })
    .to_string();
    let (mock, url) = spawn_mock(vec![interaction(&bad), interaction(DECISION)]).await;
    let decision = engine(&url, TIMEOUT).decide(&hank_context()).await.unwrap();

    assert_eq!(decision.metadata.attempts, 2);
    assert!(decision.metadata.repaired);
    assert_eq!(
        decision.metadata.usage["total_tokens"], 642,
        "usage is summed"
    );
    assert!(
        decision
            .actions
            .iter()
            .all(|a| a.type_name() != "execute_script")
    );

    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let first = requests[0].body["input"].as_str().unwrap();
    let second = requests[1].body["input"].as_str().unwrap();
    assert!(!first.contains("Your previous output was rejected"));
    assert!(
        second.starts_with(first),
        "the repair extends the original prompt"
    );
    assert!(second.contains("Your previous output was rejected by validation."));
    assert!(
        second.contains("arrest(walter)"),
        "shows the rejected output"
    );
    assert!(second.contains("- actions[0]: unknown action type \"execute_script\""));
}

#[tokio::test]
async fn output_that_stays_malformed_is_rejected_after_two_requests() {
    let (mock, url) = spawn_mock(vec![
        interaction("I would have Hank arrest everyone."),
        interaction("{\"actions\": \"all of them\"}"),
        interaction(DECISION), // must never be requested
    ])
    .await;
    let error = engine(&url, TIMEOUT)
        .decide(&hank_context())
        .await
        .unwrap_err();

    let DirectorError::InvalidDecision { attempts, issues } = error else {
        panic!("expected InvalidDecision, got {error:?}");
    };
    assert_eq!(attempts, 2);
    assert!(issues.iter().any(|i| i.code == IssueCode::Malformed));
    assert_eq!(
        mock.request_count(),
        2,
        "one attempt plus one repair, never more"
    );
}

#[tokio::test]
async fn http_429_surfaces_as_rate_limited_and_is_not_retried() {
    let message = format!("Quota exceeded for key {KEY}. Please retry in 37s.");
    let (mock, url) = spawn_mock(vec![google_error(429, &message), interaction(DECISION)]).await;
    let error = provider_error(
        engine(&url, TIMEOUT)
            .decide(&hank_context())
            .await
            .unwrap_err(),
    );

    assert_eq!(error.kind, ProviderErrorKind::RateLimited);
    assert_eq!(error.status, Some(429));
    assert_eq!(
        error.to_string(),
        "gemini rate_limited: HTTP 429: Quota exceeded for key [redacted]. Please retry in 37s."
    );
    assert_eq!(mock.request_count(), 1);
}

#[tokio::test]
async fn http_503_surfaces_as_unavailable_and_is_not_retried() {
    let (mock, url) = spawn_mock(vec![
        google_error(503, "The model is overloaded. Please try again later."),
        interaction(DECISION),
    ])
    .await;
    let error = provider_error(
        engine(&url, TIMEOUT)
            .decide(&hank_context())
            .await
            .unwrap_err(),
    );

    assert_eq!(error.kind, ProviderErrorKind::Unavailable);
    assert_eq!(error.status, Some(503));
    assert_eq!(
        error.to_string(),
        "gemini unavailable: HTTP 503: The model is overloaded. Please try again later."
    );
    assert_eq!(mock.request_count(), 1);
}

#[tokio::test]
async fn other_http_failures_are_classified() {
    let cases = [
        (
            google_error(401, "API key not valid."),
            ProviderErrorKind::Auth,
            401,
        ),
        (
            google_error(403, "Permission denied."),
            ProviderErrorKind::Auth,
            403,
        ),
        (
            google_error(400, "Invalid JSON schema."),
            ProviderErrorKind::Http,
            400,
        ),
        (
            reply(500, "<html>Internal Server Error</html>"),
            ProviderErrorKind::Unavailable,
            500,
        ),
    ];
    for (canned, kind, status) in cases {
        let (mock, url) = spawn_mock(vec![canned]).await;
        let error = provider_error(
            engine(&url, TIMEOUT)
                .decide(&hank_context())
                .await
                .unwrap_err(),
        );
        assert_eq!(error.kind, kind, "HTTP {status}");
        assert_eq!(error.status, Some(status));
        assert!(error.detail.starts_with(&format!("HTTP {status}")));
        assert_eq!(mock.request_count(), 1);
    }
}

#[tokio::test]
async fn long_provider_messages_are_cut() {
    let (_mock, url) = spawn_mock(vec![google_error(500, &"x".repeat(5_000))]).await;
    let error = provider_error(
        engine(&url, TIMEOUT)
            .decide(&hank_context())
            .await
            .unwrap_err(),
    );
    assert!(error.detail.chars().count() < 320, "{}", error.detail.len());
}

#[tokio::test]
async fn a_slow_response_times_out() {
    let mut slow = interaction(DECISION);
    slow.delay = Duration::from_secs(3);
    let (mock, url) = spawn_mock(vec![slow, interaction(DECISION)]).await;

    let started = Instant::now();
    let error = provider_error(
        engine(&url, Duration::from_millis(200))
            .decide(&hank_context())
            .await
            .unwrap_err(),
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(error.kind, ProviderErrorKind::Timeout);
    assert_eq!(error.status, None);
    assert_eq!(mock.request_count(), 1, "a timeout is not retried");
}

#[tokio::test]
async fn unusable_success_responses_are_provider_errors() {
    let cases = [
        (reply(200, "<html>ok</html>"), ProviderErrorKind::Malformed),
        (
            reply(200, "{\"status\": \"incomplete\"}"),
            ProviderErrorKind::Incomplete,
        ),
        (
            reply(200, "{\"status\": \"completed\", \"steps\": []}"),
            ProviderErrorKind::Malformed,
        ),
        (
            reply(200, "{\"status\": \"failed\"}"),
            ProviderErrorKind::Unavailable,
        ),
    ];
    for (canned, kind) in cases {
        let (mock, url) = spawn_mock(vec![canned, interaction(DECISION)]).await;
        let error = provider_error(
            engine(&url, TIMEOUT)
                .decide(&hank_context())
                .await
                .unwrap_err(),
        );
        assert_eq!(error.kind, kind);
        assert_eq!(mock.request_count(), 1, "{kind} is not retried");
    }
}

#[tokio::test]
async fn an_unreachable_server_is_a_network_error_without_the_url() {
    // Bind to learn a free port, then close it again.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let error = provider_error(
        engine(&format!("http://{addr}"), TIMEOUT)
            .decide(&hank_context())
            .await
            .unwrap_err(),
    );
    assert_eq!(error.kind, ProviderErrorKind::Network);
    assert_eq!(error.detail, "connection failed");
    assert!(!error.to_string().contains(&addr.port().to_string()));
}

#[tokio::test]
async fn deterministic_fallback_takes_over_when_gemini_is_down() {
    let (mock, url) = spawn_mock(vec![google_error(503, "high demand")]).await;
    let decision = engine(&url, TIMEOUT)
        .with_fallback(Arc::new(FallbackDirector))
        .decide(&hank_context())
        .await
        .unwrap();

    assert_eq!(decision.metadata.provider, "fallback");
    assert_eq!(
        decision.metadata.fallback_reason.as_deref(),
        Some("gemini unavailable: HTTP 503: high demand")
    );
    assert!(
        decision
            .actions
            .iter()
            .any(|a| a.type_name() == "invalidate_mission")
    );
    assert_eq!(mock.request_count(), 1);
}
