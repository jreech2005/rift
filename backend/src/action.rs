//! Player actions and the deterministic V1 world-event rules.
//!
//! Phase 0 rules are intentionally trivial: they exist to prove that a client
//! action travels to Rust, is validated against authoritative session state,
//! and comes back as a structured event. Nothing here calls an LLM.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::protocol::{ErrorCode, PROTOCOL_VERSION, ProtocolError};
use crate::session::GameSession;

pub const MAX_IDENTIFIER_LEN: usize = 64;
pub const MAX_CONTENT_LEN: usize = 500;

/// Namespace for deriving deterministic event ids from action ids.
const EVENT_ID_NAMESPACE: Uuid = Uuid::from_u128(0x5f1c_7a2e_9d4b_4e61_8a3f_2b7c_0d9e_1a46);

/// A player intent submitted by a client. Never trusted until validated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerAction {
    pub protocol_version: u32,
    pub action_id: Uuid,
    pub session_id: Uuid,
    pub actor_id: String,
    pub action_type: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionType {
    Interact,
    Inspect,
    Move,
    Speak,
}

impl ActionType {
    pub const ALL: [&'static str; 4] = ["interact", "inspect", "move", "speak"];

    fn parse(s: &str) -> Option<Self> {
        match s {
            "interact" => Some(Self::Interact),
            "inspect" => Some(Self::Inspect),
            "move" => Some(Self::Move),
            "speak" => Some(Self::Speak),
            _ => None,
        }
    }
}

/// An action that passed validation and may be applied to a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedAction {
    pub action_id: Uuid,
    pub session_id: Uuid,
    pub actor_id: String,
    pub action_type: ActionType,
    pub target: Option<String>,
    pub content: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorldEventType {
    InteractionAcknowledged,
    InspectionAcknowledged,
    LocationChanged,
    SpeechAcknowledged,
}

/// A structured, validated event the game client may execute.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldEvent {
    pub event_id: Uuid,
    pub session_id: Uuid,
    /// Monotonic per-session sequence number, starting at 1.
    pub sequence: u64,
    pub event_type: WorldEventType,
    #[serde(default)]
    pub target: Option<String>,
    pub payload: Value,
    pub timestamp: DateTime<Utc>,
}

fn invalid(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidAction, message)
}

fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_IDENTIFIER_LEN
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

/// Validate a client action. `envelope_session_id` is the `session_id` from the
/// enclosing envelope and must agree with the action's own `session_id`.
pub fn validate(
    action: &PlayerAction,
    envelope_session_id: Option<Uuid>,
) -> Result<ValidatedAction, ProtocolError> {
    if action.protocol_version != PROTOCOL_VERSION {
        return Err(ProtocolError::new(
            ErrorCode::UnsupportedProtocolVersion,
            format!(
                "player_action protocol_version {} is not supported",
                action.protocol_version
            ),
        ));
    }

    match envelope_session_id {
        Some(id) if id == action.session_id => {}
        Some(_) => {
            return Err(ProtocolError::new(
                ErrorCode::SessionMismatch,
                "envelope session_id does not match action session_id",
            ));
        }
        None => {
            return Err(ProtocolError::new(
                ErrorCode::SessionMismatch,
                "player_action requires envelope session_id",
            ));
        }
    }

    if !is_identifier(&action.actor_id) {
        return Err(invalid(
            "actor_id must be a 1-64 char identifier [A-Za-z0-9_.:-]",
        ));
    }

    let action_type = ActionType::parse(&action.action_type).ok_or_else(|| {
        invalid(format!(
            "unknown action_type {:?}; expected one of {:?}",
            action.action_type,
            ActionType::ALL
        ))
    })?;

    if let Some(target) = &action.target
        && !is_identifier(target)
    {
        return Err(invalid(
            "target must be a 1-64 char identifier [A-Za-z0-9_.:-]",
        ));
    }

    match action_type {
        ActionType::Interact | ActionType::Inspect | ActionType::Move => {
            if action.target.is_none() {
                return Err(invalid(format!("{} requires a target", action.action_type)));
            }
            if action.content.is_some() {
                return Err(invalid(format!(
                    "{} does not accept content",
                    action.action_type
                )));
            }
        }
        ActionType::Speak => match &action.content {
            Some(c) if !c.trim().is_empty() && c.chars().count() <= MAX_CONTENT_LEN => {}
            _ => {
                return Err(invalid(format!(
                    "speak requires non-empty content of at most {MAX_CONTENT_LEN} chars"
                )));
            }
        },
    }

    Ok(ValidatedAction {
        action_id: action.action_id,
        session_id: action.session_id,
        actor_id: action.actor_id.clone(),
        action_type,
        target: action.target.clone(),
        content: action.content.clone(),
    })
}

/// Deterministically derive the event id for an action.
pub fn event_id_for(action_id: Uuid) -> Uuid {
    Uuid::new_v5(&EVENT_ID_NAMESPACE, action_id.as_bytes())
}

/// Apply a validated action to the authoritative session and return the
/// resulting event. Given the same session state, action and `now`, the
/// output is always identical.
pub fn apply(
    session: &mut GameSession,
    action: &ValidatedAction,
    now: DateTime<Utc>,
) -> Result<WorldEvent, ProtocolError> {
    let event_id = event_id_for(action.action_id);
    if session.recent_events.iter().any(|e| e.event_id == event_id) {
        return Err(ProtocolError::new(
            ErrorCode::DuplicateAction,
            "action_id was already applied",
        ));
    }

    let target = action.target.clone();
    let (event_type, payload) = match action.action_type {
        ActionType::Interact => {
            // `validate` guarantees a target for interact.
            let flag = format!("interacted:{}", target.as_deref().unwrap_or_default());
            let first = session.world_flags.insert(flag, true).is_none();
            (
                WorldEventType::InteractionAcknowledged,
                json!({ "actor_id": action.actor_id, "first_interaction": first }),
            )
        }
        ActionType::Inspect => (
            WorldEventType::InspectionAcknowledged,
            json!({ "actor_id": action.actor_id }),
        ),
        ActionType::Move => {
            let from = session
                .current_location
                .replace(target.clone().unwrap_or_default());
            (
                WorldEventType::LocationChanged,
                json!({ "actor_id": action.actor_id, "from": from, "to": target }),
            )
        }
        ActionType::Speak => (
            WorldEventType::SpeechAcknowledged,
            json!({ "actor_id": action.actor_id, "content": action.content }),
        ),
    };

    let event = WorldEvent {
        event_id,
        session_id: session.session_id,
        sequence: session.event_count + 1,
        event_type,
        target,
        payload,
        timestamp: now,
    };
    session.record(event.clone());
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(session_id: Uuid, action_type: &str, target: Option<&str>) -> PlayerAction {
        PlayerAction {
            protocol_version: 1,
            action_id: Uuid::new_v4(),
            session_id,
            actor_id: "player".into(),
            action_type: action_type.into(),
            target: target.map(Into::into),
            content: None,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn deserializes_player_action() {
        let json = r#"{
            "protocol_version": 1,
            "action_id": "6f1c1b64-3f4b-4c1f-9a43-3f1a3d0a8b11",
            "session_id": "0b5e0f53-0a52-4c8f-8f0e-9d0a1b2c3d4e",
            "actor_id": "player",
            "action_type": "interact",
            "target": "test_door",
            "timestamp": "2026-10-03T12:00:00Z"
        }"#;
        let a: PlayerAction = serde_json::from_str(json).unwrap();
        assert_eq!(a.target.as_deref(), Some("test_door"));
        assert_eq!(a.content, None);
    }

    #[test]
    fn accepts_valid_interact() {
        let sid = Uuid::new_v4();
        let v = validate(&action(sid, "interact", Some("test_door")), Some(sid)).unwrap();
        assert_eq!(v.action_type, ActionType::Interact);
    }

    #[test]
    fn rejects_unknown_action_type() {
        let sid = Uuid::new_v4();
        let err = validate(&action(sid, "fly", Some("x")), Some(sid)).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidAction);
    }

    #[test]
    fn rejects_missing_target() {
        let sid = Uuid::new_v4();
        let err = validate(&action(sid, "interact", None), Some(sid)).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidAction);
    }

    #[test]
    fn rejects_bad_identifiers() {
        let sid = Uuid::new_v4();
        let err = validate(&action(sid, "interact", Some("door; DROP")), Some(sid)).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidAction);
        let mut a = action(sid, "interact", Some("door"));
        a.actor_id = String::new();
        assert_eq!(
            validate(&a, Some(sid)).unwrap_err().code,
            ErrorCode::InvalidAction
        );
    }

    #[test]
    fn speak_requires_bounded_content() {
        let sid = Uuid::new_v4();
        let mut a = action(sid, "speak", None);
        assert_eq!(
            validate(&a, Some(sid)).unwrap_err().code,
            ErrorCode::InvalidAction
        );
        a.content = Some("x".repeat(MAX_CONTENT_LEN + 1));
        assert_eq!(
            validate(&a, Some(sid)).unwrap_err().code,
            ErrorCode::InvalidAction
        );
        a.content = Some("hello there".into());
        assert!(validate(&a, Some(sid)).is_ok());
    }

    #[test]
    fn rejects_session_mismatch() {
        let sid = Uuid::new_v4();
        let a = action(sid, "interact", Some("door"));
        assert_eq!(
            validate(&a, Some(Uuid::new_v4())).unwrap_err().code,
            ErrorCode::SessionMismatch
        );
        assert_eq!(
            validate(&a, None).unwrap_err().code,
            ErrorCode::SessionMismatch
        );
    }

    #[test]
    fn rejects_action_protocol_version() {
        let sid = Uuid::new_v4();
        let mut a = action(sid, "interact", Some("door"));
        a.protocol_version = 9;
        assert_eq!(
            validate(&a, Some(sid)).unwrap_err().code,
            ErrorCode::UnsupportedProtocolVersion
        );
    }

    #[test]
    fn world_event_is_deterministic() {
        let sid = Uuid::new_v4();
        let now = Utc::now();
        let v = validate(&action(sid, "interact", Some("test_door")), Some(sid)).unwrap();

        let mut s1 = GameSession::new(sid, now);
        let mut s2 = GameSession::new(sid, now);
        let e1 = apply(&mut s1, &v, now).unwrap();
        let e2 = apply(&mut s2, &v, now).unwrap();

        assert_eq!(e1, e2);
        assert_eq!(e1.event_id, event_id_for(v.action_id));
        assert_eq!(e1.event_type, WorldEventType::InteractionAcknowledged);
        assert_eq!(e1.target.as_deref(), Some("test_door"));
        assert_eq!(e1.sequence, 1);
        assert_eq!(e1.payload["first_interaction"], true);
        assert_eq!(s1.world_flags.get("interacted:test_door"), Some(&true));
    }

    #[test]
    fn duplicate_action_is_rejected() {
        let sid = Uuid::new_v4();
        let now = Utc::now();
        let v = validate(&action(sid, "inspect", Some("lamp")), Some(sid)).unwrap();
        let mut s = GameSession::new(sid, now);
        apply(&mut s, &v, now).unwrap();
        assert_eq!(
            apply(&mut s, &v, now).unwrap_err().code,
            ErrorCode::DuplicateAction
        );
        assert_eq!(s.event_count, 1);
    }

    #[test]
    fn move_updates_location() {
        let sid = Uuid::new_v4();
        let now = Utc::now();
        let mut s = GameSession::new(sid, now);
        let v1 = validate(&action(sid, "move", Some("hallway")), Some(sid)).unwrap();
        let v2 = validate(&action(sid, "move", Some("vault")), Some(sid)).unwrap();
        apply(&mut s, &v1, now).unwrap();
        let e = apply(&mut s, &v2, now).unwrap();
        assert_eq!(s.current_location.as_deref(), Some("vault"));
        assert_eq!(e.payload["from"], "hallway");
        assert_eq!(e.payload["to"], "vault");
        assert_eq!(e.sequence, 2);
    }
}
