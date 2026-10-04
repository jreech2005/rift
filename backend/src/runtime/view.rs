//! Authoritative state -> the Director's bounded DTOs.
//!
//! The Director never sees NPC or narrative internals, only these views. In
//! particular an [`NpcView`] carries no knowledge, facts or memories: what an
//! NPC privately knows stays in the NPC layer.

use std::collections::BTreeSet;

use crate::director::limits;
use crate::director::{
    DirectorContext, Disposition as DirectorDisposition, MissionStatus as ViewMissionStatus,
    MissionView, NarrativeView, NpcView, ObjectiveStatus as ViewObjectiveStatus, ObjectiveView,
    Trigger, truncate_chars,
};
use crate::narrative::{
    CanonPolicy, CharacterStatus, Mission, MissionStatus, NarrativeState, Objective,
    ObjectiveStatus,
};
use crate::npc::{CharacterState, Disposition, LifeStatus};
use crate::session::GameSession;

use super::world::RuntimeWorld;

/// Metadata key marking the one-objective missions that carry a Director
/// `set_objective`. They are bookkeeping, not story arcs, so the Director is
/// shown their objective but not the wrapper.
pub(crate) const ORIGIN_KEY: &str = "origin";
pub(crate) const ORIGIN_DIRECTOR: &str = "director";
/// Metadata key: the mission a Director objective was attached to.
pub(crate) const PARENT_MISSION_KEY: &str = "parent_mission";
/// Metadata key: the NPC who asked for a mission's objectives.
pub(crate) const GIVER_KEY: &str = "giver_npc_id";

pub(crate) fn is_director_mission(mission: &Mission) -> bool {
    mission.metadata.get(ORIGIN_KEY).map(String::as_str) == Some(ORIGIN_DIRECTOR)
}

pub(crate) fn director_disposition(disposition: Disposition) -> DirectorDisposition {
    match disposition {
        Disposition::Hostile => DirectorDisposition::Hostile,
        Disposition::Wary => DirectorDisposition::Wary,
        Disposition::Neutral => DirectorDisposition::Neutral,
        Disposition::Friendly => DirectorDisposition::Friendly,
        Disposition::Loyal => DirectorDisposition::Loyal,
    }
}

fn text(value: &str, max: usize) -> Option<String> {
    let value = truncate_chars(value, max);
    (!value.trim().is_empty()).then_some(value)
}

fn mission_status(status: MissionStatus) -> Option<ViewMissionStatus> {
    match status {
        MissionStatus::Inactive => None,
        MissionStatus::Active => Some(ViewMissionStatus::Active),
        MissionStatus::Completed => Some(ViewMissionStatus::Completed),
        MissionStatus::Failed => Some(ViewMissionStatus::Failed),
        MissionStatus::Invalidated => Some(ViewMissionStatus::Invalidated),
    }
}

/// The Director has no "invalidated" objective: for it, an objective that can
/// no longer be done has failed.
fn objective_status(status: ObjectiveStatus) -> Option<ViewObjectiveStatus> {
    match status {
        ObjectiveStatus::Pending => None,
        ObjectiveStatus::Active => Some(ViewObjectiveStatus::Active),
        ObjectiveStatus::Completed => Some(ViewObjectiveStatus::Completed),
        ObjectiveStatus::Failed | ObjectiveStatus::Invalidated => Some(ViewObjectiveStatus::Failed),
    }
}

fn trigger_objective(trigger: &Trigger) -> Option<&str> {
    match trigger {
        Trigger::ObjectiveRefused { objective_id }
        | Trigger::ObjectiveCompleted { objective_id }
        | Trigger::ObjectiveFailed { objective_id } => Some(objective_id),
        Trigger::PlayerDisclosure { objective_id, .. } => objective_id.as_deref(),
        _ => None,
    }
}

/// Where the story stands against its plan, in one bounded line.
fn divergence_summary(narrative: &NarrativeState) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for mission in &narrative.plan.missions {
        if is_director_mission(mission) {
            continue;
        }
        match mission.status {
            MissionStatus::Failed => parts.push(format!("mission {} failed", mission.mission_id)),
            MissionStatus::Invalidated => {
                parts.push(format!("mission {} invalidated", mission.mission_id));
            }
            _ => {}
        }
    }
    for beat in narrative.divergences() {
        let policy = match beat.policy {
            CanonPolicy::Preserve => continue,
            CanonPolicy::Adapt => "must be adapted",
            CanonPolicy::Replace => "cannot happen as written and needs a replacement",
            CanonPolicy::Delete => "will not happen",
        };
        parts.push(format!("planned beat {} {policy}", beat.checkpoint_id));
    }
    let ready: Vec<String> = narrative
        .ready_checkpoints()
        .into_iter()
        .map(|beat| beat.checkpoint_id)
        .collect();
    if !ready.is_empty() {
        parts.push(format!("beats possible now: {}", ready.join(", ")));
    }
    text(&parts.join("; "), limits::MAX_CONTEXT_TEXT_CHARS)
}

fn narrative_view(
    world: &RuntimeWorld,
    narrative: &NarrativeState,
    trigger: &Trigger,
) -> NarrativeView {
    let focus = trigger_objective(trigger);
    let focus_mission = focus
        .and_then(|id| narrative.plan.objective(id))
        .map(|(mission, _)| mission.mission_id.as_str());

    // Missions: the one the trigger is about, then active ones, then the rest.
    let mut missions: Vec<(&Mission, ViewMissionStatus)> = narrative
        .plan
        .missions
        .iter()
        .filter(|m| !is_director_mission(m))
        .filter_map(|m| mission_status(m.status).map(|status| (m, status)))
        .collect();
    missions.sort_by_key(|(m, _)| {
        (
            Some(m.mission_id.as_str()) != focus_mission,
            m.status != MissionStatus::Active,
        )
    });
    missions.truncate(limits::MAX_MISSIONS);
    let listed: BTreeSet<&str> = missions
        .iter()
        .map(|(m, _)| m.mission_id.as_str())
        .collect();

    let mut objectives: Vec<(&Mission, &Objective, ViewObjectiveStatus)> = narrative
        .plan
        .missions
        .iter()
        .filter(|m| m.status != MissionStatus::Inactive)
        .flat_map(|m| m.objectives.iter().map(move |o| (m, o)))
        .filter_map(|(m, o)| objective_status(o.status).map(|status| (m, o, status)))
        .collect();
    objectives.sort_by_key(|(_, o, _)| {
        (
            Some(o.objective_id.as_str()) != focus,
            o.status != ObjectiveStatus::Active,
        )
    });
    objectives.truncate(limits::MAX_OBJECTIVES);

    NarrativeView {
        summary: divergence_summary(narrative),
        missions: missions
            .into_iter()
            .map(|(mission, status)| MissionView {
                mission_id: mission.mission_id.clone(),
                title: text(&mission.title, limits::MAX_NAME_CHARS)
                    .unwrap_or_else(|| mission.mission_id.clone()),
                status,
                summary: text(&mission.description, limits::MAX_CONTEXT_TEXT_CHARS),
            })
            .collect(),
        objectives: objectives
            .into_iter()
            .map(|(mission, objective, status)| {
                let wrapper = is_director_mission(mission);
                let mission_id = if wrapper {
                    mission.metadata.get(PARENT_MISSION_KEY).map(String::as_str)
                } else {
                    Some(mission.mission_id.as_str())
                };
                let title = if wrapper {
                    &mission.title
                } else {
                    &objective.description
                };
                ObjectiveView {
                    objective_id: objective.objective_id.clone(),
                    title: text(title, limits::MAX_NAME_CHARS)
                        .unwrap_or_else(|| objective.objective_id.clone()),
                    status,
                    description: text(&objective.description, limits::MAX_CONTEXT_TEXT_CHARS),
                    mission_id: mission_id
                        .filter(|id| listed.contains(id))
                        .map(str::to_owned),
                    giver_npc_id: mission
                        .metadata
                        .get(GIVER_KEY)
                        .filter(|id| world.summary.character(id).is_some())
                        .cloned(),
                }
            })
            .collect(),
    }
}

fn npc_views(
    world: &RuntimeWorld,
    narrative: &NarrativeState,
    active: &BTreeSet<String>,
    npcs: &[CharacterState],
) -> Vec<NpcView> {
    // Only characters in the Director's world digest can be referred to.
    world
        .summary
        .characters
        .iter()
        .filter_map(|c| npcs.iter().find(|s| s.character_id().as_str() == c.id))
        .take(limits::MAX_NPCS)
        .map(|state| {
            let id = state.character_id().as_str();
            let alive = state.is_alive()
                && narrative.world.character_status(id) != Some(CharacterStatus::Dead);
            NpcView {
                npc_id: id.to_owned(),
                location: state.location().map(|l| l.as_str().to_owned()),
                active: alive && active.contains(id),
                alive,
                disposition: Some(director_disposition(state.disposition_toward_player())),
                status: (alive && state.status() == LifeStatus::Incapacitated)
                    .then(|| "incapacitated".to_owned()),
            }
        })
        .collect()
}

/// The Director's view of one session as it is right now.
pub(crate) fn build_context(
    world: &RuntimeWorld,
    session: &GameSession,
    narrative: &NarrativeState,
    active: &BTreeSet<String>,
    npcs: &[CharacterState],
    trigger: Trigger,
) -> DirectorContext {
    let view = narrative_view(world, narrative, &trigger);
    let mut ctx = DirectorContext::from_session(
        session,
        world.universe_id.clone(),
        world.summary.clone(),
        trigger,
    );
    ctx.narrative = Some(view);
    ctx.npcs = npc_views(world, narrative, active, npcs);
    ctx
}
