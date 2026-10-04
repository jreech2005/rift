//! Events the narrative layer consumes.
//!
//! A [`NarrativeEvent`] reports something that already happened in the played
//! world. The engine checks it against the current state and derives the
//! consequences; it never trusts an event that contradicts the world.

use serde::{Deserialize, Serialize};

use super::model::{
    ActorId, CharacterId, CheckpointId, FactId, FlagId, LocationId, MissionId, ObjectId,
    ObjectiveId, TruthId,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NarrativeEvent {
    FlagSet {
        flag: FlagId,
        value: bool,
    },
    /// Permanent.
    CharacterDied {
        character_id: CharacterId,
    },
    CharacterAvailabilityChanged {
        character_id: CharacterId,
        available: bool,
    },
    CharacterMoved {
        character_id: CharacterId,
        location_id: LocationId,
    },
    PlayerMoved {
        location_id: LocationId,
    },
    /// Permanent: the location can no longer be used.
    LocationLost {
        location_id: LocationId,
    },
    /// Permanent.
    ObjectDestroyed {
        object_id: ObjectId,
    },
    /// Permanent: `to` now knows the fact.
    FactRevealed {
        fact_id: FactId,
        to: ActorId,
    },
    /// Absolute values, as reported by whoever owns relationships.
    RelationshipChanged {
        from: ActorId,
        to: ActorId,
        trust: i32,
        fear: i32,
        affinity: i32,
    },
    /// Permanent.
    TruthEnded {
        truth_id: TruthId,
    },
    TruthEstablished {
        truth_id: TruthId,
        statement: String,
    },
    /// Gameplay reports an active objective as done.
    ObjectiveCompleted {
        objective_id: ObjectiveId,
    },
    ObjectiveFailed {
        objective_id: ObjectiveId,
    },
    /// The premise of an active mission is reported gone. Permanent; like
    /// every invalidation it fires no effects.
    MissionInvalidated {
        mission_id: MissionId,
    },
    /// A planned beat happened. Rejected unless its prerequisites hold.
    CheckpointReached {
        checkpoint_id: CheckpointId,
    },
    /// A planned beat did not, and will not, happen.
    CheckpointSkipped {
        checkpoint_id: CheckpointId,
    },
}
