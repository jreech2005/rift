//! The Director boundary: typed data describing why and how to replan.
//!
//! The engine never invents a replacement character, object or mission. It
//! reports what was lost and what is still available, and a later Director
//! proposes something that fits (validated by
//! [`NarrativeEngine::adopt_mission`]).
//!
//! [`NarrativeEngine::adopt_mission`]: super::NarrativeEngine::adopt_mission

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::canon::BeatAssessment;
use super::condition::LostDependency;
use super::event::NarrativeEvent;
use super::model::{
    CharacterId, Effect, FlagId, LocationId, MissionId, MissionStatus, NarrativeState, ObjectId,
    TruthId,
};
use super::transition::{MissionChange, ObjectiveChange};
use super::world::CharacterStatus;

/// The most severe thing that happened, in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplanReason {
    MissionInvalidated,
    MissionFailed,
    ObjectiveInvalidated,
    ObjectiveFailed,
    /// Missions are intact but a planned beat can no longer be preserved.
    CanonDivergence,
}

/// The world changes behind the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldChanges {
    /// The event that set it off; `None` when the plan was just started or
    /// extended.
    pub trigger: Option<NarrativeEvent>,
    /// Plan effects that fired as a consequence.
    pub effects: Vec<Effect>,
}

/// What a replacement may still build on. Anything not listed here must not be
/// assumed: in particular `dead_characters` stay dead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemainingContext {
    pub available_characters: Vec<CharacterId>,
    /// Alive but out of action.
    pub unavailable_characters: Vec<CharacterId>,
    pub dead_characters: Vec<CharacterId>,
    pub available_locations: Vec<LocationId>,
    pub lost_locations: Vec<LocationId>,
    pub intact_objects: Vec<ObjectId>,
    pub destroyed_objects: Vec<ObjectId>,
    pub holding_truths: Vec<TruthId>,
    pub ended_truths: Vec<TruthId>,
    pub flags: BTreeMap<FlagId, bool>,
    pub player_location: Option<LocationId>,
    pub active_missions: Vec<MissionId>,
    pub completed_missions: Vec<MissionId>,
}

impl RemainingContext {
    pub(crate) fn of(state: &NarrativeState) -> Self {
        let world = &state.world;
        let characters = |status: CharacterStatus| {
            world
                .characters
                .iter()
                .filter(|(_, c)| c.status == status)
                .map(|(id, _)| id.clone())
                .collect()
        };
        let truths = |holds: bool| {
            world
                .truths
                .iter()
                .filter(|(_, t)| t.holds == holds)
                .map(|(id, _)| id.clone())
                .collect()
        };
        let missions = |status: MissionStatus| {
            state
                .plan
                .missions
                .iter()
                .filter(|m| m.status == status)
                .map(|m| m.mission_id.clone())
                .collect()
        };
        Self {
            available_characters: characters(CharacterStatus::Available),
            unavailable_characters: characters(CharacterStatus::Unavailable),
            dead_characters: characters(CharacterStatus::Dead),
            available_locations: world
                .locations
                .difference(&world.lost_locations)
                .cloned()
                .collect(),
            lost_locations: world.lost_locations.iter().cloned().collect(),
            intact_objects: world
                .objects
                .difference(&world.destroyed_objects)
                .cloned()
                .collect(),
            destroyed_objects: world.destroyed_objects.iter().cloned().collect(),
            holding_truths: truths(true),
            ended_truths: truths(false),
            flags: world.flags.clone(),
            player_location: world.player_location.clone(),
            active_missions: missions(MissionStatus::Active),
            completed_missions: missions(MissionStatus::Completed),
        }
    }
}

/// Sent to the Director when the plan cannot proceed as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplanRequest {
    pub schema_version: u32,
    /// The state revision this request describes.
    pub revision: u64,
    pub reason: ReplanReason,
    /// Missions that failed or were invalidated in this step.
    pub invalidated_missions: Vec<MissionChange>,
    /// Objectives that failed or were invalidated in this step.
    pub invalidated_objectives: Vec<ObjectiveChange>,
    /// Every dependency the plan lost in this step, without duplicates.
    pub lost_prerequisites: Vec<LostDependency>,
    pub world_changes: WorldChanges,
    /// Every beat still ahead, re-evaluated, plus the ones dropped in this
    /// step. Plan order.
    pub beats: Vec<BeatAssessment>,
    pub remaining_context: RemainingContext,
}
