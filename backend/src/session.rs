//! Authoritative in-memory game sessions.
//!
//! Active gameplay state lives here, in memory, and only Rust mutates it.
//! Locks are held for short synchronous sections only — never across an
//! `.await` and never around I/O or slow external services.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::action::{self, ValidatedAction, WorldEvent};
use crate::protocol::{ErrorCode, ProtocolError};

pub type SessionId = Uuid;

/// Number of recent events retained per session.
pub const RECENT_EVENTS_CAP: usize = 32;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameSession {
    pub session_id: SessionId,
    pub created_at: DateTime<Utc>,
    pub current_location: Option<String>,
    pub world_flags: BTreeMap<String, bool>,
    pub recent_events: VecDeque<WorldEvent>,
    pub event_count: u64,
}

impl GameSession {
    pub fn new(session_id: SessionId, now: DateTime<Utc>) -> Self {
        Self {
            session_id,
            created_at: now,
            current_location: None,
            world_flags: BTreeMap::new(),
            recent_events: VecDeque::with_capacity(RECENT_EVENTS_CAP),
            event_count: 0,
        }
    }

    pub(crate) fn record(&mut self, event: WorldEvent) {
        if self.recent_events.len() == RECENT_EVENTS_CAP {
            self.recent_events.pop_front();
        }
        self.recent_events.push_back(event);
        self.event_count += 1;
    }
}

/// Concurrent in-memory session store. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct SessionStore {
    inner: Arc<RwLock<HashMap<SessionId, GameSession>>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new session and return a snapshot of it.
    pub fn create(&self) -> GameSession {
        let session = GameSession::new(Uuid::new_v4(), Utc::now());
        self.inner
            .write()
            .expect("session store lock poisoned")
            .insert(session.session_id, session.clone());
        session
    }

    /// Snapshot of a session, if it exists.
    pub fn get(&self, id: SessionId) -> Option<GameSession> {
        self.inner
            .read()
            .expect("session store lock poisoned")
            .get(&id)
            .cloned()
    }

    pub fn len(&self) -> usize {
        self.inner
            .read()
            .expect("session store lock poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Commit an updated snapshot of an existing session. Returns `false`,
    /// storing nothing, if the session no longer exists.
    ///
    /// For callers that compute a change on a snapshot and commit it whole
    /// (`runtime`); they are responsible for serialising their own writers.
    pub fn replace(&self, session: GameSession) -> bool {
        let mut sessions = self.inner.write().expect("session store lock poisoned");
        match sessions.get_mut(&session.session_id) {
            Some(slot) => {
                *slot = session;
                true
            }
            None => false,
        }
    }

    /// Apply a validated action to its session, returning the world event.
    pub fn apply_action(&self, action: &ValidatedAction) -> Result<WorldEvent, ProtocolError> {
        let mut sessions = self.inner.write().expect("session store lock poisoned");
        let session = sessions.get_mut(&action.session_id).ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::SessionNotFound,
                format!("session {} does not exist", action.session_id),
            )
        })?;
        action::apply(session, action, Utc::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::ActionType;

    fn validated(session_id: Uuid) -> ValidatedAction {
        ValidatedAction {
            action_id: Uuid::new_v4(),
            session_id,
            actor_id: "player".into(),
            action_type: ActionType::Inspect,
            target: Some("lamp".into()),
            content: None,
        }
    }

    #[test]
    fn create_stores_real_session() {
        let store = SessionStore::new();
        assert!(store.is_empty());
        let s = store.create();
        assert_eq!(store.len(), 1);
        let stored = store.get(s.session_id).unwrap();
        assert_eq!(stored, s);
        assert!(stored.recent_events.is_empty());
        assert_eq!(stored.current_location, None);
    }

    #[test]
    fn sessions_are_distinct() {
        let store = SessionStore::new();
        let a = store.create();
        let b = store.create();
        assert_ne!(a.session_id, b.session_id);
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn apply_to_unknown_session_fails() {
        let store = SessionStore::new();
        let err = store.apply_action(&validated(Uuid::new_v4())).unwrap_err();
        assert_eq!(err.code, ErrorCode::SessionNotFound);
    }

    #[test]
    fn apply_mutates_stored_session() {
        let store = SessionStore::new();
        let s = store.create();
        let event = store.apply_action(&validated(s.session_id)).unwrap();
        let stored = store.get(s.session_id).unwrap();
        assert_eq!(stored.event_count, 1);
        assert_eq!(stored.recent_events.back(), Some(&event));
    }

    #[test]
    fn recent_events_are_bounded() {
        let now = Utc::now();
        let mut s = GameSession::new(Uuid::new_v4(), now);
        for _ in 0..(RECENT_EVENTS_CAP + 5) {
            let v = validated(s.session_id);
            action::apply(&mut s, &v, now).unwrap();
        }
        assert_eq!(s.recent_events.len(), RECENT_EVENTS_CAP);
        assert_eq!(s.event_count, (RECENT_EVENTS_CAP + 5) as u64);
    }
}
