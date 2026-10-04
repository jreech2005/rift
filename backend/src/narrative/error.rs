//! Errors raised by the narrative layer.

use std::fmt;

use super::condition::LostDependency;

/// What an id refers to, for error reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdKind {
    Plan,
    Universe,
    Mission,
    Objective,
    Checkpoint,
    Character,
    Location,
    Object,
    Fact,
    Flag,
    Truth,
    Actor,
}

impl fmt::Display for IdKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Plan => "plan",
            Self::Universe => "universe",
            Self::Mission => "mission",
            Self::Objective => "objective",
            Self::Checkpoint => "checkpoint",
            Self::Character => "character",
            Self::Location => "location",
            Self::Object => "object",
            Self::Fact => "fact",
            Self::Flag => "flag",
            Self::Truth => "truth",
            Self::Actor => "actor",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NarrativeError {
    #[error("narrative schema_version {0} is not supported")]
    UnsupportedSchemaVersion(u32),

    #[error("invalid {kind} id {id:?}: {reason}")]
    InvalidIdentifier {
        kind: IdKind,
        id: String,
        reason: &'static str,
    },

    #[error("duplicate {kind} id {id:?}")]
    DuplicateId { kind: IdKind, id: String },

    #[error("unknown {kind} id {id:?}")]
    UnknownId { kind: IdKind, id: String },

    #[error("dependency cycle: {}", .path.join(" -> "))]
    DependencyCycle { path: Vec<String> },

    #[error("invalid plan: {0}")]
    InvalidPlan(String),

    /// The event contradicts the current state (e.g. a dead character moving).
    #[error("invalid transition: {0}")]
    InvalidTransition(String),

    /// The prerequisites of `id` do not hold right now, though they still could.
    #[error("prerequisites of {kind} {id:?} are not met")]
    PrerequisitesNotMet { kind: IdKind, id: String },

    /// `id` depends on something the world has permanently lost.
    #[error("{kind} {id:?} is impossible in the current world")]
    Impossible {
        kind: IdKind,
        id: String,
        lost: Vec<LostDependency>,
    },
}
