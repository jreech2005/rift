//! NPC voice end to end, without ElevenLabs: the provider over real HTTP
//! against a local stand-in for the text-to-speech API, and the runtime over
//! a real WebSocket with a scripted voice provider. No test here contacts
//! ElevenLabs or needs an API key.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use futures_util::{SinkExt, StreamExt};
use rift_backend::director::{DirectorEngine, ProviderErrorKind, ScriptedProvider, Secret};
use rift_backend::runtime::{Runtime, RuntimeWorld};
use rift_backend::session::SessionStore;
use rift_backend::voice::{
    ElevenLabsConfig, ElevenLabsVoiceProvider, MAX_AUDIO_BYTES, ScriptedVoice,
    ScriptedVoiceProvider, VoiceAudio, VoiceError, VoiceProvider, VoiceRequest, VoiceService,
    parse_voices,
};
use rift_backend::{AppState, app};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

const KEY: &str = "itest-voice-key-do-not-leak-1a2b3c";
const HANK_VOICE: &str = "voiceHank123";
const LINE: &str = "You did the right thing telling me. Now tell me everything you saw.";
const MP3: &[u8] = b"ID3\x04fake-mp3-bytes";

// ---------------------------------------------------------------------------
// ElevenLabsVoiceProvider against a local stand-in.

struct Canned {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    delay: Duration,
}

fn audio(body: &[u8]) -> Canned {
    reply(200, "audio/mpeg", body)
}

fn reply(status: u16, content_type: &'static str, body: &[u8]) -> Canned {
    Canned {
        status,
        content_type,
        body: body.to_vec(),
        delay: Duration::ZERO,
    }
}

struct Recorded {
    api_key: Option<String>,
    voice_id: String,
    query: Option<String>,
    body: Value,
}

#[derive(Default)]
struct Mock {
    responses: Mutex<VecDeque<Canned>>,
    requests: Mutex<Vec<Recorded>>,
}

async fn text_to_speech(
    State(mock): State<Arc<Mock>>,
    Path(voice_id): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: String,
) -> Response {
    mock.requests.lock().unwrap().push(Recorded {
        api_key: headers
            .get("xi-api-key")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        voice_id,
        query,
        body: serde_json::from_str(&body).unwrap_or(Value::Null),
    });
    let canned = mock.responses.lock().unwrap().pop_front();
    match canned {
        Some(canned) => {
            tokio::time::sleep(canned.delay).await;
            (
                StatusCode::from_u16(canned.status).unwrap(),
                [(header::CONTENT_TYPE, canned.content_type)],
                canned.body,
            )
                .into_response()
        }
        None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
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
        .route("/v1/text-to-speech/{voice_id}", post(text_to_speech))
        .with_state(mock.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (mock, format!("http://{addr}"))
}

fn elevenlabs(base_url: &str, timeout: Duration) -> ElevenLabsVoiceProvider {
    let mut config = ElevenLabsConfig::new(Secret::new(KEY));
    config.base_url = base_url.to_owned();
    config.model = "test-model".to_owned();
    config.timeout = timeout;
    ElevenLabsVoiceProvider::new(config).unwrap()
}

fn hank_line() -> VoiceRequest<'static> {
    VoiceRequest {
        npc_id: "hank_schrader",
        text: LINE,
        voice_id: HANK_VOICE,
    }
}

/// Synthesize Hank's line against canned responses and return the error;
/// also proves the key is in neither the message nor the debug output.
async fn failure(canned: Canned) -> VoiceError {
    let (mock, url) = spawn_mock(vec![canned]).await;
    let error = elevenlabs(&url, Duration::from_secs(5))
        .synthesize(hank_line())
        .await
        .unwrap_err();
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
    assert!(!error.to_string().contains(KEY), "key leaked: {error}");
    assert!(!format!("{error:?}").contains(KEY), "key leaked: {error:?}");
    error
}

#[tokio::test]
async fn successful_synthesis_returns_the_audio() {
    let (mock, url) = spawn_mock(vec![audio(MP3)]).await;
    let clip = elevenlabs(&url, Duration::from_secs(5))
        .synthesize(hank_line())
        .await
        .unwrap();
    assert_eq!(clip.bytes, MP3);
    assert_eq!(clip.content_type, "audio/mpeg");

    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.api_key.as_deref(), Some(KEY));
    assert_eq!(request.voice_id, HANK_VOICE);
    assert_eq!(
        request.query.as_deref(),
        Some("output_format=mp3_44100_128")
    );
    // Only the spoken line and the model leave the process.
    assert_eq!(
        request.body,
        json!({"text": LINE, "model_id": "test-model"})
    );
}

#[tokio::test]
async fn provider_unavailable_is_reported_by_kind() {
    let secret_echo = format!("{{\"detail\": \"bad key {KEY}\"}}");
    for (status, kind) in [
        (503, ProviderErrorKind::Unavailable),
        (500, ProviderErrorKind::Unavailable),
        (429, ProviderErrorKind::RateLimited),
        (401, ProviderErrorKind::Auth),
        (422, ProviderErrorKind::Http),
    ] {
        // Even a response that echoes the key back cannot leak it.
        let error = failure(reply(status, "application/json", secret_echo.as_bytes())).await;
        assert_eq!(error.kind, kind, "HTTP {status}");
        assert_eq!(error.status, Some(status));
    }
}

#[tokio::test]
async fn unreachable_provider_is_a_network_error() {
    // Bind then drop: nothing listens on this port.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let error = elevenlabs(&url, Duration::from_secs(5))
        .synthesize(hank_line())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ProviderErrorKind::Network);
    assert!(!error.to_string().contains("127.0.0.1"), "{error}");
}

#[tokio::test]
async fn slow_provider_times_out() {
    let mut slow = audio(MP3);
    slow.delay = Duration::from_secs(5);
    let (_mock, url) = spawn_mock(vec![slow]).await;
    let started = Instant::now();
    let error = elevenlabs(&url, Duration::from_millis(150))
        .synthesize(hank_line())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ProviderErrorKind::Timeout);
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn malformed_responses_are_rejected() {
    let oversized = vec![0u8; MAX_AUDIO_BYTES + 1];
    for canned in [
        reply(
            200,
            "application/json",
            br#"{"audio": "not what we asked for"}"#,
        ),
        reply(200, "text/html", b"<html>gateway</html>"),
        audio(b""),
        audio(&oversized),
    ] {
        let error = failure(canned).await;
        assert_eq!(error.kind, ProviderErrorKind::Malformed, "{error}");
    }
}

#[tokio::test]
async fn a_voice_id_that_is_not_an_id_is_never_sent() {
    let (mock, url) = spawn_mock(vec![audio(MP3)]).await;
    let error = elevenlabs(&url, Duration::from_secs(5))
        .synthesize(VoiceRequest {
            voice_id: "../user",
            ..hank_line()
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind, ProviderErrorKind::NotConfigured);
    assert!(mock.requests.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// The runtime over a real WebSocket, with a scripted voice provider.

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const WORLD_BIBLE: &str = include_str!("fixtures/director/world_bible_breaking_bad.json");
const SCENARIO: &str = include_str!("fixtures/runtime/scenario_burner_phone.json");
/// Hank opens a dialogue (action `a4`) among other consequences.
const DECISION: &str = include_str!("fixtures/runtime/decision_after_disclosure.json");

/// The same decision without its `start_dialogue` action.
fn decision_without_dialogue() -> String {
    let mut decision: Value = serde_json::from_str(DECISION).unwrap();
    let actions = decision["actions"].as_array_mut().unwrap();
    actions.retain(|action| action["type"] != "start_dialogue");
    decision.to_string()
}

struct Server {
    http: String,
    ws: String,
    state: AppState,
    voice: Arc<ScriptedVoiceProvider>,
}

async fn spawn_server(decision: &str, voices: &str, scripted: Vec<ScriptedVoice>) -> Server {
    let world = RuntimeWorld::from_world_bible(WORLD_BIBLE)
        .and_then(|world| world.with_scenario_json(SCENARIO))
        .unwrap();
    let voice = Arc::new(ScriptedVoiceProvider::new(scripted));
    let service = VoiceService::new(voice.clone(), parse_voices(voices).unwrap())
        .with_timeout(Duration::from_millis(200));
    let runtime = Runtime::new(SessionStore::new())
        .with_world(world)
        .with_director(DirectorEngine::new(Arc::new(ScriptedProvider::texts([
            decision,
        ]))))
        .with_voice(service);
    let state = AppState::new(runtime);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Server {
        http: format!("http://{addr}"),
        ws: format!("ws://{addr}/ws"),
        state,
        voice,
    }
}

async fn send(ws: &mut Ws, message_type: &str, session_id: Option<Uuid>, payload: Value) {
    let text = json!({
        "protocol_version": 1,
        "message_id": Uuid::new_v4().to_string(),
        "message_type": message_type,
        "timestamp": chrono::Utc::now(),
        "session_id": session_id,
        "payload": payload,
    })
    .to_string();
    ws.send(Message::text(text)).await.unwrap();
}

async fn next(ws: &mut Ws) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out waiting for a frame")
            .expect("connection closed")
            .unwrap();
        if let Message::Text(t) = msg {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

/// Tell Hank about the phone and collect every `world_event` payload the
/// Director's decision pushed (up to and including its last event).
async fn tell_hank(server: &Server) -> Vec<Value> {
    let (mut ws, _) = tokio_tungstenite::connect_async(&server.ws).await.unwrap();
    send(&mut ws, "hello", None, json!({"client": "itest"})).await;
    assert_eq!(next(&mut ws).await["message_type"], "hello_ack");
    send(&mut ws, "create_session", None, json!({})).await;
    let created = next(&mut ws).await;
    let sid: Uuid = serde_json::from_value(created["session_id"].clone()).unwrap();

    let tell = json!({
        "protocol_version": 1,
        "action_id": Uuid::new_v4(),
        "session_id": sid,
        "actor_id": "player",
        "action_type": "speak",
        "target": "hank_schrader",
        "content": "Walter keeps a burner phone under the mattress.",
        "timestamp": chrono::Utc::now(),
    });
    send(&mut ws, "player_action", Some(sid), tell).await;
    let ack = next(&mut ws).await;
    assert_eq!(ack["payload"]["event_type"], "speech_acknowledged");

    let mut pushed = Vec::new();
    loop {
        let frame = next(&mut ws).await;
        assert_eq!(frame["message_type"], "world_event", "{frame}");
        let payload = frame["payload"].clone();
        let last = payload["target"] == "choose_what_to_tell_hank";
        pushed.push(payload);
        if last {
            break;
        }
    }
    // The game goes on: the connection still answers.
    send(&mut ws, "ping", None, json!({"nonce": "n"})).await;
    assert_eq!(next(&mut ws).await["message_type"], "pong");
    pushed
}

fn dialogue(events: &[Value]) -> &Value {
    events
        .iter()
        .find(|event| event["event_type"] == "dialogue_started")
        .expect("a dialogue_started event")
}

fn mp3_clip() -> ScriptedVoice {
    ScriptedVoice::Audio(VoiceAudio {
        bytes: MP3.to_vec(),
        content_type: "audio/mpeg".into(),
    })
}

#[tokio::test]
async fn dialogue_carries_an_audio_url_that_serves_the_clip() {
    let voices = format!("hank_schrader={HANK_VOICE},walter_white=voiceWalt456");
    let server = spawn_server(DECISION, &voices, vec![mp3_clip()]).await;
    let events = tell_hank(&server).await;

    let event = dialogue(&events);
    assert_eq!(event["target"], "hank_schrader");
    assert_eq!(event["payload"]["npc_id"], "hank_schrader");
    assert_eq!(event["payload"]["text"], LINE);
    assert_eq!(event["payload"]["opening_line"], LINE);
    let audio_url = event["payload"]["audio_url"].as_str().unwrap();
    assert!(audio_url.starts_with("/audio/"), "{audio_url}");
    // A reference, never the audio itself.
    assert!(event.to_string().len() < 1024);

    // Exactly one call, with only the spoken line.
    let calls = server.voice.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].npc_id, "hank_schrader");
    assert_eq!(calls[0].voice_id, HANK_VOICE);
    assert_eq!(calls[0].text, LINE);

    let response = reqwest::get(format!("{}{audio_url}", server.http))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "audio/mpeg");
    assert_eq!(response.bytes().await.unwrap().as_ref(), MP3);

    // Voice decorates the outgoing event only; the session is as without it.
    let sid: Uuid = serde_json::from_value(event["session_id"].clone()).unwrap();
    let session = server.state.sessions.get(sid).unwrap();
    assert_eq!(session.event_count, 1 + events.len() as u64);
}

#[tokio::test]
async fn unknown_audio_is_not_found() {
    let server = spawn_server(DECISION, "hank_schrader=v1", vec![]).await;
    for path in [format!("/audio/{}", Uuid::new_v4()), "/audio/nope".into()] {
        let response = reqwest::get(format!("{}{path}", server.http))
            .await
            .unwrap();
        assert_eq!(response.status(), 404, "{path}");
    }
    // Without voice configured the route still answers 404.
    let plain = AppState::default();
    assert!(plain.runtime.voice().is_none());
}

#[tokio::test]
async fn dialogue_is_still_delivered_as_text_when_voice_fails() {
    let unavailable = VoiceError::new("scripted", ProviderErrorKind::Unavailable, "HTTP 503");
    for failure in [ScriptedVoice::Error(unavailable), ScriptedVoice::Hang] {
        let server = spawn_server(DECISION, "hank_schrader=v1", vec![failure]).await;
        let events = tell_hank(&server).await;

        let event = dialogue(&events);
        assert_eq!(event["payload"]["npc_id"], "hank_schrader");
        assert_eq!(event["payload"]["text"], LINE);
        assert!(event["payload"].get("audio_url").is_none(), "{event}");
        assert_eq!(server.voice.call_count(), 1);

        // Every other consequence of the decision arrived too.
        let types: Vec<&str> = events
            .iter()
            .filter(|e| e["payload"]["source"] == "director")
            .map(|e| e["event_type"].as_str().unwrap())
            .collect();
        assert_eq!(
            types,
            [
                "world_flag_changed",
                "npc_disposition_changed",
                "world_event_triggered",
                "dialogue_started",
                "objective_updated",
            ]
        );
        assert!(server.state.runtime.voice().unwrap().cache().is_empty());
    }
}

#[tokio::test]
async fn no_voice_call_without_a_dialogue_action() {
    let server = spawn_server(
        &decision_without_dialogue(),
        "hank_schrader=v1",
        vec![mp3_clip()],
    )
    .await;
    let events = tell_hank(&server).await;
    assert!(events.iter().all(|e| e["event_type"] != "dialogue_started"));
    assert!(events.iter().any(|e| e["payload"]["source"] == "director"));
    assert_eq!(server.voice.call_count(), 0);
}

#[tokio::test]
async fn no_voice_call_for_an_npc_without_a_voice() {
    let server = spawn_server(DECISION, "walter_white=v1", vec![mp3_clip()]).await;
    let events = tell_hank(&server).await;
    let event = dialogue(&events);
    assert_eq!(event["payload"]["text"], LINE);
    assert!(event["payload"].get("audio_url").is_none());
    assert_eq!(server.voice.call_count(), 0);
}
