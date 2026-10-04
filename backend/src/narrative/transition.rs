//! What one engine step produced.

use serde::{Deserialize, Serialize};

use super::condition::{Condition, LostDependency};
use super::model::{
    CanonPolicy, CheckpointId, CheckpointStatus, Effect, MissionId, MissionStatus, NarrativeState,
    ObjectiveId, ObjectiveStatus,
};
use super::replan::ReplanRequest;

/// Why a mission or objective ended without being completed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Cause {
    /// An essential prerequisite is permanently broken.
    PrerequisiteBroken { lost: Vec<LostDependency> },
    /// Its completion condition can never come true.
    CompletionImpossible { lost: Vec<LostDependency> },
    /// Its fail condition came true.
    FailConditionMet { condition: Condition },
    /// Gameplay reported the failure.
    Reported,
    /// A required objective failed.
    ObjectiveFailed { objective_id: ObjectiveId },
    /// A required objective became impossible.
    ObjectiveInvalidated { objective_id: ObjectiveId },
    /// The mission it belongs to ended.
    MissionEnded { mission_id: MissionId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionChange {
    pub mission_id: MissionId,
    pub from: MissionStatus,
    pub to: MissionStatus,
    /// Set for `failed` and `invalidated`.
    pub cause: Option<Cause>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectiveChange {
    pub mission_id: MissionId,
    pub objective_id: ObjectiveId,
    pub from: ObjectiveStatus,
    pub to: ObjectiveStatus,
    /// Set for `failed` and `invalidated`.
    pub cause: Option<Cause>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointChange {
    pub checkpoint_id: CheckpointId,
    pub from_status: CheckpointStatus,
    pub to_status: CheckpointStatus,
    pub from_policy: CanonPolicy,
    pub to_policy: CanonPolicy,
    /// What the beat lost, when its policy changed.
    pub lost: Vec<LostDependency>,
}

/// The result of one engine step. `state` replaces the previous state; the
/// rest describes what changed, in the order it happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NarrativeTransition {
    pub state: NarrativeState,
    pub objective_changes: Vec<ObjectiveChange>,
    pub mission_changes: Vec<MissionChange>,
    pub checkpoint_changes: Vec<CheckpointChange>,
    /// Plan effects that fired and were applied to the world.
    pub effects: Vec<Effect>,
    /// Present when the plan can no longer proceed as written.
    pub replan: Option<ReplanRequest>,
}
