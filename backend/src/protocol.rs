//! Rift WebSocket protocol V1 (JSON).
//!
//! Every frame is an [`Envelope`]. Decoding is staged so that callers get a
//! precise error code:
//!
//! 1. raw JSON object?                    -> `malformed_message`
//! 2. `protocol_version == 1`?            -> `unsupported_protocol_version`
//! 3. strict envelope shape?              -> `malformed_message`
//! 4. known client `message_type`?        -> `unknown_message_type`
//! 5. payload matches the message type?   -> `invalid_payload`
//!
//! See `docs/PROTOCOL.md` for the human-readable contract.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use uuid::Uuid;

use crate::action::PlayerAction;

pub const PROTOCOL_VERSION: u32 = 1;

/// Maximum accepted length of a client-supplied `message_id`.
pub const MAX_MESSAGE_ID_LEN: usize = 128;

/// Client -> server message types.
pub mod client_type {
    pub const HELLO: &str = "hello";
    pub const PING: &str = "ping";
    pub const CREATE_SESSION: &str = "create_session";
    pub const PLAYER_ACTION: &str = "player_action";
}

/// Server -> client message types.
pub mod server_type {
    pub const HELLO_ACK: &str = "hello_ack";
    pub const PONG: &str = "pong";
    pub const SESSION_CREATED: &str = "session_created";
    pub const WORLD_EVENT: &str = "world_event";
    pub const ERROR: &str = "error";
}

/// Common message envelope shared by both directions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub protocol_version: u32,
    pub message_id: String,
    pub message_type: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub session_id: Option<Uuid>,
    /// Server -> client only: the `message_id` this message responds to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub payload: Value,
}

impl Envelope {
    /// Build a server-originated envelope.
    pub fn server(
        message_type: &str,
        session_id: Option<Uuid>,
        reply_to: Option<String>,
        payload: Value,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            message_id: Uuid::new_v4().to_string(),
            message_type: message_type.to_owned(),
            timestamp: Utc::now(),
            session_id,
            reply_to,
            payload,
        }
    }

    /// Build an `error` envelope from a protocol error.
    pub fn error(err: &ProtocolError, session_id: Option<Uuid>) -> Self {
        let payload = serde_json::to_value(ErrorPayload {
            code: err.code,
            message: err.message.clone(),
        })
        .unwrap_or(Value::Null);
        Self::server(
            server_type::ERROR,
            session_id,
            err.reply_to.clone(),
            payload,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    MalformedMessage,
    UnsupportedProtocolVersion,
    UnknownMessageType,
    InvalidPayload,
    HandshakeRequired,
    SessionNotFound,
    SessionMismatch,
    InvalidAction,
    DuplicateAction,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
    /// `message_id` of the offending message, when it could be recovered.
    pub reply_to: Option<String>,
}

impl ProtocolError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            reply_to: None,
        }
    }

    pub fn replying_to(mut self, message_id: impl Into<String>) -> Self {
        self.reply_to = Some(message_id.into());
        self
    }
}

// ---------------------------------------------------------------------------
// Payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelloPayload {
    /// Free-form client name, e.g. `"rift-unreal"` or `"smoke-cli"`.
    pub client: String,
    #[serde(default)]
    pub client_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloAckPayload {
    pub server: String,
    pub server_version: String,
    pub protocol_version: u32,
    pub connection_id: Uuid,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PingPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
}

pub type PongPayload = PingPayload;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSessionPayload {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorPayload {
    pub code: ErrorCode,
    pub message: String,
}

/// A fully decoded client message.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientMessage {
    Hello(HelloPayload),
    Ping(PingPayload),
    CreateSession(CreateSessionPayload),
    PlayerAction(PlayerAction),
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// Decode and validate one client text frame.
pub fn decode_client_message(text: &str) -> Result<(Envelope, ClientMessage), ProtocolError> {
    let raw: Value = serde_json::from_str(text).map_err(|e| {
        ProtocolError::new(ErrorCode::MalformedMessage, format!("invalid JSON: {e}"))
    })?;

    let Value::Object(obj) = &raw else {
        return Err(ProtocolError::new(
            ErrorCode::MalformedMessage,
            "message must be a JSON object",
        ));
    };

    // Recover message_id early so errors can be correlated where possible.
    let message_id = obj
        .get("message_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= MAX_MESSAGE_ID_LEN)
        .map(str::to_owned);
    let with_reply = |err: ProtocolError| match &message_id {
        Some(id) => err.replying_to(id.clone()),
        None => err,
    };

    match obj.get("protocol_version").and_then(Value::as_u64) {
        Some(v) if v == u64::from(PROTOCOL_VERSION) => {}
        Some(v) => {
            return Err(with_reply(ProtocolError::new(
                ErrorCode::UnsupportedProtocolVersion,
                format!("protocol_version {v} is not supported; server speaks {PROTOCOL_VERSION}"),
            )));
        }
        None => {
            return Err(with_reply(ProtocolError::new(
                ErrorCode::MalformedMessage,
                "missing or non-integer protocol_version",
            )));
        }
    }

    let envelope: Envelope = serde_json::from_value(raw).map_err(|e| {
        with_reply(ProtocolError::new(
            ErrorCode::MalformedMessage,
            format!("invalid envelope: {e}"),
        ))
    })?;

    if envelope.message_id.is_empty() || envelope.message_id.len() > MAX_MESSAGE_ID_LEN {
        return Err(ProtocolError::new(
            ErrorCode::MalformedMessage,
            format!("message_id must be 1..={MAX_MESSAGE_ID_LEN} characters"),
        ));
    }
    if envelope.reply_to.is_some() {
        return Err(with_reply(ProtocolError::new(
            ErrorCode::MalformedMessage,
            "reply_to is server-only",
        )));
    }

    let message = match envelope.message_type.as_str() {
        client_type::HELLO => ClientMessage::Hello(payload(&envelope)?),
        client_type::PING => ClientMessage::Ping(payload(&envelope)?),
        client_type::CREATE_SESSION => ClientMessage::CreateSession(payload(&envelope)?),
        client_type::PLAYER_ACTION => ClientMessage::PlayerAction(payload(&envelope)?),
        other => {
            return Err(with_reply(ProtocolError::new(
                ErrorCode::UnknownMessageType,
                format!("unknown message_type {other:?}"),
            )));
        }
    };

    Ok((envelope, message))
}

/// Deserialize the envelope payload; a missing/null payload is treated as `{}`.
fn payload<T: DeserializeOwned>(envelope: &Envelope) -> Result<T, ProtocolError> {
    let value = match &envelope.payload {
        Value::Null => Value::Object(Default::default()),
        other => other.clone(),
    };
    serde_json::from_value(value).map_err(|e| {
        ProtocolError::new(
            ErrorCode::InvalidPayload,
            format!("invalid {} payload: {e}", envelope.message_type),
        )
        .replying_to(envelope.message_id.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope_json(message_type: &str, payload: Value) -> String {
        json!({
            "protocol_version": 1,
            "message_id": "m-1",
            "message_type": message_type,
            "timestamp": "2026-10-03T12:00:00Z",
            "session_id": null,
            "payload": payload,
        })
        .to_string()
    }

    #[test]
    fn decodes_hello() {
        let text = envelope_json("hello", json!({"client": "test"}));
        let (env, msg) = decode_client_message(&text).unwrap();
        assert_eq!(env.message_id, "m-1");
        assert_eq!(
            msg,
            ClientMessage::Hello(HelloPayload {
                client: "test".into(),
                client_version: None
            })
        );
    }

    #[test]
    fn decodes_ping_without_payload() {
        let text = json!({
            "protocol_version": 1,
            "message_id": "m-2",
            "message_type": "ping",
            "timestamp": "2026-10-03T12:00:00Z",
        })
        .to_string();
        let (_, msg) = decode_client_message(&text).unwrap();
        assert_eq!(msg, ClientMessage::Ping(PingPayload { nonce: None }));
    }

    #[test]
    fn server_envelope_round_trips() {
        let env = Envelope::server(
            server_type::PONG,
            None,
            Some("m-1".into()),
            serde_json::to_value(PongPayload {
                nonce: Some("n".into()),
            })
            .unwrap(),
        );
        let text = serde_json::to_string(&env).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["protocol_version"], 1);
        assert_eq!(value["message_type"], "pong");
        assert_eq!(value["reply_to"], "m-1");
        assert_eq!(value["payload"]["nonce"], "n");
        assert!(value["session_id"].is_null());
        let back: Envelope = serde_json::from_str(&text).unwrap();
        assert_eq!(back, env);
    }

    #[test]
    fn error_envelope_serializes_code() {
        let err = ProtocolError::new(ErrorCode::SessionNotFound, "nope").replying_to("m-9");
        let value = serde_json::to_value(Envelope::error(&err, None)).unwrap();
        assert_eq!(value["message_type"], "error");
        assert_eq!(value["reply_to"], "m-9");
        assert_eq!(value["payload"]["code"], "session_not_found");
        assert_eq!(value["payload"]["message"], "nope");
    }

    #[test]
    fn rejects_bad_protocol_version() {
        let text = envelope_json("hello", json!({"client": "t"}))
            .replace("\"protocol_version\":1", "\"protocol_version\":2");
        let err = decode_client_message(&text).unwrap_err();
        assert_eq!(err.code, ErrorCode::UnsupportedProtocolVersion);
        assert_eq!(err.reply_to.as_deref(), Some("m-1"));
    }

    #[test]
    fn rejects_missing_protocol_version() {
        let text = json!({"message_id": "m", "message_type": "ping"}).to_string();
        let err = decode_client_message(&text).unwrap_err();
        assert_eq!(err.code, ErrorCode::MalformedMessage);
    }

    #[test]
    fn rejects_invalid_json() {
        let err = decode_client_message("{not json").unwrap_err();
        assert_eq!(err.code, ErrorCode::MalformedMessage);
    }

    #[test]
    fn rejects_non_object() {
        let err = decode_client_message("[1,2,3]").unwrap_err();
        assert_eq!(err.code, ErrorCode::MalformedMessage);
    }

    #[test]
    fn rejects_unknown_envelope_field() {
        let mut value: Value = serde_json::from_str(&envelope_json("ping", json!({}))).unwrap();
        value["extra"] = json!(true);
        let err = decode_client_message(&value.to_string()).unwrap_err();
        assert_eq!(err.code, ErrorCode::MalformedMessage);
    }

    #[test]
    fn rejects_bad_timestamp() {
        let text = envelope_json("ping", json!({})).replace("2026-10-03T12:00:00Z", "yesterday");
        let err = decode_client_message(&text).unwrap_err();
        assert_eq!(err.code, ErrorCode::MalformedMessage);
    }

    #[test]
    fn rejects_empty_message_id() {
        let text = envelope_json("ping", json!({})).replace("\"m-1\"", "\"\"");
        let err = decode_client_message(&text).unwrap_err();
        assert_eq!(err.code, ErrorCode::MalformedMessage);
    }

    #[test]
    fn rejects_client_reply_to() {
        let mut value: Value = serde_json::from_str(&envelope_json("ping", json!({}))).unwrap();
        value["reply_to"] = json!("x");
        let err = decode_client_message(&value.to_string()).unwrap_err();
        assert_eq!(err.code, ErrorCode::MalformedMessage);
    }

    #[test]
    fn rejects_unknown_message_type() {
        let err = decode_client_message(&envelope_json("teleport", json!({}))).unwrap_err();
        assert_eq!(err.code, ErrorCode::UnknownMessageType);
        // Server-only types are not accepted from clients either.
        let err = decode_client_message(&envelope_json("world_event", json!({}))).unwrap_err();
        assert_eq!(err.code, ErrorCode::UnknownMessageType);
    }

    #[test]
    fn rejects_invalid_payload() {
        let err = decode_client_message(&envelope_json("hello", json!({"client": 7}))).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidPayload);
        let err =
            decode_client_message(&envelope_json("create_session", json!({"x": 1}))).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidPayload);
    }
}
