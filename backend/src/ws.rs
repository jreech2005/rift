//! `GET /ws` — persistent WebSocket connection between a game client and Rust.
//!
//! Each connection has one reader loop and one writer task joined by a bounded
//! channel, so later phases can push server-originated events (e.g. from async
//! AI work) without blocking the read path.

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::AppState;
use crate::action;
use crate::protocol::{
    ClientMessage, Envelope, ErrorCode, HelloAckPayload, PROTOCOL_VERSION, PongPayload,
    ProtocolError, decode_client_message, server_type,
};

/// Maximum size of a single inbound WebSocket message.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Outbound queue depth per connection.
const OUTBOUND_QUEUE: usize = 64;

/// Per-connection protocol state.
#[derive(Debug, Clone)]
pub struct Connection {
    pub connection_id: Uuid,
    pub hello_received: bool,
}

impl Connection {
    pub fn new() -> Self {
        Self {
            connection_id: Uuid::new_v4(),
            hello_received: false,
        }
    }
}

impl Default for Connection {
    fn default() -> Self {
        Self::new()
    }
}

pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let mut conn = Connection::new();
    let connection_id = conn.connection_id;
    info!(%connection_id, "client connected");

    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<Message>(OUTBOUND_QUEUE);

    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sink.send(msg).await.is_err() {
                break;
            }
        }
    });

    while let Some(frame) = stream.next().await {
        let reply = match frame {
            Ok(Message::Text(text)) => handle_text(&state, &mut conn, text.as_str()),
            Ok(Message::Binary(_)) => Envelope::error(
                &ProtocolError::new(
                    ErrorCode::MalformedMessage,
                    "binary frames are not supported; send JSON text",
                ),
                None,
            ),
            // Ping/pong control frames are answered by the WebSocket layer.
            Ok(Message::Ping(_) | Message::Pong(_)) => continue,
            Ok(Message::Close(_)) => break,
            Err(err) => {
                warn!(%connection_id, error = %err, "websocket receive error");
                break;
            }
        };

        let text = match serde_json::to_string(&reply) {
            Ok(text) => text,
            Err(err) => {
                warn!(%connection_id, error = %err, "failed to serialize reply");
                continue;
            }
        };
        if tx.send(Message::Text(text.into())).await.is_err() {
            break;
        }
    }

    drop(tx);
    let _ = writer.await;
    info!(%connection_id, "client disconnected");
}

fn to_payload<T: Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

/// Handle one inbound text frame and produce exactly one reply envelope.
pub fn handle_text(state: &AppState, conn: &mut Connection, text: &str) -> Envelope {
    let (envelope, message) = match decode_client_message(text) {
        Ok(decoded) => decoded,
        Err(err) => {
            debug!(connection_id = %conn.connection_id, code = ?err.code, "rejected message");
            return Envelope::error(&err, None);
        }
    };
    let reply_to = Some(envelope.message_id.clone());

    let handshake_ok =
        conn.hello_received || matches!(message, ClientMessage::Hello(_) | ClientMessage::Ping(_));
    if !handshake_ok {
        let err = ProtocolError::new(
            ErrorCode::HandshakeRequired,
            "send hello before other messages",
        )
        .replying_to(envelope.message_id);
        return Envelope::error(&err, envelope.session_id);
    }

    match message {
        ClientMessage::Hello(hello) => {
            conn.hello_received = true;
            info!(connection_id = %conn.connection_id, client = %hello.client, "hello");
            let ack = HelloAckPayload {
                server: "rift-backend".into(),
                server_version: env!("CARGO_PKG_VERSION").into(),
                protocol_version: PROTOCOL_VERSION,
                connection_id: conn.connection_id,
            };
            Envelope::server(server_type::HELLO_ACK, None, reply_to, to_payload(&ack))
        }
        ClientMessage::Ping(ping) => {
            let pong = PongPayload { nonce: ping.nonce };
            Envelope::server(
                server_type::PONG,
                envelope.session_id,
                reply_to,
                to_payload(&pong),
            )
        }
        ClientMessage::CreateSession(_) => {
            let session = state.sessions.create();
            info!(connection_id = %conn.connection_id, session_id = %session.session_id, "session created");
            Envelope::server(
                server_type::SESSION_CREATED,
                Some(session.session_id),
                reply_to,
                to_payload(&session),
            )
        }
        ClientMessage::PlayerAction(player_action) => {
            let result = action::validate(&player_action, envelope.session_id)
                .and_then(|validated| state.sessions.apply_action(&validated));
            match result {
                Ok(event) => Envelope::server(
                    server_type::WORLD_EVENT,
                    Some(event.session_id),
                    reply_to,
                    to_payload(&event),
                ),
                Err(err) => {
                    Envelope::error(&err.replying_to(envelope.message_id), envelope.session_id)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn frame(message_type: &str, session_id: Option<Uuid>, payload: Value) -> String {
        json!({
            "protocol_version": 1,
            "message_id": format!("m-{message_type}"),
            "message_type": message_type,
            "timestamp": "2026-10-03T12:00:00Z",
            "session_id": session_id,
            "payload": payload,
        })
        .to_string()
    }

    fn action_payload(session_id: Uuid, target: &str) -> Value {
        json!({
            "protocol_version": 1,
            "action_id": Uuid::new_v4(),
            "session_id": session_id,
            "actor_id": "player",
            "action_type": "interact",
            "target": target,
            "timestamp": "2026-10-03T12:00:00Z",
        })
    }

    fn error_code(env: &Envelope) -> Value {
        assert_eq!(env.message_type, "error");
        env.payload["code"].clone()
    }

    #[test]
    fn requires_hello_before_session() {
        let state = AppState::default();
        let mut conn = Connection::new();
        let reply = handle_text(&state, &mut conn, &frame("create_session", None, json!({})));
        assert_eq!(error_code(&reply), "handshake_required");
        assert!(state.sessions.is_empty());
    }

    #[test]
    fn full_flow() {
        let state = AppState::default();
        let mut conn = Connection::new();

        let ack = handle_text(
            &state,
            &mut conn,
            &frame("hello", None, json!({"client": "t"})),
        );
        assert_eq!(ack.message_type, "hello_ack");
        assert_eq!(ack.reply_to.as_deref(), Some("m-hello"));
        assert_eq!(ack.payload["protocol_version"], 1);

        let pong = handle_text(
            &state,
            &mut conn,
            &frame("ping", None, json!({"nonce": "abc"})),
        );
        assert_eq!(pong.message_type, "pong");
        assert_eq!(pong.payload["nonce"], "abc");

        let created = handle_text(&state, &mut conn, &frame("create_session", None, json!({})));
        assert_eq!(created.message_type, "session_created");
        let sid = created.session_id.unwrap();
        assert!(state.sessions.get(sid).is_some());

        let event = handle_text(
            &state,
            &mut conn,
            &frame("player_action", Some(sid), action_payload(sid, "test_door")),
        );
        assert_eq!(event.message_type, "world_event");
        assert_eq!(event.payload["event_type"], "interaction_acknowledged");
        assert_eq!(event.payload["target"], "test_door");
        assert_eq!(state.sessions.get(sid).unwrap().event_count, 1);
    }

    #[test]
    fn rejects_unknown_session() {
        let state = AppState::default();
        let mut conn = Connection::new();
        handle_text(
            &state,
            &mut conn,
            &frame("hello", None, json!({"client": "t"})),
        );
        let sid = Uuid::new_v4();
        let reply = handle_text(
            &state,
            &mut conn,
            &frame("player_action", Some(sid), action_payload(sid, "door")),
        );
        assert_eq!(error_code(&reply), "session_not_found");
        assert_eq!(reply.reply_to.as_deref(), Some("m-player_action"));
    }

    #[test]
    fn malformed_frame_yields_error() {
        let state = AppState::default();
        let mut conn = Connection::new();
        let reply = handle_text(&state, &mut conn, "garbage");
        assert_eq!(error_code(&reply), "malformed_message");
    }
}
