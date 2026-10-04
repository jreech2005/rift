//! Director Engine (Phase 2).
//!
//! Turns a bounded [`DirectorContext`] into a validated [`DirectorDecision`]:
//!
//! ```text
//! WorldBible + session snapshot + history + narrative/NPC views
//!     -> DirectorContext -> provider (Gemini | deterministic rules)
//!     -> proposal (untrusted JSON) -> Rust validation -> DirectorDecision
//! ```
//!
//! The Director only *proposes*. Nothing in this module mutates a
//! `GameSession`, emits a `WorldEvent` or touches the WebSocket path; a later
//! integration pass applies validated decisions. A provider can only return
//! data: there is no action that carries code, a script or a console command.
//!
//! See `docs/DIRECTOR.md`.

pub mod actions;
pub mod canon;
pub mod context;
pub mod decision;
pub mod engine;
pub mod fallback;
pub mod gemini;
pub mod prompt;
pub mod provider;
pub mod schema;
pub mod validate;

#[cfg(test)]
pub(crate) mod testing;

pub use actions::{DirectorAction, Disposition, WorldEventKind};
pub use canon::{CanonError, WorldBibleView};
pub use context::{
    CharacterSummary, DirectorContext, EventView, FactionSummary, LocationSummary, MissionStatus,
    MissionView, NarrativeView, NpcView, ObjectiveStatus, ObjectiveView, OpeningSummary,
    PlayerRoleSummary, PlayerTelemetry, PlayerView, Trigger, WorldSummary,
};
pub use decision::{DecisionMetadata, DirectorDecision, DirectorProposal, ReasonCode};
pub use engine::{DEFAULT_CALL_TIMEOUT, DirectorEngine, DirectorError, MAX_ATTEMPTS};
pub use fallback::FallbackDirector;
pub use gemini::{GeminiConfig, GeminiDirector, Secret};
pub use provider::{
    DirectorProvider, ProviderError, ProviderErrorKind, ProviderOutput, ProviderRequest,
    RecordedCall, RepairRequest, ScriptedProvider, ScriptedResponse,
};
pub use validate::{
    IssueCode, ValidationIssue, parse_proposal, validate_actions, validate_proposal,
};

use crate::action::MAX_IDENTIFIER_LEN;

/// Version of the `DirectorContext` and `DirectorDecision` contracts.
pub const DIRECTOR_SCHEMA_VERSION: u32 = 1;

/// The entity id of the player. Never a valid NPC id.
pub const PLAYER_ID: &str = "player";

/// World flags with this prefix belong to the deterministic `player_action`
/// rules (`action.rs`). The Director may read them but never set or clear them.
pub const RESERVED_FLAG_PREFIX: &str = "interacted:";

/// Every bound the Director enforces. Small on purpose: a decision is a nudge,
/// not a rewrite of the world.
pub mod limits {
    /// Maximum number of actions in one decision.
    pub const MAX_ACTIONS: usize = 8;
    /// Maximum length of an `action_id`.
    pub const MAX_ACTION_ID_LEN: usize = 32;
    /// Maximum length of a world flag key (often `prefix:entity_id`).
    pub const MAX_FLAG_KEY_LEN: usize = 128;
    /// Maximum length of a `universe_id`.
    pub const MAX_UNIVERSE_ID_LEN: usize = 128;

    // Text produced by the Director (player-facing or logged).
    pub const MAX_TITLE_CHARS: usize = 80;
    pub const MAX_TEXT_CHARS: usize = 300;
    pub const MAX_REASON_CHARS: usize = 200;
    pub const MAX_SUMMARY_CHARS: usize = 400;
    /// Maximum NPCs named by one `trigger_world_event`.
    pub const MAX_EVENT_NPCS: usize = 4;

    // Context size.
    pub const MAX_RECENT_EVENTS: usize = 16;
    pub const MAX_WORLD_FLAGS: usize = 64;
    pub const MAX_NPCS: usize = 12;
    pub const MAX_LOCATIONS: usize = 12;
    pub const MAX_CHARACTERS: usize = 12;
    pub const MAX_FACTIONS: usize = 6;
    pub const MAX_FACTION_MEMBERS: usize = 12;
    pub const MAX_WORLD_RULES: usize = 8;
    pub const MAX_TRAITS: usize = 4;
    pub const MAX_GOALS: usize = 3;
    pub const MAX_CAPABILITIES: usize = 6;
    pub const MAX_OPENING_CHARACTERS: usize = 5;
    pub const MAX_MISSIONS: usize = 4;
    pub const MAX_OBJECTIVES: usize = 12;
    pub const MAX_PLAYER_ATTRIBUTES: usize = 16;

    // Player telemetry.
    pub const MAX_TELEMETRY_SCORE: u8 = 100;
    pub const MAX_TELEMETRY_WINDOW_SECONDS: u32 = 3600;

    // Context text.
    pub const MAX_NAME_CHARS: usize = 120;
    pub const MAX_TAG_CHARS: usize = 60;
    pub const MAX_BRIEF_CHARS: usize = 200;
    pub const MAX_CONTEXT_TEXT_CHARS: usize = 600;

    /// Hard ceiling on a serialized context, whatever its parts add up to.
    pub const MAX_CONTEXT_BYTES: usize = 48 * 1024;
}

fn is_id_charset(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

/// A protocol V1 identifier: `^[A-Za-z0-9_.:-]{1,64}$`. WorldBible entity ids
/// are a subset, so character and location ids pass unchanged.
pub fn is_identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_IDENTIFIER_LEN && is_id_charset(s)
}

/// A world flag key: the identifier charset, up to 128 chars.
pub fn is_flag_key(s: &str) -> bool {
    !s.is_empty() && s.len() <= limits::MAX_FLAG_KEY_LEN && is_id_charset(s)
}

/// An id the Director mints itself: `^[a-z0-9_]{1,max}$`.
pub fn is_snake_id(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Trim `text` and cut it to at most `max` chars on a char boundary, marking a
/// cut with `…`. Control characters (including newlines) become spaces.
pub fn truncate_chars(text: &str, max: usize) -> String {
    let clean: String = text
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if clean.chars().count() <= max {
        return clean;
    }
    let mut cut: String = clean.chars().take(max.saturating_sub(1)).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_rules() {
        assert!(is_identifier("hank_schrader"));
        assert!(is_identifier("interacted:test_door"));
        assert!(!is_identifier(""));
        assert!(!is_identifier("door; DROP"));
        assert!(!is_identifier(&"x".repeat(65)));

        assert!(is_flag_key(&format!("interacted:{}", "x".repeat(64))));
        assert!(!is_flag_key(&"x".repeat(129)));

        assert!(is_snake_id("a1", limits::MAX_ACTION_ID_LEN));
        assert!(!is_snake_id("A1", limits::MAX_ACTION_ID_LEN));
        assert!(!is_snake_id("a-1", limits::MAX_ACTION_ID_LEN));
        assert!(!is_snake_id(&"a".repeat(33), limits::MAX_ACTION_ID_LEN));
    }

    #[test]
    fn truncation_is_bounded_and_char_safe() {
        assert_eq!(truncate_chars("  short  ", 10), "short");
        let cut = truncate_chars("héllo wörld, this is long", 8);
        assert_eq!(cut.chars().count(), 8);
        assert!(cut.ends_with('…'));
        assert_eq!(truncate_chars("line\none", 20), "line one");
        assert_eq!(truncate_chars("exactly10!", 10), "exactly10!");
    }
}
