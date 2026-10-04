//! Narrative plan V1: missions, objectives and planned beats.
//!
//! The plan is the *reference* timeline: what is supposed to happen. What is
//! actually true lives in [`WorldFacts`].

use std::cmp::Reverse;
use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

use super::canon::{self, BeatAssessment};
use super::condition::{Condition, Prerequisite};
use super::event::NarrativeEvent;
use super::world::{RelationshipAxis, WorldFacts};

pub const NARRATIVE_SCHEMA_VERSION: u32 = 1;

/// Number of applied events kept in [`NarrativeState::actual_timeline`].
pub const ACTUAL_TIMELINE_CAP: usize = 64;

/// The entity id of the player. Never a character id.
pub const PLAYER_ID: &str = "player";

// Ids use the protocol V1 identifier charset, so a WorldBible entity id or a
// protocol `target` is always a valid narrative id.
pub type PlanId = String;
pub type MissionId = String;
pub type ObjectiveId = String;
pub type CheckpointId = String;
pub type CharacterId = String;
pub type LocationId = String;
pub type ObjectId = String;
pub type TruthId = String;
pub type FactId = String;
pub type FlagId = String;
/// A character id, a faction id or [`PLAYER_ID`].
pub type ActorId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionStatus {
    #[default]
    Inactive,
    Active,
    Completed,
    Failed,
    /// Its premise collapsed: something it depended on is permanently gone.
    Invalidated,
}

impl MissionStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Invalidated)
    }

    fn rank(self) -> u8 {
        match self {
            Self::Inactive => 0,
            Self::Active => 1,
            _ => 2,
        }
    }

    /// Statuses only move forward, and a terminal status never changes.
    pub(crate) fn can_become(self, wanted: Self) -> bool {
        self == wanted || (!self.is_terminal() && wanted.rank() > self.rank())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectiveStatus {
    #[default]
    Pending,
    Active,
    Completed,
    Failed,
    Invalidated,
}

impl ObjectiveStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Invalidated)
    }

    fn rank(self) -> u8 {
        match self {
            Self::Pending => 0,
            Self::Active => 1,
            _ => 2,
        }
    }

    pub(crate) fn can_become(self, wanted: Self) -> bool {
        self == wanted || (!self.is_terminal() && wanted.rank() > self.rank())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointStatus {
    /// Still ahead on the timeline.
    #[default]
    Pending,
    /// It happened in the played world.
    Reached,
    /// It will not happen. Its policy is always `delete`.
    Skipped,
}

/// Where a mission, beat or truth comes from. Mirrors WorldBible
/// `classification`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonRelation {
    /// It happens in the source material.
    Canon,
    /// Deduced from the source material.
    Inferred,
    /// Invented for the game.
    Generated,
}

/// What to do with a planned beat given the world as it now is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonPolicy {
    /// Nothing it needs is lost: keep it as written.
    #[default]
    Preserve,
    /// Only flexible circumstances are lost: it can still happen, altered.
    Adapt,
    /// It cannot happen as written but its place in the story still matters:
    /// a different beat is needed.
    Replace,
    /// It cannot happen and nothing needs to take its place.
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Importance {
    Minor,
    Major,
    Critical,
}

/// A deterministic consequence of an objective or mission outcome. Effects
/// only touch reversible flags, knowledge, relationships and truths; they
/// cannot kill, create or revive anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Effect {
    SetFlag {
        flag: FlagId,
        value: bool,
    },
    RevealFact {
        fact_id: FactId,
        to: ActorId,
    },
    AdjustRelationship {
        from: ActorId,
        to: ActorId,
        axis: RelationshipAxis,
        delta: i32,
    },
    EstablishTruth {
        truth_id: TruthId,
        statement: String,
    },
    EndTruth {
        truth_id: TruthId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Objective {
    /// Unique across the whole plan.
    pub objective_id: ObjectiveId,
    pub description: String,
    #[serde(default)]
    pub status: ObjectiveStatus,
    /// Optional objectives never decide the mission outcome.
    #[serde(default)]
    pub optional: bool,
    /// Must hold to become active; an essential one breaking invalidates.
    #[serde(default)]
    pub prerequisites: Vec<Prerequisite>,
    /// Completes an active objective. `None`: only an explicit
    /// `objective_completed` event does.
    #[serde(default)]
    pub completes_when: Option<Condition>,
    /// Fails a pending or active objective.
    #[serde(default)]
    pub fails_when: Option<Condition>,
    #[serde(default)]
    pub success_effects: Vec<Effect>,
    #[serde(default)]
    pub failure_effects: Vec<Effect>,
}

impl Objective {
    pub fn new(objective_id: impl Into<ObjectiveId>, description: impl Into<String>) -> Self {
        Self {
            objective_id: objective_id.into(),
            description: description.into(),
            status: ObjectiveStatus::Pending,
            optional: false,
            prerequisites: Vec::new(),
            completes_when: None,
            fails_when: None,
            success_effects: Vec::new(),
            failure_effects: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mission {
    pub mission_id: MissionId,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub status: MissionStatus,
    pub objectives: Vec<Objective>,
    /// Must hold to activate; an essential one breaking invalidates.
    #[serde(default)]
    pub prerequisites: Vec<Prerequisite>,
    #[serde(default)]
    pub success_effects: Vec<Effect>,
    #[serde(default)]
    pub failure_effects: Vec<Effect>,
    #[serde(default)]
    pub related_characters: Vec<CharacterId>,
    #[serde(default)]
    pub related_locations: Vec<LocationId>,
    pub canon_relation: CanonRelation,
    pub importance: Importance,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl Mission {
    /// An inactive, generated, major mission with the given objectives.
    pub fn new(
        mission_id: impl Into<MissionId>,
        title: impl Into<String>,
        objectives: Vec<Objective>,
    ) -> Self {
        Self {
            mission_id: mission_id.into(),
            title: title.into(),
            description: String::new(),
            status: MissionStatus::Inactive,
            objectives,
            prerequisites: Vec::new(),
            success_effects: Vec::new(),
            failure_effects: Vec::new(),
            related_characters: Vec::new(),
            related_locations: Vec::new(),
            canon_relation: CanonRelation::Generated,
            importance: Importance::Major,
            metadata: BTreeMap::new(),
        }
    }
}

/// A planned story beat on the reference timeline: a canon event, or one
/// invented for the game. It is a plan, not a fact — see [`WorldTruth`] for
/// facts.
///
/// [`WorldTruth`]: super::WorldTruth
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NarrativeCheckpoint {
    pub checkpoint_id: CheckpointId,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub canon_relation: CanonRelation,
    pub importance: Importance,
    #[serde(default)]
    pub prerequisites: Vec<Prerequisite>,
    /// Marks the beat reached once its prerequisites hold. `None`: only an
    /// explicit `checkpoint_reached` event does.
    #[serde(default)]
    pub reached_when: Option<Condition>,
    #[serde(default)]
    pub status: CheckpointStatus,
    /// Current classification. Meaningful while the beat is pending.
    #[serde(default)]
    pub policy: CanonPolicy,
}

impl NarrativeCheckpoint {
    pub fn new(
        checkpoint_id: impl Into<CheckpointId>,
        title: impl Into<String>,
        canon_relation: CanonRelation,
        importance: Importance,
    ) -> Self {
        Self {
            checkpoint_id: checkpoint_id.into(),
            title: title.into(),
            description: String::new(),
            canon_relation,
            importance,
            prerequisites: Vec::new(),
            reached_when: None,
            status: CheckpointStatus::Pending,
            policy: CanonPolicy::Preserve,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NarrativePlan {
    pub schema_version: u32,
    pub plan_id: PlanId,
    pub universe_id: String,
    pub missions: Vec<Mission>,
    /// Planned beats, in reference-timeline order.
    #[serde(default)]
    pub checkpoints: Vec<NarrativeCheckpoint>,
}

impl NarrativePlan {
    pub fn new(plan_id: impl Into<PlanId>, universe_id: impl Into<String>) -> Self {
        Self {
            schema_version: NARRATIVE_SCHEMA_VERSION,
            plan_id: plan_id.into(),
            universe_id: universe_id.into(),
            missions: Vec::new(),
            checkpoints: Vec::new(),
        }
    }

    pub fn mission(&self, mission_id: &str) -> Option<&Mission> {
        self.missions.iter().find(|m| m.mission_id == mission_id)
    }

    pub fn objective(&self, objective_id: &str) -> Option<(&Mission, &Objective)> {
        self.missions.iter().find_map(|mission| {
            mission
                .objectives
                .iter()
                .find(|o| o.objective_id == objective_id)
                .map(|objective| (mission, objective))
        })
    }

    pub fn checkpoint(&self, checkpoint_id: &str) -> Option<&NarrativeCheckpoint> {
        self.checkpoints
            .iter()
            .find(|c| c.checkpoint_id == checkpoint_id)
    }
}

/// One event that was applied to the played world.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineEntry {
    /// The state revision this event produced.
    pub revision: u64,
    pub event: NarrativeEvent,
}

/// Everything the narrative layer knows: the reference timeline (`plan`), the
/// actual world (`world`) and what recently happened in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NarrativeState {
    pub plan: NarrativePlan,
    pub world: WorldFacts,
    /// Increases by one with every applied change.
    pub revision: u64,
    /// The most recent events, oldest first, capped at
    /// [`ACTUAL_TIMELINE_CAP`].
    #[serde(default)]
    pub actual_timeline: VecDeque<TimelineEntry>,
}

impl NarrativeState {
    pub fn active_missions(&self) -> Vec<&Mission> {
        self.plan
            .missions
            .iter()
            .filter(|m| m.status == MissionStatus::Active)
            .collect()
    }

    /// What the player is currently supposed to be doing.
    pub fn active_objectives(&self) -> Vec<(&Mission, &Objective)> {
        self.active_missions()
            .into_iter()
            .flat_map(|mission| {
                mission
                    .objectives
                    .iter()
                    .filter(|o| o.status == ObjectiveStatus::Active)
                    .map(move |objective| (mission, objective))
            })
            .collect()
    }

    /// Current classification of every pending beat, in plan order.
    pub fn assess_checkpoints(&self) -> Vec<BeatAssessment> {
        self.plan
            .checkpoints
            .iter()
            .filter(|c| c.status == CheckpointStatus::Pending)
            .map(|c| canon::assess_checkpoint(c, &self.plan, &self.world))
            .collect()
    }

    /// Pending beats whose prerequisites all hold right now, strongest canon
    /// gravity first, then plan order.
    pub fn ready_checkpoints(&self) -> Vec<BeatAssessment> {
        let mut ready: Vec<BeatAssessment> = self
            .assess_checkpoints()
            .into_iter()
            .filter(|beat| beat.ready)
            .collect();
        ready.sort_by_key(|beat| Reverse(beat.gravity));
        ready
    }

    /// Canon and inferred beats that will not happen as the source tells them.
    pub fn divergences(&self) -> Vec<&NarrativeCheckpoint> {
        self.plan
            .checkpoints
            .iter()
            .filter(|c| c.canon_relation != CanonRelation::Generated)
            .filter(|c| c.policy != CanonPolicy::Preserve)
            .collect()
    }
}
