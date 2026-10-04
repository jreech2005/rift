//! Applying a validated Director decision to drafts of the authoritative
//! state. Nothing here commits: the caller does, and only if every action
//! applied.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::action::WorldEventType;
use crate::director::{
    DirectorAction, DirectorDecision, Disposition as DirectorDisposition, PLAYER_ID,
    ValidationIssue,
};
use crate::narrative::{
    Mission, NarrativeEngine, NarrativeError, NarrativeEvent, NarrativeState, Objective,
};
use crate::npc::events::Roster;
use crate::npc::{
    Audience, CharacterId, CharacterState, Disposition, EntityId, Fact, FactId, LocationId,
    MemoryEntry, NpcError, NpcEvent, NpcEventKind, Relationship, perceive,
};
use crate::session::GameSession;

use super::adapt::{Emit, derived_id, transition_emits};
use super::view::{ORIGIN_DIRECTOR, ORIGIN_KEY, PARENT_MISSION_KEY};

/// Who an NPC hears Director-revealed information from when no NPC who
/// actually knows it is named. Never a character.
const NARRATOR_ID: &str = "narrator";

/// Namespace for the fact ids of Director-revealed information.
const DIRECTOR_FACT_NAMESPACE: Uuid = Uuid::from_u128(0x3e7a_90c4_1b5d_4f28_a6e1_8c0b_d2f4_7a19);

const SOURCE: &str = "director";

/// Why a decision was not applied. In every case nothing was changed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ApplyError {
    #[error("session {0} has no runtime world")]
    NoStory(Uuid),
    #[error("decision is for universe {decision:?}, session runs {session:?}")]
    WrongUniverse { decision: String, session: String },
    /// The decision does not validate against the state as it is now.
    #[error("decision is not valid for the current state: {}", summarize(.0))]
    Invalid(Vec<ValidationIssue>),
    #[error("action {action_id}: {source}")]
    Narrative {
        action_id: String,
        source: NarrativeError,
    },
    #[error("action {action_id}: {source}")]
    Npc { action_id: String, source: NpcError },
    #[error("could not commit npc state: {0}")]
    Commit(NpcError),
}

fn summarize(issues: &[ValidationIssue]) -> String {
    issues
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// A `request_replan` the Director issued. Recorded, never executed: Phase 2
/// has no planner, and running one here would start a Director loop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplanNote {
    pub decision_id: Uuid,
    pub action_id: String,
    pub reason: String,
    pub mission_id: Option<String>,
    /// `GameSession::event_count` when it was recorded.
    pub event_count: u64,
}

/// Drafts of everything a decision may touch.
pub(crate) struct Drafts<'a> {
    pub session: &'a mut GameSession,
    pub narrative: &'a mut NarrativeState,
    pub roster: &'a mut Roster,
    pub active: &'a mut BTreeSet<String>,
}

#[derive(Default)]
pub(crate) struct Output {
    pub emits: Vec<Emit>,
    pub memories: Vec<MemoryEntry>,
    pub replans: Vec<ReplanNote>,
}

/// A relationship that reads as `disposition` (`Relationship::disposition`).
fn relationship_for(disposition: DirectorDisposition) -> (Relationship, Disposition) {
    match disposition {
        DirectorDisposition::Hostile => (Relationship::new(-50, 0, -50), Disposition::Hostile),
        DirectorDisposition::Wary => (Relationship::new(-20, 0, -15), Disposition::Wary),
        DirectorDisposition::Neutral => (Relationship::new(0, 0, 0), Disposition::Neutral),
        DirectorDisposition::Friendly => (Relationship::new(30, 0, 30), Disposition::Friendly),
        DirectorDisposition::Loyal => (Relationship::new(60, 0, 60), Disposition::Loyal),
    }
}

fn wire<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

struct Applier<'a, 'd> {
    drafts: Drafts<'d>,
    decision: &'a DirectorDecision,
    now: DateTime<Utc>,
    out: Output,
    /// Counter for the NPC events this decision produces.
    npc_events: usize,
}

impl Applier<'_, '_> {
    fn narrative(&mut self, action_id: &str, event: NarrativeEvent) -> Result<(), ApplyError> {
        let transition =
            NarrativeEngine::apply_event(self.drafts.narrative, &event).map_err(|source| {
                ApplyError::Narrative {
                    action_id: action_id.to_owned(),
                    source,
                }
            })?;
        self.out.emits.extend(transition_emits(
            SOURCE,
            &transition.state,
            &transition.objective_changes,
            &transition.mission_changes,
        ));
        *self.drafts.narrative = transition.state;
        Ok(())
    }

    fn npc_mut(&mut self, npc_id: &str) -> Result<&mut CharacterState, NpcError> {
        let id = CharacterId::new(npc_id)?;
        self.drafts
            .roster
            .get_mut(&id)
            .ok_or_else(|| NpcError::UnknownCharacter(npc_id.to_owned()))
    }

    /// Let the audience of `kind` perceive it: knowledge, feelings, memories.
    fn npc_event(
        &mut self,
        kind: NpcEventKind,
        audience: Audience,
        location: Option<LocationId>,
    ) -> Result<(), NpcError> {
        let event = NpcEvent {
            event_id: derived_id(self.decision.decision_id, "npc", self.npc_events),
            session_id: self.drafts.session.session_id,
            world_time: self.drafts.session.event_count,
            timestamp: self.now,
            location,
            kind,
            audience,
        };
        self.npc_events += 1;
        for perception in perceive(&event, self.drafts.roster)? {
            let state = self
                .drafts
                .roster
                .get_mut(&perception.character_id)
                .ok_or_else(|| NpcError::UnknownCharacter(perception.character_id.to_string()))?;
            perception.apply_to(state, &event)?;
            self.out.memories.push(perception.memory);
        }
        Ok(())
    }

    /// Put an NPC somewhere, in both the NPC layer and, when the narrative
    /// world knows the character and the place, the narrative layer.
    fn place(
        &mut self,
        action_id: &str,
        npc_id: &str,
        location_id: &str,
    ) -> Result<(), ApplyError> {
        let now = self.now;
        let npc = |source| ApplyError::Npc {
            action_id: action_id.to_owned(),
            source,
        };
        let location = LocationId::new(location_id).map_err(npc)?;
        self.npc_mut(npc_id)
            .and_then(|state| state.set_location(Some(location), now))
            .map_err(npc)?;

        let world = &self.drafts.narrative.world;
        if world.characters.contains_key(npc_id)
            && !world.is_dead(npc_id)
            && world.locations.contains(location_id)
            && !world.location_lost(location_id)
        {
            self.narrative(
                action_id,
                NarrativeEvent::CharacterMoved {
                    character_id: npc_id.to_owned(),
                    location_id: location_id.to_owned(),
                },
            )?;
        }
        Ok(())
    }

    fn emit(&mut self, event_type: WorldEventType, target: Option<&str>, payload: Value) {
        self.out.emits.push(Emit {
            event_type,
            target: target.map(str::to_owned),
            payload,
        });
    }

    fn set_flag(&mut self, action_id: &str, flag: &str, value: bool) -> Result<(), ApplyError> {
        self.narrative(
            action_id,
            NarrativeEvent::FlagSet {
                flag: flag.to_owned(),
                value,
            },
        )?;
        self.drafts
            .session
            .world_flags
            .insert(flag.to_owned(), value);
        self.emit(
            WorldEventType::WorldFlagChanged,
            None,
            json!({ "source": SOURCE, "flag": flag, "value": value }),
        );
        Ok(())
    }

    fn apply(&mut self, action: &DirectorAction) -> Result<(), ApplyError> {
        let action_id = action.action_id();
        let npc = |source| ApplyError::Npc {
            action_id: action_id.to_owned(),
            source,
        };
        let now = self.now;
        match action {
            DirectorAction::SetObjective {
                objective_id,
                title,
                description,
                mission_id,
                ..
            } => {
                // The narrative layer adds objectives only as part of a new
                // mission, so a Director objective is a one-objective mission.
                let mut mission = Mission::new(
                    objective_id.clone(),
                    title.clone(),
                    vec![Objective::new(objective_id.clone(), description.clone())],
                );
                mission.description = description.clone();
                mission
                    .metadata
                    .insert(ORIGIN_KEY.to_owned(), ORIGIN_DIRECTOR.to_owned());
                if let Some(parent) = mission_id {
                    mission
                        .metadata
                        .insert(PARENT_MISSION_KEY.to_owned(), parent.clone());
                }
                let transition = NarrativeEngine::adopt_mission(self.drafts.narrative, mission)
                    .map_err(|source| ApplyError::Narrative {
                        action_id: action_id.to_owned(),
                        source,
                    })?;
                self.out.emits.extend(transition_emits(
                    SOURCE,
                    &transition.state,
                    &transition.objective_changes,
                    &transition.mission_changes,
                ));
                *self.drafts.narrative = transition.state;
            }
            DirectorAction::CompleteObjective { objective_id, .. } => self.narrative(
                action_id,
                NarrativeEvent::ObjectiveCompleted {
                    objective_id: objective_id.clone(),
                },
            )?,
            DirectorAction::FailObjective { objective_id, .. } => self.narrative(
                action_id,
                NarrativeEvent::ObjectiveFailed {
                    objective_id: objective_id.clone(),
                },
            )?,
            DirectorAction::InvalidateMission { mission_id, .. } => self.narrative(
                action_id,
                NarrativeEvent::MissionInvalidated {
                    mission_id: mission_id.clone(),
                },
            )?,
            DirectorAction::ActivateNpc {
                npc_id,
                location_id,
                ..
            } => {
                self.place(action_id, npc_id, location_id)?;
                self.drafts.active.insert(npc_id.clone());
                self.emit(
                    WorldEventType::NpcActivated,
                    Some(npc_id),
                    json!({ "source": SOURCE, "npc_id": npc_id, "location_id": location_id }),
                );
            }
            DirectorAction::MoveNpc {
                npc_id,
                location_id,
                reason,
                ..
            } => {
                self.place(action_id, npc_id, location_id)?;
                self.emit(
                    WorldEventType::NpcMoved,
                    Some(npc_id),
                    json!({
                        "source": SOURCE,
                        "npc_id": npc_id,
                        "location_id": location_id,
                        "reason": reason,
                    }),
                );
            }
            DirectorAction::SetNpcDisposition {
                npc_id,
                toward,
                disposition,
                reason,
                ..
            } => {
                let (relationship, reads_as) = relationship_for(*disposition);
                debug_assert_eq!(relationship.disposition(), reads_as);
                let toward_id = EntityId::new(toward.clone()).map_err(npc)?;
                self.npc_mut(npc_id)
                    .and_then(|state| state.set_relationship(toward_id, relationship, now))
                    .map_err(npc)?;
                self.emit(
                    WorldEventType::NpcDispositionChanged,
                    Some(npc_id),
                    json!({
                        "source": SOURCE,
                        "npc_id": npc_id,
                        "toward": toward,
                        "disposition": wire(disposition),
                        "reason": reason,
                    }),
                );
            }
            DirectorAction::RevealInformation {
                recipient_id,
                text,
                source_npc_id,
                ..
            } => {
                if recipient_id == PLAYER_ID {
                    self.emit(
                        WorldEventType::InformationRevealed,
                        Some(PLAYER_ID),
                        json!({
                            "source": SOURCE,
                            "recipient_id": recipient_id,
                            "source_npc_id": source_npc_id,
                            "text": text,
                        }),
                    );
                } else {
                    let digest = Uuid::new_v5(&DIRECTOR_FACT_NAMESPACE, text.as_bytes()).simple();
                    let fact_id = FactId::new(format!("director:{digest}")).map_err(npc)?;
                    let fact = Fact::new(fact_id.clone(), text).map_err(npc)?;
                    let listener = CharacterId::new(recipient_id.clone()).map_err(npc)?;
                    // An NPC can only pass on what it knows. A named source
                    // that does not know this is not made to: the recipient
                    // hears it from no one in particular.
                    let knowing_source = source_npc_id
                        .as_deref()
                        .and_then(|id| CharacterId::new(id).ok())
                        .filter(|id| {
                            self.drafts
                                .roster
                                .get(id)
                                .is_some_and(|s| s.knows(&fact_id))
                        });
                    let speaker = match &knowing_source {
                        Some(id) => EntityId::from(id),
                        None => EntityId::new(NARRATOR_ID).map_err(npc)?,
                    };
                    self.npc_event(
                        NpcEventKind::FactRevealed {
                            speaker,
                            listener,
                            fact,
                        },
                        Audience::Participants,
                        None,
                    )
                    .map_err(npc)?;
                    // What an NPC was told is private to that NPC: the client
                    // learns that it happened, not what was said.
                    self.emit(
                        WorldEventType::InformationRevealed,
                        Some(recipient_id),
                        json!({
                            "source": SOURCE,
                            "recipient_id": recipient_id,
                            "source_npc_id": knowing_source.as_ref().map(CharacterId::as_str),
                        }),
                    );
                }
            }
            DirectorAction::SetWorldFlag { flag, .. } => self.set_flag(action_id, flag, true)?,
            DirectorAction::ClearWorldFlag { flag, .. } => self.set_flag(action_id, flag, false)?,
            DirectorAction::TriggerWorldEvent {
                event,
                description,
                location_id,
                npc_ids,
                ..
            } => {
                let location = location_id
                    .as_deref()
                    .map(LocationId::new)
                    .transpose()
                    .map_err(npc)?;
                // Named NPCs witness it; otherwise whoever is at the place
                // does; with neither, no NPC perceives it at all.
                let audience = if !npc_ids.is_empty() {
                    let witnesses = npc_ids
                        .iter()
                        .map(|id| CharacterId::new(id.clone()))
                        .collect::<Result<BTreeSet<_>, _>>()
                        .map_err(npc)?;
                    Some(Audience::Witnesses { witnesses })
                } else if location.is_some() {
                    Some(Audience::Location)
                } else {
                    None
                };
                if let Some(audience) = audience {
                    let entities = npc_ids
                        .iter()
                        .map(|id| EntityId::new(id.clone()))
                        .collect::<Result<BTreeSet<_>, _>>()
                        .map_err(npc)?;
                    self.npc_event(
                        NpcEventKind::WorldEvent {
                            summary: description.clone(),
                            entities,
                            flag: None,
                        },
                        audience,
                        location,
                    )
                    .map_err(npc)?;
                }
                self.emit(
                    WorldEventType::WorldEventTriggered,
                    location_id.as_deref(),
                    json!({
                        "source": SOURCE,
                        "event": wire(event),
                        "description": description,
                        "location_id": location_id,
                        "npc_ids": npc_ids,
                    }),
                );
            }
            DirectorAction::StartDialogue {
                npc_id,
                opening_line,
                ..
            } => self.emit(
                WorldEventType::DialogueStarted,
                Some(npc_id),
                json!({
                    "source": SOURCE,
                    "npc_id": npc_id,
                    "opening_line": opening_line,
                    // The line as the client shows and speaks it.
                    "text": opening_line,
                }),
            ),
            DirectorAction::RequestReplan {
                reason, mission_id, ..
            } => self.out.replans.push(ReplanNote {
                decision_id: self.decision.decision_id,
                action_id: action_id.to_owned(),
                reason: reason.clone(),
                mission_id: mission_id.clone(),
                event_count: self.drafts.session.event_count,
            }),
        }
        Ok(())
    }
}

/// Apply every action of `decision` to the drafts, in order. On error the
/// drafts are left half-changed and must be discarded.
pub(crate) fn apply_actions(
    drafts: Drafts<'_>,
    decision: &DirectorDecision,
    now: DateTime<Utc>,
) -> Result<Output, ApplyError> {
    let mut applier = Applier {
        drafts,
        decision,
        now,
        out: Output::default(),
        npc_events: 0,
    };
    for action in &decision.actions {
        applier.apply(action)?;
    }
    Ok(applier.out)
}
