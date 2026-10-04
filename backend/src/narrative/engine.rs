//! The deterministic transition engine.
//!
//! Every entry point is a pure function of its arguments: no clock, no
//! randomness, no I/O. The input state is never modified; a successful call
//! returns the next state inside a [`NarrativeTransition`], and a failed call
//! changes nothing.

use std::collections::VecDeque;

use super::canon::{BeatAssessment, assess_checkpoint, gravity};
use super::condition::{LostDependency, Verdict, assess, evaluate, push_unique};
use super::error::{IdKind, NarrativeError};
use super::event::NarrativeEvent;
use super::model::{
    ACTUAL_TIMELINE_CAP, CanonPolicy, CheckpointStatus, Effect, Mission, MissionStatus,
    NARRATIVE_SCHEMA_VERSION, NarrativePlan, NarrativeState, Objective, ObjectiveStatus,
    TimelineEntry,
};
use super::replan::{RemainingContext, ReplanReason, ReplanRequest, WorldChanges};
use super::transition::{
    Cause, CheckpointChange, MissionChange, NarrativeTransition, ObjectiveChange,
};
use super::validate::{check_id, check_key, validate};
use super::world::{CharacterStatus, RelationshipFacts, WorldFacts};

pub struct NarrativeEngine;

impl NarrativeEngine {
    /// Validate a plan against its world and bring it to rest: missions whose
    /// prerequisites hold become active, and so on.
    pub fn start(
        plan: NarrativePlan,
        world: WorldFacts,
    ) -> Result<NarrativeTransition, NarrativeError> {
        validate(&plan, &world)?;
        let mut state = NarrativeState {
            plan,
            world,
            revision: 0,
            actual_timeline: VecDeque::new(),
        };
        let mut changes = Changes::default();
        settle(&mut state, &mut changes);
        Ok(finish(state, changes, None))
    }

    /// Apply something that happened in the played world and derive every
    /// consequence for missions, objectives and planned beats.
    pub fn apply_event(
        state: &NarrativeState,
        event: &NarrativeEvent,
    ) -> Result<NarrativeTransition, NarrativeError> {
        let mut next = state.clone();
        let mut changes = Changes::default();
        apply(&mut next, event, &mut changes)?;
        settle(&mut next, &mut changes);

        next.revision += 1;
        if next.actual_timeline.len() == ACTUAL_TIMELINE_CAP {
            next.actual_timeline.pop_front();
        }
        next.actual_timeline.push_back(TimelineEntry {
            revision: next.revision,
            event: event.clone(),
        });
        Ok(finish(next, changes, Some(event.clone())))
    }

    /// Add a new mission to a running plan, e.g. a Director's answer to a
    /// [`ReplanRequest`]. The mission is a proposal: it is rejected unless it
    /// is well formed, fresh, and possible in the world as it is now. A
    /// mission that needs a dead character is not possible.
    pub fn adopt_mission(
        state: &NarrativeState,
        mission: Mission,
    ) -> Result<NarrativeTransition, NarrativeError> {
        if mission.status != MissionStatus::Inactive
            || mission
                .objectives
                .iter()
                .any(|o| o.status != ObjectiveStatus::Pending)
        {
            return Err(NarrativeError::InvalidPlan(format!(
                "mission {:?} must be adopted inactive with pending objectives",
                mission.mission_id
            )));
        }

        let mut next = state.clone();
        next.plan.missions.push(mission);
        validate(&next.plan, &next.world)?;

        let mission = next.plan.missions.last().expect("mission was just pushed");
        let prerequisites = assess(&mission.prerequisites, &next.plan, &next.world);
        if prerequisites.impossible {
            return Err(NarrativeError::Impossible {
                kind: IdKind::Mission,
                id: mission.mission_id.clone(),
                lost: prerequisites.lost_essential,
            });
        }
        for objective in mission.objectives.iter().filter(|o| !o.optional) {
            if let Some(lost) = impossible(objective, &next.plan, &next.world) {
                return Err(NarrativeError::Impossible {
                    kind: IdKind::Objective,
                    id: objective.objective_id.clone(),
                    lost,
                });
            }
        }

        let mut changes = Changes::default();
        settle(&mut next, &mut changes);
        next.revision += 1;
        Ok(finish(next, changes, None))
    }
}

#[derive(Default)]
struct Changes {
    objectives: Vec<ObjectiveChange>,
    missions: Vec<MissionChange>,
    checkpoints: Vec<CheckpointChange>,
    effects: Vec<Effect>,
}

fn unknown(kind: IdKind, id: &str) -> NarrativeError {
    NarrativeError::UnknownId {
        kind,
        id: id.to_owned(),
    }
}

fn rejected(message: String) -> NarrativeError {
    NarrativeError::InvalidTransition(message)
}

// ---------------------------------------------------------------------------
// Applying one event
// ---------------------------------------------------------------------------

fn living_character(world: &WorldFacts, character_id: &str) -> Result<(), NarrativeError> {
    match world.character_status(character_id) {
        None => Err(unknown(IdKind::Character, character_id)),
        Some(CharacterStatus::Dead) => Err(rejected(format!(
            "character {character_id:?} is dead and stays dead"
        ))),
        Some(_) => Ok(()),
    }
}

fn usable_location(world: &WorldFacts, location_id: &str) -> Result<(), NarrativeError> {
    if !world.locations.contains(location_id) {
        Err(unknown(IdKind::Location, location_id))
    } else if world.location_lost(location_id) {
        Err(rejected(format!("location {location_id:?} is lost")))
    } else {
        Ok(())
    }
}

fn apply(
    state: &mut NarrativeState,
    event: &NarrativeEvent,
    out: &mut Changes,
) -> Result<(), NarrativeError> {
    let world = &mut state.world;
    match event {
        NarrativeEvent::FlagSet { flag, value } => {
            check_key(IdKind::Flag, flag)?;
            world.flags.insert(flag.clone(), *value);
        }
        NarrativeEvent::CharacterDied { character_id } => {
            let character = world
                .characters
                .get_mut(character_id)
                .ok_or_else(|| unknown(IdKind::Character, character_id))?;
            character.status = CharacterStatus::Dead;
        }
        NarrativeEvent::CharacterAvailabilityChanged {
            character_id,
            available,
        } => {
            living_character(world, character_id)?;
            if let Some(character) = world.characters.get_mut(character_id) {
                character.status = if *available {
                    CharacterStatus::Available
                } else {
                    CharacterStatus::Unavailable
                };
            }
        }
        NarrativeEvent::CharacterMoved {
            character_id,
            location_id,
        } => {
            living_character(world, character_id)?;
            usable_location(world, location_id)?;
            if let Some(character) = world.characters.get_mut(character_id) {
                character.location = Some(location_id.clone());
            }
        }
        NarrativeEvent::PlayerMoved { location_id } => {
            usable_location(world, location_id)?;
            world.player_location = Some(location_id.clone());
        }
        NarrativeEvent::LocationLost { location_id } => {
            if !world.locations.contains(location_id) {
                return Err(unknown(IdKind::Location, location_id));
            }
            world.lost_locations.insert(location_id.clone());
        }
        NarrativeEvent::ObjectDestroyed { object_id } => {
            if !world.objects.contains(object_id) {
                return Err(unknown(IdKind::Object, object_id));
            }
            world.destroyed_objects.insert(object_id.clone());
        }
        NarrativeEvent::FactRevealed { fact_id, to } => {
            check_key(IdKind::Fact, fact_id)?;
            check_id(IdKind::Actor, to)?;
            if world.is_dead(to) {
                return Err(rejected(format!(
                    "{to:?} is dead and cannot learn anything"
                )));
            }
            world.reveal_fact(fact_id, to);
        }
        NarrativeEvent::RelationshipChanged {
            from,
            to,
            trust,
            fear,
            affinity,
        } => {
            check_id(IdKind::Actor, from)?;
            check_id(IdKind::Actor, to)?;
            world.set_relationship(
                from,
                to,
                RelationshipFacts {
                    trust: *trust,
                    fear: *fear,
                    affinity: *affinity,
                },
            );
        }
        NarrativeEvent::TruthEnded { truth_id } => {
            if !world.truths.contains_key(truth_id) {
                return Err(unknown(IdKind::Truth, truth_id));
            }
            world.end_truth(truth_id);
        }
        NarrativeEvent::TruthEstablished {
            truth_id,
            statement,
        } => {
            check_id(IdKind::Truth, truth_id)?;
            if world.truths.contains_key(truth_id) {
                return Err(NarrativeError::DuplicateId {
                    kind: IdKind::Truth,
                    id: truth_id.clone(),
                });
            }
            world.establish_truth(truth_id, statement);
        }

        NarrativeEvent::ObjectiveCompleted { objective_id } => {
            let (mi, oi) = reportable_objective(&state.plan, objective_id)?;
            if state.plan.missions[mi].objectives[oi].status != ObjectiveStatus::Active {
                return Err(rejected(format!(
                    "objective {objective_id:?} is not active and cannot be completed"
                )));
            }
            set_objective(state, mi, oi, ObjectiveStatus::Completed, None, out);
        }
        NarrativeEvent::ObjectiveFailed { objective_id } => {
            let (mi, oi) = reportable_objective(&state.plan, objective_id)?;
            set_objective(
                state,
                mi,
                oi,
                ObjectiveStatus::Failed,
                Some(Cause::Reported),
                out,
            );
        }

        NarrativeEvent::MissionInvalidated { mission_id } => {
            let mi = state
                .plan
                .missions
                .iter()
                .position(|m| m.mission_id == *mission_id)
                .ok_or_else(|| unknown(IdKind::Mission, mission_id))?;
            if state.plan.missions[mi].status != MissionStatus::Active {
                return Err(rejected(format!(
                    "mission {mission_id:?} is not active and cannot be invalidated"
                )));
            }
            end_mission(
                state,
                mi,
                MissionStatus::Invalidated,
                Some(Cause::Reported),
                out,
            );
        }

        NarrativeEvent::CheckpointReached { checkpoint_id } => {
            let ci = pending_checkpoint(&state.plan, checkpoint_id)?;
            let beat = assess_checkpoint(&state.plan.checkpoints[ci], &state.plan, &state.world);
            // A beat the world contradicts cannot be declared to have happened,
            // however much canon wants it.
            if matches!(beat.policy, CanonPolicy::Replace | CanonPolicy::Delete) {
                return Err(NarrativeError::Impossible {
                    kind: IdKind::Checkpoint,
                    id: checkpoint_id.clone(),
                    lost: beat.lost,
                });
            }
            if !beat.ready {
                return Err(NarrativeError::PrerequisitesNotMet {
                    kind: IdKind::Checkpoint,
                    id: checkpoint_id.clone(),
                });
            }
            set_checkpoint(
                state,
                ci,
                CheckpointStatus::Reached,
                beat.policy,
                beat.lost,
                out,
            );
        }
        NarrativeEvent::CheckpointSkipped { checkpoint_id } => {
            // Skipping removes the beat from the timeline: a skipped beat is
            // always a deleted one, whether or not it was still possible.
            let ci = pending_checkpoint(&state.plan, checkpoint_id)?;
            set_checkpoint(
                state,
                ci,
                CheckpointStatus::Skipped,
                CanonPolicy::Delete,
                Vec::new(),
                out,
            );
        }
    }
    Ok(())
}

/// Indices of an open objective in an active mission.
fn reportable_objective(
    plan: &NarrativePlan,
    objective_id: &str,
) -> Result<(usize, usize), NarrativeError> {
    let (mi, oi) = plan
        .missions
        .iter()
        .enumerate()
        .find_map(|(mi, mission)| {
            mission
                .objectives
                .iter()
                .position(|o| o.objective_id == objective_id)
                .map(|oi| (mi, oi))
        })
        .ok_or_else(|| unknown(IdKind::Objective, objective_id))?;
    let mission = &plan.missions[mi];
    if mission.status != MissionStatus::Active || mission.objectives[oi].status.is_terminal() {
        return Err(rejected(format!(
            "objective {objective_id:?} is not open in an active mission"
        )));
    }
    Ok((mi, oi))
}

fn pending_checkpoint(plan: &NarrativePlan, checkpoint_id: &str) -> Result<usize, NarrativeError> {
    let ci = plan
        .checkpoints
        .iter()
        .position(|c| c.checkpoint_id == checkpoint_id)
        .ok_or_else(|| unknown(IdKind::Checkpoint, checkpoint_id))?;
    if plan.checkpoints[ci].status != CheckpointStatus::Pending {
        return Err(rejected(format!(
            "checkpoint {checkpoint_id:?} is no longer pending"
        )));
    }
    Ok(ci)
}

// ---------------------------------------------------------------------------
// Settling: derive consequences until nothing changes
// ---------------------------------------------------------------------------

/// Statuses only move forward and every pass that continues the loop moves at
/// least one, so this terminates.
fn settle(state: &mut NarrativeState, out: &mut Changes) {
    loop {
        let mut progressed = false;
        for mi in 0..state.plan.missions.len() {
            progressed |= settle_mission(state, mi, out);
        }
        for ci in 0..state.plan.checkpoints.len() {
            progressed |= settle_checkpoint(state, ci, out);
        }
        if !progressed {
            break;
        }
    }
}

fn settle_mission(state: &mut NarrativeState, mi: usize, out: &mut Changes) -> bool {
    let status = state.plan.missions[mi].status;
    if status.is_terminal() {
        return false;
    }

    let prerequisites = assess(
        &state.plan.missions[mi].prerequisites,
        &state.plan,
        &state.world,
    );
    if prerequisites.impossible {
        let cause = Cause::PrerequisiteBroken {
            lost: prerequisites.lost_essential,
        };
        end_mission(state, mi, MissionStatus::Invalidated, Some(cause), out);
        return true;
    }

    if status == MissionStatus::Inactive {
        if !prerequisites.satisfied {
            return false;
        }
        let mission = &mut state.plan.missions[mi];
        mission.status = MissionStatus::Active;
        out.missions.push(MissionChange {
            mission_id: mission.mission_id.clone(),
            from: MissionStatus::Inactive,
            to: MissionStatus::Active,
            cause: None,
        });
        return true;
    }

    let mut progressed = false;
    for oi in 0..state.plan.missions[mi].objectives.len() {
        progressed |= settle_objective(state, mi, oi, out);
    }

    let mission = &state.plan.missions[mi];
    let required = || mission.objectives.iter().filter(|o| !o.optional);
    let ended_as = |status: ObjectiveStatus| {
        required()
            .find(|o| o.status == status)
            .map(|o| o.objective_id.clone())
    };
    let outcome = if let Some(objective_id) = ended_as(ObjectiveStatus::Failed) {
        Some((
            MissionStatus::Failed,
            Some(Cause::ObjectiveFailed { objective_id }),
        ))
    } else if let Some(objective_id) = ended_as(ObjectiveStatus::Invalidated) {
        Some((
            MissionStatus::Invalidated,
            Some(Cause::ObjectiveInvalidated { objective_id }),
        ))
    } else if required().all(|o| o.status == ObjectiveStatus::Completed) {
        Some((MissionStatus::Completed, None))
    } else {
        None
    };
    if let Some((to, cause)) = outcome {
        end_mission(state, mi, to, cause, out);
        progressed = true;
    }
    progressed
}

/// What an essential dependency of `objective` has permanently lost, if any.
fn impossible(
    objective: &Objective,
    plan: &NarrativePlan,
    world: &WorldFacts,
) -> Option<Vec<LostDependency>> {
    match decide(objective, plan, world) {
        Some((
            ObjectiveStatus::Invalidated,
            Some(Cause::PrerequisiteBroken { lost } | Cause::CompletionImpossible { lost }),
        )) => Some(lost),
        _ => None,
    }
}

/// The next status of an open objective. Failing takes precedence over
/// becoming impossible, which takes precedence over completing.
fn decide(
    objective: &Objective,
    plan: &NarrativePlan,
    world: &WorldFacts,
) -> Option<(ObjectiveStatus, Option<Cause>)> {
    if let Some(condition) = &objective.fails_when
        && evaluate(condition, plan, world).verdict == Verdict::Holds
    {
        let cause = Cause::FailConditionMet {
            condition: condition.clone(),
        };
        return Some((ObjectiveStatus::Failed, Some(cause)));
    }

    let prerequisites = assess(&objective.prerequisites, plan, world);
    if prerequisites.impossible {
        let cause = Cause::PrerequisiteBroken {
            lost: prerequisites.lost_essential,
        };
        return Some((ObjectiveStatus::Invalidated, Some(cause)));
    }

    let completion = objective
        .completes_when
        .as_ref()
        .map(|condition| evaluate(condition, plan, world));
    if let Some(completion) = &completion
        && completion.verdict == Verdict::Broken
    {
        let cause = Cause::CompletionImpossible {
            lost: completion.lost.clone(),
        };
        return Some((ObjectiveStatus::Invalidated, Some(cause)));
    }

    match objective.status {
        ObjectiveStatus::Pending if prerequisites.satisfied => {
            Some((ObjectiveStatus::Active, None))
        }
        ObjectiveStatus::Active if completion.is_some_and(|c| c.verdict == Verdict::Holds) => {
            Some((ObjectiveStatus::Completed, None))
        }
        _ => None,
    }
}

fn settle_objective(state: &mut NarrativeState, mi: usize, oi: usize, out: &mut Changes) -> bool {
    let objective = &state.plan.missions[mi].objectives[oi];
    if objective.status.is_terminal() {
        return false;
    }
    match decide(objective, &state.plan, &state.world) {
        Some((to, cause)) => {
            set_objective(state, mi, oi, to, cause, out);
            true
        }
        None => false,
    }
}

fn set_objective(
    state: &mut NarrativeState,
    mi: usize,
    oi: usize,
    to: ObjectiveStatus,
    cause: Option<Cause>,
    out: &mut Changes,
) {
    let mission = &mut state.plan.missions[mi];
    let objective = &mut mission.objectives[oi];
    out.objectives.push(ObjectiveChange {
        mission_id: mission.mission_id.clone(),
        objective_id: objective.objective_id.clone(),
        from: objective.status,
        to,
        cause,
    });
    objective.status = to;
    let effects = match to {
        ObjectiveStatus::Completed => objective.success_effects.clone(),
        ObjectiveStatus::Failed => objective.failure_effects.clone(),
        _ => Vec::new(),
    };
    apply_effects(&mut state.world, effects, out);
}

/// End a mission. Its open objectives no longer matter and are invalidated.
/// An invalidated mission fires no effects: its premise is gone.
fn end_mission(
    state: &mut NarrativeState,
    mi: usize,
    to: MissionStatus,
    cause: Option<Cause>,
    out: &mut Changes,
) {
    let mission = &mut state.plan.missions[mi];
    out.missions.push(MissionChange {
        mission_id: mission.mission_id.clone(),
        from: mission.status,
        to,
        cause,
    });
    mission.status = to;
    for objective in &mut mission.objectives {
        if !objective.status.is_terminal() {
            out.objectives.push(ObjectiveChange {
                mission_id: mission.mission_id.clone(),
                objective_id: objective.objective_id.clone(),
                from: objective.status,
                to: ObjectiveStatus::Invalidated,
                cause: Some(Cause::MissionEnded {
                    mission_id: mission.mission_id.clone(),
                }),
            });
            objective.status = ObjectiveStatus::Invalidated;
        }
    }
    let effects = match to {
        MissionStatus::Completed => mission.success_effects.clone(),
        MissionStatus::Failed => mission.failure_effects.clone(),
        _ => Vec::new(),
    };
    apply_effects(&mut state.world, effects, out);
}

fn apply_effects(world: &mut WorldFacts, effects: Vec<Effect>, out: &mut Changes) {
    for effect in effects {
        match &effect {
            Effect::SetFlag { flag, value } => {
                world.flags.insert(flag.clone(), *value);
            }
            Effect::RevealFact { fact_id, to } => world.reveal_fact(fact_id, to),
            Effect::AdjustRelationship {
                from,
                to,
                axis,
                delta,
            } => world.adjust_relationship(from, to, *axis, *delta),
            Effect::EstablishTruth {
                truth_id,
                statement,
            } => world.establish_truth(truth_id, statement),
            Effect::EndTruth { truth_id } => world.end_truth(truth_id),
        }
        out.effects.push(effect);
    }
}

/// Re-classify one pending beat against the world. A deleted beat is skipped;
/// a beat with a reach condition is reached once that condition and its
/// prerequisites hold.
fn settle_checkpoint(state: &mut NarrativeState, ci: usize, out: &mut Changes) -> bool {
    let checkpoint = &state.plan.checkpoints[ci];
    if checkpoint.status != CheckpointStatus::Pending {
        return false;
    }
    let beat = assess_checkpoint(checkpoint, &state.plan, &state.world);
    let status = if beat.policy == CanonPolicy::Delete {
        CheckpointStatus::Skipped
    } else if beat.ready
        && checkpoint
            .reached_when
            .as_ref()
            .is_some_and(|c| evaluate(c, &state.plan, &state.world).verdict == Verdict::Holds)
    {
        CheckpointStatus::Reached
    } else {
        CheckpointStatus::Pending
    };
    if status == checkpoint.status && beat.policy == checkpoint.policy {
        return false;
    }
    set_checkpoint(state, ci, status, beat.policy, beat.lost, out);
    true
}

fn set_checkpoint(
    state: &mut NarrativeState,
    ci: usize,
    status: CheckpointStatus,
    policy: CanonPolicy,
    lost: Vec<LostDependency>,
    out: &mut Changes,
) {
    let checkpoint = &mut state.plan.checkpoints[ci];
    out.checkpoints.push(CheckpointChange {
        checkpoint_id: checkpoint.checkpoint_id.clone(),
        from_status: checkpoint.status,
        to_status: status,
        from_policy: checkpoint.policy,
        to_policy: policy,
        lost,
    });
    checkpoint.status = status;
    checkpoint.policy = policy;
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

fn finish(
    state: NarrativeState,
    changes: Changes,
    trigger: Option<NarrativeEvent>,
) -> NarrativeTransition {
    let replan = replan_request(&state, &changes, trigger);
    NarrativeTransition {
        state,
        objective_changes: changes.objectives,
        mission_changes: changes.missions,
        checkpoint_changes: changes.checkpoints,
        effects: changes.effects,
        replan,
    }
}

fn replan_request(
    state: &NarrativeState,
    changes: &Changes,
    trigger: Option<NarrativeEvent>,
) -> Option<ReplanRequest> {
    let missions: Vec<MissionChange> = changes
        .missions
        .iter()
        .filter(|c| matches!(c.to, MissionStatus::Failed | MissionStatus::Invalidated))
        .cloned()
        .collect();

    // Objectives voided because their mission *completed* are not a problem.
    let completed = |mission_id: &str| {
        state
            .plan
            .mission(mission_id)
            .is_some_and(|m| m.status == MissionStatus::Completed)
    };
    let objectives: Vec<ObjectiveChange> = changes
        .objectives
        .iter()
        .filter(|c| matches!(c.to, ObjectiveStatus::Failed | ObjectiveStatus::Invalidated))
        .filter(|c| !completed(&c.mission_id))
        .cloned()
        .collect();

    let diverged = changes
        .checkpoints
        .iter()
        .any(|c| c.to_policy != c.from_policy);

    let reason = if missions.iter().any(|c| c.to == MissionStatus::Invalidated) {
        ReplanReason::MissionInvalidated
    } else if !missions.is_empty() {
        ReplanReason::MissionFailed
    } else if objectives
        .iter()
        .any(|c| c.to == ObjectiveStatus::Invalidated)
    {
        ReplanReason::ObjectiveInvalidated
    } else if !objectives.is_empty() {
        ReplanReason::ObjectiveFailed
    } else if diverged {
        ReplanReason::CanonDivergence
    } else {
        return None;
    };

    let mut lost_prerequisites = Vec::new();
    let causes = missions
        .iter()
        .map(|c| &c.cause)
        .chain(objectives.iter().map(|c| &c.cause));
    for cause in causes {
        if let Some(Cause::PrerequisiteBroken { lost } | Cause::CompletionImpossible { lost }) =
            cause
        {
            push_unique(&mut lost_prerequisites, lost.clone());
        }
    }
    for change in &changes.checkpoints {
        push_unique(&mut lost_prerequisites, change.lost.clone());
    }

    // Beats still ahead, plus the ones this step dropped.
    let beats = state
        .plan
        .checkpoints
        .iter()
        .filter_map(|checkpoint| match checkpoint.status {
            CheckpointStatus::Pending => {
                Some(assess_checkpoint(checkpoint, &state.plan, &state.world))
            }
            CheckpointStatus::Skipped => changes
                .checkpoints
                .iter()
                .rfind(|c| c.checkpoint_id == checkpoint.checkpoint_id)
                .map(|change| BeatAssessment {
                    checkpoint_id: checkpoint.checkpoint_id.clone(),
                    policy: change.to_policy,
                    gravity: gravity(checkpoint.importance, checkpoint.canon_relation),
                    ready: false,
                    lost: change.lost.clone(),
                }),
            CheckpointStatus::Reached => None,
        })
        .collect();

    Some(ReplanRequest {
        schema_version: NARRATIVE_SCHEMA_VERSION,
        revision: state.revision,
        reason,
        invalidated_missions: missions,
        invalidated_objectives: objectives,
        lost_prerequisites,
        world_changes: WorldChanges {
            trigger,
            effects: changes.effects.clone(),
        },
        beats,
        remaining_context: RemainingContext::of(state),
    })
}
