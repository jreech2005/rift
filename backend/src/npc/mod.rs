//! NPC state and memory V1.
//!
//! ```text
//! WorldBible character ──► CharacterState ◄── NpcDirectory (authoritative, in memory)
//!                                ▲
//!        NpcEvent ──► perceive() ┤  (only the event's audience)
//!                                ▼
//!                          MemoryEntry ──► MemoryStore (in-memory | TiDB)
//!                                               │ bounded, ranked retrieval
//!                                               ▼
//!                                          NpcContext
//! ```
//!
//! Ground rules (see `docs/NPC_MEMORY.md`):
//!
//! * **No omniscience.** An NPC knows a fact or flag only if it was taught to
//!   that NPC. Nothing in this module reads `GameSession`.
//! * **No broadcast.** An event reaches only its [`events::Audience`].
//! * **Deterministic.** No clock, randomness or LLM: callers pass time in.
//! * **Bounded.** Every collection and every retrieval has a hard cap.
//!
//! Nothing here is wired into the WebSocket path yet; a later integration
//! layer builds [`NpcEvent`]s from validated world events and calls
//! [`NpcDirectory::record_event`] and [`build_context`].

pub mod context;
pub mod directory;
pub mod events;
pub mod ids;
pub mod memory;
pub mod relationship;
pub mod state;
pub mod store;
pub mod tidb;

use uuid::Uuid;

pub use context::{NpcContext, build_context};
pub use directory::NpcDirectory;
pub use events::{Audience, FlagChange, NpcEvent, NpcEventKind, Perception, StateEffect, perceive};
pub use ids::{CharacterId, EntityId, FactId, FlagKey, LocationId};
pub use memory::{MemoryEntry, MemoryQuery, MemoryType, ScoredMemory};
pub use relationship::{Disposition, Relationship, RelationshipDelta};
pub use state::{CharacterState, Fact, KnowledgeSource, KnownFact, LifeStatus};
pub use store::{InMemoryMemoryStore, MemoryStore, StoreError};

/// Maximum length, in chars, of any free-text field (summaries, statements,
/// goals). Same bound as protocol V1 `content`.
pub const MAX_TEXT_LEN: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NpcError {
    #[error("invalid {kind}: {reason}")]
    InvalidIdentifier { kind: &'static str, reason: String },
    #[error("invalid {field}: {reason}")]
    InvalidText { field: &'static str, reason: String },
    #[error("unknown character {0}")]
    UnknownCharacter(String),
    #[error("no NPCs registered for session {0}")]
    UnknownSession(Uuid),
    #[error("character {0} already exists in this session")]
    DuplicateCharacter(String),
    #[error("event {0} was already applied")]
    DuplicateEvent(Uuid),
    #[error("character {0} is dead")]
    CharacterDead(String),
    #[error("{character} does not know fact {fact}")]
    UnknownFact { character: String, fact: String },
    #[error("invalid event: {0}")]
    InvalidEvent(String),
    #[error("invalid query: {0}")]
    InvalidQuery(String),
    #[error("invalid world bible: {0}")]
    InvalidWorldBible(String),
    #[error("too many {what} (max {max})")]
    LimitExceeded { what: &'static str, max: usize },
    #[error("unsupported schema version {0}")]
    UnsupportedSchemaVersion(u32),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Trim `value` and require 1..=`max_chars` chars.
pub(crate) fn clean_text(
    field: &'static str,
    value: &str,
    max_chars: usize,
) -> Result<String, NpcError> {
    let value = value.trim();
    let reason = if value.is_empty() {
        "must not be empty".to_owned()
    } else if value.chars().count() > max_chars {
        format!("must be at most {max_chars} chars")
    } else {
        return Ok(value.to_owned());
    };
    Err(NpcError::InvalidText { field, reason })
}

/// Trim `value` and cut it to at most `max_chars` chars.
pub(crate) fn truncate_text(value: &str, max_chars: usize) -> String {
    value
        .trim()
        .chars()
        .take(max_chars)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

#[cfg(test)]
mod tests;
