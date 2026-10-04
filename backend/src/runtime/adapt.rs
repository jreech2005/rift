//! Adapters between the protocol's world events and the event models of the
//! NPC and narrative layers.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::action::{ActionType, ValidatedAction, WorldEvent, WorldEventType};
use crate::narrative::{MissionChange, NarrativeEvent, NarrativeState, ObjectiveChange};
use crate::npc::events::Roster;
use crate::npc::{Audience, CharacterId, EntityId, NpcError, NpcEvent, NpcEventKind};
use crate::session::GameSession;

use super::view::{PARENT_MISSION_KEY, is_director_mission};
use super::world::{RuntimeWorld, Secret};

/// Namespace for the deterministic ids of runtime-produced events.
const RUNTIME_EVENT_NAMESPACE: Uuid = Uuid::from_u128(0x8b2d_41f6_c07a_4e93_b5d1_6a3e_9f20_c7d4);

/// Deterministic id for the `index`th thing derived from `seed` (an action's
/// event id or a decision id). `kind` separates the id spaces.
pub(crate) fn derived_id(seed: Uuid, kind: &str, index: usize) -> Uuid {
    let mut name = seed.as_bytes().to_vec();
    name.extend_from_slice(kind.as_bytes());
    name.extend_from_slice(&(index as u64).to_be_bytes());
    Uuid::new_v5(&RUNTIME_EVENT_NAMESPACE, &name)
}

/// A world event that has been decided but not yet recorded in the session.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Emit {
    pub event_type: WorldEventType,
    pub target: Option<String>,
    pub payload: Value,
}

/// Record `emits` in the session log, in order, and return the events.
pub(crate) fn record(
    session: &mut GameSession,
    seed: Uuid,
    emits: Vec<Emit>,
    now: DateTime<Utc>,
) -> Vec<WorldEvent> {
    emits
        .into_iter()
        .enumerate()
        .map(|(index, emit)| {
            let event = WorldEvent {
                event_id: derived_id(seed, "event", index),
                session_id: session.session_id,
                sequence: session.event_count + 1,
                event_type: emit.event_type,
                target: emit.target,
                payload: emit.payload,
                timestamp: now,
            };
            session.record(event.clone());
            event
        })
        .collect()
}

fn wire<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// Narrative status changes as world events for the client. `source` says who
/// caused them: `"narrative"` (a player action) or `"director"`.
pub(crate) fn transition_emits(
    source: &str,
    state: &NarrativeState,
    objective_changes: &[ObjectiveChange],
    mission_changes: &[MissionChange],
) -> Vec<Emit> {
    let mut emits = Vec::new();
    for change in mission_changes {
        let Some(mission) = state.plan.mission(&change.mission_id) else {
            continue;
        };
        if is_director_mission(mission) {
            continue;
        }
        emits.push(Emit {
            event_type: WorldEventType::MissionUpdated,
            target: Some(change.mission_id.clone()),
            payload: json!({
                "source": source,
                "mission_id": change.mission_id,
                "status": wire(&change.to),
                "title": mission.title,
                "cause": wire(&change.cause),
            }),
        });
    }
    for change in objective_changes {
        let Some((mission, objective)) = state.plan.objective(&change.objective_id) else {
            continue;
        };
        let (mission_id, title) = if is_director_mission(mission) {
            (mission.metadata.get(PARENT_MISSION_KEY), &mission.title)
        } else {
            (Some(&mission.mission_id), &objective.description)
        };
        emits.push(Emit {
            event_type: WorldEventType::ObjectiveUpdated,
            target: Some(change.objective_id.clone()),
            payload: json!({
                "source": source,
                "objective_id": change.objective_id,
                "mission_id": mission_id,
                "status": wire(&change.to),
                "title": title,
                "description": objective.description,
                "cause": wire(&change.cause),
            }),
        });
    }
    emits
}

/// The secret a `speak` action gives away, and the NPC who hears it.
///
/// Only an NPC that exists in this session and can perceive hears anything:
/// talking to the dead, the unconscious or to nobody discloses nothing.
pub(crate) fn disclosure<'a>(
    world: &'a RuntimeWorld,
    roster: &Roster,
    action: &ValidatedAction,
) -> Option<(&'a Secret, CharacterId)> {
    if action.action_type != ActionType::Speak {
        return None;
    }
    let listener = CharacterId::new(action.target.as_deref()?).ok()?;
    if !roster.get(&listener)?.can_perceive() {
        return None;
    }
    let content = action.content.as_deref()?;
    let secret = world.secrets.iter().find(|s| s.matches(content))?;
    Some((secret, listener))
}

/// The private NPC event for a disclosure: the listener alone learns the fact.
pub(crate) fn disclosure_event(
    event: &WorldEvent,
    secret: &Secret,
    listener: &CharacterId,
    roster: &Roster,
) -> Result<NpcEvent, NpcError> {
    Ok(NpcEvent {
        event_id: derived_id(event.event_id, "npc", 0),
        session_id: event.session_id,
        world_time: event.sequence,
        timestamp: event.timestamp,
        location: roster.get(listener).and_then(|s| s.location().cloned()),
        kind: NpcEventKind::FactRevealed {
            speaker: EntityId::player(),
            listener: listener.clone(),
            fact: secret.fact()?,
        },
        audience: Audience::Participants,
    })
}

/// What an accepted player action means to the narrative layer. Only things
/// the narrative world can represent are reported, so a harmless action (a
/// walk to a place the story does not know) never becomes an error.
pub(crate) fn narrative_events(
    action: &ValidatedAction,
    event: &WorldEvent,
    disclosed: Option<(&Secret, &CharacterId)>,
    narrative: &NarrativeState,
) -> Vec<NarrativeEvent> {
    let world = &narrative.world;
    match action.action_type {
        ActionType::Interact => {
            let first = event.payload.get("first_interaction") == Some(&Value::Bool(true));
            match &action.target {
                Some(target) if first => vec![NarrativeEvent::FlagSet {
                    flag: format!("interacted:{target}"),
                    value: true,
                }],
                _ => Vec::new(),
            }
        }
        ActionType::Move => match &action.target {
            Some(target) if world.locations.contains(target) && !world.location_lost(target) => {
                vec![NarrativeEvent::PlayerMoved {
                    location_id: target.clone(),
                }]
            }
            _ => Vec::new(),
        },
        ActionType::Speak => match disclosed {
            Some((secret, listener)) if !world.is_dead(listener.as_str()) => {
                vec![NarrativeEvent::FactRevealed {
                    fact_id: secret.fact_id.clone(),
                    to: listener.as_str().to_owned(),
                }]
            }
            _ => Vec::new(),
        },
        ActionType::Inspect => Vec::new(),
    }
}
