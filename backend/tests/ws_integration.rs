//! End-to-end tests over a real TCP listener and WebSocket connection.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use futures_util::{SinkExt, StreamExt};
use rift_backend::{AppState, app};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;
use uuid::Uuid;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn spawn_server() -> (String, AppState) {
    let state = AppState::default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("ws://{addr}/ws"), state)
}

fn envelope(message_type: &str, session_id: Option<Uuid>, payload: Value) -> String {
    json!({
        "protocol_version": 1,
        "message_id": Uuid::new_v4().to_string(),
        "message_type": message_type,
        "timestamp": chrono::Utc::now(),
        "session_id": session_id,
        "payload": payload,
    })
    .to_string()
}

async fn roundtrip(ws: &mut Ws, text: String) -> Value {
    ws.send(Message::text(text)).await.unwrap();
    loop {
        let msg = ws.next().await.expect("connection closed").unwrap();
        if let Message::Text(t) = msg {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

#[tokio::test]
async fn health_endpoint() {
    let response = app(AppState::default())
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["service"], "rift-backend");
}

#[tokio::test]
async fn websocket_protocol_flow() {
    let (url, state) = spawn_server().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let ack = roundtrip(&mut ws, envelope("hello", None, json!({"client": "itest"}))).await;
    assert_eq!(ack["message_type"], "hello_ack");
    assert_eq!(ack["payload"]["server"], "rift-backend");

    let pong = roundtrip(&mut ws, envelope("ping", None, json!({"nonce": "n1"}))).await;
    assert_eq!(pong["message_type"], "pong");
    assert_eq!(pong["payload"]["nonce"], "n1");

    let created = roundtrip(&mut ws, envelope("create_session", None, json!({}))).await;
    assert_eq!(created["message_type"], "session_created");
    let sid: Uuid = serde_json::from_value(created["session_id"].clone()).unwrap();
    assert!(
        state.sessions.get(sid).is_some(),
        "session must exist in memory"
    );

    let action_id = Uuid::new_v4();
    let action = json!({
        "protocol_version": 1,
        "action_id": action_id,
        "session_id": sid,
        "actor_id": "player",
        "action_type": "interact",
        "target": "test_door",
        "timestamp": chrono::Utc::now(),
    });
    let event = roundtrip(
        &mut ws,
        envelope("player_action", Some(sid), action.clone()),
    )
    .await;
    assert_eq!(event["message_type"], "world_event");
    assert_eq!(event["payload"]["event_type"], "interaction_acknowledged");
    assert_eq!(event["payload"]["target"], "test_door");
    assert_eq!(
        event["payload"]["event_id"],
        rift_backend::action::event_id_for(action_id).to_string()
    );

    // Replaying the same action is rejected, not re-applied.
    let dup = roundtrip(&mut ws, envelope("player_action", Some(sid), action)).await;
    assert_eq!(dup["payload"]["code"], "duplicate_action");
    assert_eq!(state.sessions.get(sid).unwrap().event_count, 1);

    // Errors keep the connection usable.
    let bad = roundtrip(&mut ws, "{oops".to_string()).await;
    assert_eq!(bad["payload"]["code"], "malformed_message");
    let v2 = roundtrip(
        &mut ws,
        envelope("ping", None, json!({}))
            .replace("\"protocol_version\":1", "\"protocol_version\":2"),
    )
    .await;
    assert_eq!(v2["payload"]["code"], "unsupported_protocol_version");
    let pong = roundtrip(&mut ws, envelope("ping", None, json!({}))).await;
    assert_eq!(pong["message_type"], "pong");

    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn rejects_oversized_message() {
    let (url, _) = spawn_server().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let huge = "x".repeat(rift_backend::ws::MAX_MESSAGE_BYTES + 1);
    let _ = ws.send(Message::text(huge)).await;
    // The server closes the connection rather than processing the frame.
    let next = ws.next().await;
    assert!(
        !matches!(next, Some(Ok(Message::Text(_)))),
        "oversized frame must not be answered: {next:?}"
    );
}
