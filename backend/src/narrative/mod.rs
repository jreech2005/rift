//! Narrative causality V1: the deterministic story layer.
//!
//! It answers three questions from typed state, with no LLM, clock or I/O:
//! what is supposed to happen now, what does that depend on, and what happens
//! when the player breaks those dependencies.
//!
//! * [`NarrativePlan`] is the *reference* timeline: missions and planned beats
//!   ([`NarrativeCheckpoint`]), some of them canon.
//! * [`WorldFacts`] is the *actual* timeline: what is true in the played
//!   world, including [`WorldTruth`]s that hold whether or not any beat about
//!   them ever happens.
//! * [`NarrativeEngine::apply_event`] moves the state forward and reports the
//!   consequences. When the plan can no longer proceed it emits a typed
//!   [`ReplanRequest`] for the Director; it never invents a replacement.
//!
//! Canon is never forced: a beat is preserved while its prerequisites remain
//! valid, not because the source material says it happened.
//!
//! Nothing here is wired into the WebSocket path yet. See `docs/NARRATIVE.md`.

mod builder;
mod canon;
mod condition;
mod engine;
mod error;
mod event;
mod model;
mod replan;
mod transition;
mod validate;
mod world;

pub use builder::{
    OPENING_CHECKPOINT_ID, OPENING_MISSION_ID, OPENING_REACH_OBJECTIVE_ID,
    OPENING_RESOLVE_OBJECTIVE_ID, SeedCharacter, SeedClaim, SeedConflict, SeedLocation,
    SeedOpeningConflict, SeedStartingLocation, SeedUniverse, WorldBibleSeed, build_initial,
    opening_choice_flag,
};
pub use canon::{BeatAssessment, REPLACE_MIN_GRAVITY, assess_checkpoint, gravity};
pub use condition::{
    Condition, Evaluation, LostDependency, Necessity, Prerequisite, PrerequisiteStatus, Verdict,
    assess, evaluate,
};
pub use engine::NarrativeEngine;
pub use error::{IdKind, NarrativeError};
pub use event::NarrativeEvent;
pub use model::{
    ACTUAL_TIMELINE_CAP, ActorId, CanonPolicy, CanonRelation, CharacterId, CheckpointId,
    CheckpointStatus, Effect, FactId, FlagId, Importance, LocationId, Mission, MissionId,
    MissionStatus, NARRATIVE_SCHEMA_VERSION, NarrativeCheckpoint, NarrativePlan, NarrativeState,
    ObjectId, Objective, ObjectiveId, ObjectiveStatus, PLAYER_ID, PlanId, TimelineEntry, TruthId,
};
pub use replan::{RemainingContext, ReplanReason, ReplanRequest, WorldChanges};
pub use transition::{
    Cause, CheckpointChange, MissionChange, NarrativeTransition, ObjectiveChange,
};
pub use validate::{MAX_KEY_LEN, validate};
pub use world::{
    CharacterFacts, CharacterStatus, RELATIONSHIP_MAX, RELATIONSHIP_MIN, RelationshipAxis,
    RelationshipFacts, WorldFacts, WorldTruth,
};
