//! Deterministic event -> memory adapter.
//!
//! [`perceive`] turns one typed [`NpcEvent`] into [`Perception`]s: for each NPC
//! that plausibly witnessed or was told about the event, the memory it forms
//! and the state changes it undergoes. It is a pure function — no clock, no
//! randomness, no I/O, no LLM — and it never broadcasts: an NPC outside the
//! event's [`Audience`] gets nothing, however global the event is.
//!
//! `NpcEvent` is the NPC layer's own input type. The integration layer builds
//! it from validated world/Director events; nothing here reads `GameSession`.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::ids::{CharacterId, EntityId, FactId, FlagKey, LocationId};
use super::memory::{MAX_MEMORY_ENTITIES, MemoryEntry, MemoryType};
use super::relationship::RelationshipDelta;
use super::state::{CharacterState, Fact, KnowledgeSource, LifeStatus};
use super::{MAX_TEXT_LEN, NpcError, clean_text, truncate_text};

/// The NPCs of one session, keyed by id.
pub type Roster = BTreeMap<CharacterId, CharacterState>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NpcEvent {
    pub event_id: Uuid,
    pub session_id: Uuid,
    /// Session event sequence (`WorldEvent::sequence`).
    pub world_time: u64,
    pub timestamp: DateTime<Utc>,
    /// Where it happened. Required for [`Audience::Location`].
    #[serde(default)]
    pub location: Option<LocationId>,
    pub kind: NpcEventKind,
    pub audience: Audience,
}

/// A world change that sets a flag; witnesses learn the flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlagChange {
    pub key: FlagKey,
    pub value: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NpcEventKind {
    /// `actor` helped NPC `target`.
    Helped {
        actor: EntityId,
        target: CharacterId,
    },
    /// `actor` threatened NPC `target`.
    Threatened {
        actor: EntityId,
        target: CharacterId,
    },
    /// `actor` physically harmed NPC `target`.
    Harmed {
        actor: EntityId,
        target: CharacterId,
    },
    /// `speaker` told NPC `listener` a fact. An NPC speaker must know it.
    FactRevealed {
        speaker: EntityId,
        listener: CharacterId,
        fact: Fact,
    },
    /// NPC `character` died, optionally at the hands of `killer`. With
    /// [`Audience::Reported`] this is news of an earlier death instead.
    CharacterDied {
        character: CharacterId,
        #[serde(default)]
        killer: Option<EntityId>,
    },
    /// A mission failed.
    MissionFailed {
        mission_id: EntityId,
        summary: String,
    },
    /// Any other observable world event.
    WorldEvent {
        summary: String,
        #[serde(default)]
        entities: BTreeSet<EntityId>,
        #[serde(default)]
        flag: Option<FlagChange>,
    },
}

impl NpcEventKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Helped { .. } => "helped",
            Self::Threatened { .. } => "threatened",
            Self::Harmed { .. } => "harmed",
            Self::FactRevealed { .. } => "fact_revealed",
            Self::CharacterDied { .. } => "character_died",
            Self::MissionFailed { .. } => "mission_failed",
            Self::WorldEvent { .. } => "world_event",
        }
    }

    /// The NPC the event is done to / said to, if any.
    fn subject(&self) -> Option<&CharacterId> {
        match self {
            Self::Helped { target, .. }
            | Self::Threatened { target, .. }
            | Self::Harmed { target, .. } => Some(target),
            Self::FactRevealed { listener, .. } => Some(listener),
            _ => None,
        }
    }

    /// The entity doing or saying it, if any.
    fn actor(&self) -> Option<&EntityId> {
        match self {
            Self::Helped { actor, .. }
            | Self::Threatened { actor, .. }
            | Self::Harmed { actor, .. } => Some(actor),
            Self::FactRevealed { speaker, .. } => Some(speaker),
            Self::CharacterDied { killer, .. } => killer.as_ref(),
            _ => None,
        }
    }
}

/// Who, besides the event's subject, perceives it. There is deliberately no
/// "everyone" variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum Audience {
    /// Only the NPC it was done to / said to. A private conversation.
    Participants,
    /// The subject plus these NPCs, who saw or overheard it.
    Witnesses { witnesses: BTreeSet<CharacterId> },
    /// The subject plus every NPC currently at the event's location.
    Location,
    /// Information transfer: `informant` tells `listeners` about an event
    /// they did not see. Only the listeners form a (second-hand) memory.
    Reported {
        informant: EntityId,
        listeners: BTreeSet<CharacterId>,
    },
}

/// A change to one NPC's state caused by perceiving an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum StateEffect {
    LearnFact {
        fact: Fact,
        source: KnowledgeSource,
    },
    LearnFlag {
        key: FlagKey,
        value: bool,
    },
    AdjustRelationship {
        toward: EntityId,
        delta: RelationshipDelta,
    },
}

/// What one NPC takes away from one event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Perception {
    pub character_id: CharacterId,
    pub memory: MemoryEntry,
    pub effects: Vec<StateEffect>,
}

impl Perception {
    /// Apply the effects to the perceiving NPC's state.
    pub fn apply_to(&self, state: &mut CharacterState, event: &NpcEvent) -> Result<(), NpcError> {
        if state.character_id() != &self.character_id {
            return Err(NpcError::InvalidEvent(format!(
                "perception of {} applied to {}",
                self.character_id,
                state.character_id()
            )));
        }
        for effect in &self.effects {
            match effect {
                StateEffect::LearnFact { fact, source } => {
                    state.learn_fact(
                        fact.clone(),
                        source.clone(),
                        Some(event.event_id),
                        event.timestamp,
                    )?;
                }
                StateEffect::LearnFlag { key, value } => {
                    state.learn_flag(key.clone(), *value, event.timestamp)?;
                }
                StateEffect::AdjustRelationship { toward, delta } => {
                    state.adjust_relationship(toward, *delta, event.timestamp)?;
                }
            }
        }
        Ok(())
    }
}

/// Fact id under which NPCs know that `character` is dead.
pub fn death_fact_id(character: &CharacterId) -> FactId {
    FactId::new(format!("died:{character}")).expect("prefix + character id fits a fact id")
}

/// Flag under which NPCs know that mission `mission_id` failed.
pub fn mission_failed_flag(mission_id: &EntityId) -> FlagKey {
    FlagKey::new(format!("mission_failed:{mission_id}")).expect("prefix + id fits a flag key")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// The event was done to / said to this NPC.
    Subject,
    /// Saw or overheard it.
    Witness,
    /// Was told about it afterwards.
    Informed,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Self::Subject => "direct",
            Self::Witness => "witnessed",
            Self::Informed => "reported",
        }
    }
}

/// Compute who perceives `event` and what each of them takes away.
///
/// Results are ordered by character id. Errors mean the event is malformed
/// (unknown or dead participants, a speaker revealing something it does not
/// know, ...) and nothing should be applied.
pub fn perceive(event: &NpcEvent, roster: &Roster) -> Result<Vec<Perception>, NpcError> {
    validate(event, roster)?;
    let informant = match &event.audience {
        Audience::Reported { informant, .. } => Some(informant),
        _ => None,
    };
    recipients(event, roster)
        .into_iter()
        .map(|(id, role)| {
            let state = &roster[&id];
            build_perception(event, state, role, informant, roster)
        })
        .collect()
}

fn lookup<'a>(roster: &'a Roster, id: &CharacterId) -> Result<&'a CharacterState, NpcError> {
    roster
        .get(id)
        .ok_or_else(|| NpcError::UnknownCharacter(id.to_string()))
}

fn roster_entry<'a>(roster: &'a Roster, entity: &EntityId) -> Option<&'a CharacterState> {
    roster.values().find(|s| entity == s.character_id())
}

fn validate(event: &NpcEvent, roster: &Roster) -> Result<(), NpcError> {
    let invalid = |msg: &str| Err(NpcError::InvalidEvent(msg.to_owned()));

    if let Some(subject) = event.kind.subject() {
        let state = lookup(roster, subject)?;
        if state.status() == LifeStatus::Dead {
            return Err(NpcError::CharacterDead(subject.to_string()));
        }
        if event.kind.actor().is_some_and(|actor| actor == subject) {
            return invalid("actor and target must differ");
        }
    }
    // A dead NPC cannot act, speak or pass on news.
    let informant = match &event.audience {
        Audience::Reported { informant, .. } => Some(informant),
        _ => None,
    };
    for entity in event.kind.actor().into_iter().chain(informant) {
        if let Some(state) = roster_entry(roster, entity)
            && state.status() == LifeStatus::Dead
        {
            return Err(NpcError::CharacterDead(entity.to_string()));
        }
    }

    match &event.kind {
        NpcEventKind::FactRevealed { speaker, fact, .. } => {
            clean_text("fact statement", &fact.statement, MAX_TEXT_LEN)?;
            // Knowledge boundary: an NPC can only pass on what it knows.
            if let Some(state) = roster_entry(roster, speaker)
                && !state.knows(&fact.fact_id)
            {
                return Err(NpcError::UnknownFact {
                    character: speaker.to_string(),
                    fact: fact.fact_id.to_string(),
                });
            }
        }
        NpcEventKind::CharacterDied { character, killer } => {
            let dead = lookup(roster, character)?.status() == LifeStatus::Dead;
            let news = matches!(event.audience, Audience::Reported { .. });
            if dead && !news {
                return Err(NpcError::CharacterDead(character.to_string()));
            }
            // News of a death travels after the fact; rumours of a death that
            // never happened are not modelled.
            if news && !dead {
                return invalid("cannot report the death of a living character");
            }
            if killer.as_ref().is_some_and(|k| k == character) {
                return invalid("a character cannot be its own killer");
            }
        }
        NpcEventKind::MissionFailed { summary, .. } => {
            clean_text("event summary", summary, MAX_TEXT_LEN)?;
        }
        NpcEventKind::WorldEvent {
            summary, entities, ..
        } => {
            clean_text("event summary", summary, MAX_TEXT_LEN)?;
            if entities.len() > MAX_MEMORY_ENTITIES - 1 {
                return Err(NpcError::LimitExceeded {
                    what: "event entities",
                    max: MAX_MEMORY_ENTITIES - 1,
                });
            }
        }
        _ => {}
    }

    match &event.audience {
        Audience::Participants => {}
        Audience::Witnesses { witnesses } => {
            for id in witnesses {
                lookup(roster, id)?;
            }
        }
        Audience::Location => {
            if event.location.is_none() {
                return invalid("a location audience requires an event location");
            }
        }
        Audience::Reported { listeners, .. } => {
            if listeners.is_empty() {
                return invalid("a reported event requires at least one listener");
            }
            for id in listeners {
                lookup(roster, id)?;
            }
        }
    }
    Ok(())
}

/// Who perceives the event, in character-id order. Assumes `validate` passed.
fn recipients(event: &NpcEvent, roster: &Roster) -> Vec<(CharacterId, Role)> {
    let subject = event.kind.subject();
    let actor = event.kind.actor();
    let dying = match &event.kind {
        NpcEventKind::CharacterDied { character, .. } => Some(character),
        _ => None,
    };
    // NPCs with their own part in the event never count as bystanders.
    let involved = |id: &CharacterId| {
        subject == Some(id) || dying == Some(id) || actor.is_some_and(|a| a == id)
    };
    let perceives = |id: &CharacterId| roster.get(id).is_some_and(CharacterState::can_perceive);

    let mut out: BTreeMap<CharacterId, Role> = BTreeMap::new();
    if let Audience::Reported {
        informant,
        listeners,
    } = &event.audience
    {
        for id in listeners {
            if perceives(id) && !involved(id) && informant != id {
                out.insert(id.clone(), Role::Informed);
            }
        }
        return out.into_iter().collect();
    }

    if let Some(subject) = subject
        && perceives(subject)
    {
        out.insert(subject.clone(), Role::Subject);
    }
    match &event.audience {
        Audience::Witnesses { witnesses } => {
            for id in witnesses {
                if perceives(id) && !involved(id) {
                    out.insert(id.clone(), Role::Witness);
                }
            }
        }
        Audience::Location => {
            for (id, state) in roster {
                if state.can_perceive()
                    && !involved(id)
                    && state.location().is_some()
                    && state.location() == event.location.as_ref()
                {
                    out.insert(id.clone(), Role::Witness);
                }
            }
        }
        Audience::Participants | Audience::Reported { .. } => {}
    }
    out.into_iter().collect()
}

/// How `entity` is referred to in `viewer`'s memory.
fn label(entity: &EntityId, viewer: &CharacterState, roster: &Roster) -> String {
    if entity == viewer.character_id() {
        "me".to_owned()
    } else if entity.is_player() {
        "the player".to_owned()
    } else if let Some(state) = roster_entry(roster, entity) {
        state.name().to_owned()
    } else {
        entity.to_string()
    }
}

fn sentence(text: String) -> String {
    let mut chars = text.chars();
    let capitalized = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => text,
    };
    truncate_text(&capitalized, MAX_TEXT_LEN)
}

/// Per-recipient appraisal of an event: `(importance, valence, delta)`.
struct Appraisal {
    importance: u8,
    valence: i8,
    /// Change in feelings toward the event's actor.
    delta: RelationshipDelta,
}

fn appraisal(importance: u8, valence: i8, trust: i32, fear: i32, affinity: i32) -> Appraisal {
    Appraisal {
        importance,
        valence,
        delta: RelationshipDelta::new(trust, fear, affinity),
    }
}

fn build_perception(
    event: &NpcEvent,
    me: &CharacterState,
    role: Role,
    informant: Option<&EntityId>,
    roster: &Roster,
) -> Result<Perception, NpcError> {
    let name = |entity: &EntityId| label(entity, me, roster);
    let subject = role == Role::Subject;
    // Is the NPC this happened to someone I care about?
    let cares_about = |other: &CharacterId| me.relationship(&other.into()).is_ally();
    // "Hank told me that " for second-hand news.
    let told = informant
        .filter(|_| role == Role::Informed)
        .map(|i| format!("{} told me that ", name(i)));

    let mut effects = Vec::new();
    let mut entities: BTreeSet<EntityId> = BTreeSet::new();
    let mut metadata = BTreeMap::new();
    let knowledge_source = match (role, informant) {
        (Role::Informed, Some(informant)) => KnowledgeSource::Told {
            by: informant.clone(),
        },
        _ => KnowledgeSource::Witnessed,
    };

    let (memory_type, summary, mut verdict) = match &event.kind {
        NpcEventKind::Helped { actor, target }
        | NpcEventKind::Threatened { actor, target }
        | NpcEventKind::Harmed { actor, target } => {
            let ally = cares_about(target);
            let (past, base, verdict) = match &event.kind {
                NpcEventKind::Helped { .. } => (
                    "helped",
                    "help",
                    match (subject, ally) {
                        (true, _) => appraisal(60, 60, 15, -5, 20),
                        (false, true) => appraisal(40, 40, 8, 0, 10),
                        (false, false) => appraisal(30, 15, 3, 0, 0),
                    },
                ),
                NpcEventKind::Threatened { .. } => (
                    "threatened",
                    "threaten",
                    match (subject, ally) {
                        (true, _) => appraisal(75, -70, -20, 25, -15),
                        (false, true) => appraisal(60, -55, -12, 10, -10),
                        (false, false) => appraisal(45, -30, -5, 8, 0),
                    },
                ),
                _ => (
                    "harmed",
                    "harm",
                    match (subject, ally) {
                        (true, _) => appraisal(90, -90, -35, 30, -35),
                        (false, true) => appraisal(80, -80, -25, 10, -30),
                        (false, false) => appraisal(55, -40, -10, 15, 0),
                    },
                ),
            };
            entities.insert(actor.clone());
            entities.insert(target.into());
            let (who, whom) = (name(actor), name(&target.into()));
            let (memory_type, summary) = match role {
                Role::Subject => (MemoryType::Interaction, format!("{who} {past} me.")),
                Role::Witness => (
                    MemoryType::Observation,
                    format!("I saw {who} {base} {whom}."),
                ),
                Role::Informed => (
                    MemoryType::Report,
                    format!(
                        "{}{who} {past} {whom}.",
                        told.as_deref().unwrap_or_default()
                    ),
                ),
            };
            effects.push(StateEffect::AdjustRelationship {
                toward: actor.clone(),
                delta: verdict.delta,
            });
            (memory_type, summary, verdict)
        }
        NpcEventKind::FactRevealed {
            speaker,
            listener,
            fact,
        } => {
            entities.insert(speaker.clone());
            entities.insert(listener.into());
            metadata.insert("fact_id".to_owned(), json!(fact.fact_id));
            let (who, whom) = (name(speaker), name(&listener.into()));
            let statement = &fact.statement;
            let (memory_type, summary, source, verdict) = match role {
                Role::Subject => (
                    MemoryType::Revelation,
                    format!("{who} told me: {statement}"),
                    KnowledgeSource::Told {
                        by: speaker.clone(),
                    },
                    // Being confided in builds a little trust.
                    appraisal(65, 0, 5, 0, 0),
                ),
                Role::Witness => (
                    MemoryType::Revelation,
                    format!("I overheard {who} tell {whom}: {statement}"),
                    KnowledgeSource::Witnessed,
                    appraisal(55, 0, 0, 0, 0),
                ),
                Role::Informed => (
                    MemoryType::Report,
                    format!(
                        "{}{who} told {whom}: {statement}",
                        told.as_deref().unwrap_or_default()
                    ),
                    knowledge_source.clone(),
                    appraisal(65, 0, 0, 0, 0),
                ),
            };
            effects.push(StateEffect::LearnFact {
                fact: fact.clone(),
                source,
            });
            if !verdict.delta.is_zero() {
                effects.push(StateEffect::AdjustRelationship {
                    toward: speaker.clone(),
                    delta: verdict.delta,
                });
            }
            (memory_type, summary, verdict)
        }
        NpcEventKind::CharacterDied { character, killer } => {
            let dead: EntityId = character.into();
            let dead_name = name(&dead);
            entities.insert(dead);
            let ally = cares_about(character);
            let mut verdict = if ally {
                appraisal(95, -90, -40, 25, -50)
            } else {
                appraisal(80, -40, -15, 25, 0)
            };
            let what = match killer {
                Some(killer) => {
                    entities.insert(killer.clone());
                    if killer == me.character_id() {
                        verdict.delta = RelationshipDelta::default();
                    } else {
                        effects.push(StateEffect::AdjustRelationship {
                            toward: killer.clone(),
                            delta: verdict.delta,
                        });
                    }
                    match role {
                        Role::Informed => format!("{} killed {dead_name}.", name(killer)),
                        _ => format!("I saw {} kill {dead_name}.", name(killer)),
                    }
                }
                None => {
                    verdict.delta = RelationshipDelta::default();
                    match role {
                        Role::Informed => format!("{dead_name} died."),
                        _ => format!("I saw {dead_name} die."),
                    }
                }
            };
            let fact_id = death_fact_id(character);
            metadata.insert("fact_id".to_owned(), json!(fact_id));
            effects.push(StateEffect::LearnFact {
                fact: Fact::new(
                    fact_id,
                    format!("{} is dead.", lookup(roster, character)?.name()),
                )?,
                source: knowledge_source.clone(),
            });
            let memory_type = match role {
                Role::Informed => MemoryType::Report,
                _ => MemoryType::Observation,
            };
            let summary = format!("{}{what}", told.as_deref().unwrap_or_default());
            (memory_type, summary, verdict)
        }
        NpcEventKind::MissionFailed {
            mission_id,
            summary,
        } => {
            entities.insert(mission_id.clone());
            effects.push(StateEffect::LearnFlag {
                key: mission_failed_flag(mission_id),
                value: true,
            });
            let summary = summary.trim();
            let (memory_type, summary) = match role {
                Role::Informed => (
                    MemoryType::Report,
                    format!(
                        "{}a mission failed: {summary}",
                        told.as_deref().unwrap_or_default()
                    ),
                ),
                _ => (
                    MemoryType::Observation,
                    format!("I saw a mission fail: {summary}"),
                ),
            };
            (memory_type, summary, appraisal(50, -30, 0, 0, 0))
        }
        NpcEventKind::WorldEvent {
            summary,
            entities: involved,
            flag,
        } => {
            entities.extend(involved.iter().cloned());
            if let Some(flag) = flag {
                effects.push(StateEffect::LearnFlag {
                    key: flag.key.clone(),
                    value: flag.value,
                });
            }
            let summary = summary.trim();
            let (memory_type, summary) = match role {
                Role::Informed => (
                    MemoryType::Report,
                    format!("{}{summary}", told.as_deref().unwrap_or_default()),
                ),
                _ => (MemoryType::Observation, format!("I witnessed: {summary}")),
            };
            (memory_type, summary, appraisal(40, 0, 0, 0, 0))
        }
    };

    // Second-hand news lands softer than seeing it.
    if role == Role::Informed {
        verdict.importance = verdict.importance.saturating_sub(10);
        for effect in &mut effects {
            if let StateEffect::AdjustRelationship { delta, .. } = effect {
                *delta = delta.halved();
            }
        }
    }
    effects.retain(|effect| {
        !matches!(effect, StateEffect::AdjustRelationship { toward, delta }
            if delta.is_zero() || toward == me.character_id())
    });

    metadata.insert("event_type".to_owned(), json!(event.kind.name()));
    metadata.insert("perception".to_owned(), json!(role.as_str()));
    if let (Role::Informed, Some(informant)) = (role, informant) {
        entities.insert(informant.clone());
        metadata.insert("informant".to_owned(), json!(informant));
    }
    // The memory is mine; I am not one of its "entities".
    entities.retain(|e| e != me.character_id());

    let mut memory = MemoryEntry::new(
        MemoryEntry::id_for_event(event.event_id, me.character_id()),
        event.session_id,
        me.character_id().clone(),
        memory_type,
        sentence(summary),
        verdict.importance,
        event.world_time,
        event.timestamp,
    )?;
    memory.event_id = Some(event.event_id);
    memory.entities = entities;
    memory.location = event.location.clone();
    memory.emotional_valence = verdict.valence;
    memory.metadata = metadata;
    memory.validate()?;

    Ok(Perception {
        character_id: me.character_id().clone(),
        memory,
        effects,
    })
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::npc::memory::test_support::t;

    pub const SESSION: Uuid = Uuid::from_u128(0xA11CE);

    pub fn npc(id: &str, name: &str, location: Option<&str>) -> CharacterState {
        let mut state =
            CharacterState::new(SESSION, CharacterId::new(id).unwrap(), name, t(0)).unwrap();
        if let Some(location) = location {
            state
                .set_location(Some(LocationId::new(location).unwrap()), t(0))
                .unwrap();
        }
        state
    }

    pub fn roster(states: impl IntoIterator<Item = CharacterState>) -> Roster {
        states
            .into_iter()
            .map(|s| (s.character_id().clone(), s))
            .collect()
    }

    /// Event number `n`: id, world time and timestamp all derive from `n`.
    pub fn event(n: u64, kind: NpcEventKind, audience: Audience) -> NpcEvent {
        NpcEvent {
            event_id: Uuid::from_u128(u128::from(n)),
            session_id: SESSION,
            world_time: n,
            timestamp: t(n as i64),
            location: None,
            kind,
            audience,
        }
    }

    pub fn witnesses(ids: &[&str]) -> Audience {
        Audience::Witnesses {
            witnesses: ids
                .iter()
                .map(|id| CharacterId::new(*id).unwrap())
                .collect(),
        }
    }

    pub fn reported(informant: &str, listeners: &[&str]) -> Audience {
        Audience::Reported {
            informant: EntityId::new(informant).unwrap(),
            listeners: listeners
                .iter()
                .map(|id| CharacterId::new(*id).unwrap())
                .collect(),
        }
    }

    pub fn secret_x() -> Fact {
        Fact::new(
            FactId::new("secret:x").unwrap(),
            "The lab is hidden under the laundry.",
        )
        .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::npc::memory::test_support::{cid, eid, t};
    use crate::npc::relationship::Relationship;

    fn cast() -> Roster {
        roster([
            npc("hank", "Hank Schrader", Some("dea_office")),
            npc("walter", "Walter White", Some("lab")),
            npc("jesse", "Jesse Pinkman", Some("lab")),
            npc("gus", "Gus Fring", Some("restaurant")),
        ])
    }

    fn ids(perceptions: &[Perception]) -> Vec<&str> {
        perceptions
            .iter()
            .map(|p| p.character_id.as_str())
            .collect()
    }

    fn harmed(target: &str) -> NpcEventKind {
        NpcEventKind::Harmed {
            actor: EntityId::player(),
            target: cid(target),
        }
    }

    #[test]
    fn private_event_reaches_only_its_subject() {
        let e = event(
            1,
            NpcEventKind::Helped {
                actor: EntityId::player(),
                target: cid("jesse"),
            },
            Audience::Participants,
        );
        let ps = perceive(&e, &cast()).unwrap();
        assert_eq!(ids(&ps), ["jesse"]);
        let p = &ps[0];
        assert_eq!(p.memory.summary, "The player helped me.");
        assert_eq!(p.memory.memory_type, MemoryType::Interaction);
        assert_eq!(p.memory.event_id, Some(e.event_id));
        assert_eq!(p.memory.session_id, SESSION);
        assert_eq!(p.memory.world_time, 1);
        assert_eq!(p.memory.created_at, t(1));
        assert_eq!(p.memory.importance, 60);
        assert_eq!(p.memory.emotional_valence, 60);
        assert_eq!(p.memory.entities, BTreeSet::from([EntityId::player()]));
        assert_eq!(p.memory.metadata["event_type"], "helped");
        assert_eq!(p.memory.metadata["perception"], "direct");
        assert_eq!(
            p.effects,
            [StateEffect::AdjustRelationship {
                toward: EntityId::player(),
                delta: RelationshipDelta::new(15, -5, 20),
            }]
        );
    }

    #[test]
    fn perceive_is_deterministic() {
        let e = event(2, harmed("jesse"), witnesses(&["walter", "hank"]));
        let a = perceive(&e, &cast()).unwrap();
        let b = perceive(&e, &cast()).unwrap();
        assert_eq!(a, b);
        assert_eq!(ids(&a), ["hank", "jesse", "walter"]);
        assert_eq!(
            a[0].memory.memory_id,
            MemoryEntry::id_for_event(e.event_id, &cid("hank"))
        );
    }

    #[test]
    fn witnesses_react_according_to_their_ties() {
        let mut cast = cast();
        cast.get_mut(&cid("walter"))
            .unwrap()
            .set_relationship(eid("jesse"), Relationship::new(40, 0, 60), t(0))
            .unwrap();
        let e = event(3, harmed("jesse"), witnesses(&["walter", "hank"]));
        let ps = perceive(&e, &cast).unwrap();
        let by_id = |id: &str| ps.iter().find(|p| p.character_id.as_str() == id).unwrap();
        let delta = |p: &Perception| match &p.effects[0] {
            StateEffect::AdjustRelationship { toward, delta } => {
                assert!(toward.is_player());
                *delta
            }
            other => panic!("unexpected effect {other:?}"),
        };

        let victim = by_id("jesse");
        assert_eq!(victim.memory.summary, "The player harmed me.");
        assert_eq!(delta(victim), RelationshipDelta::new(-35, 30, -35));

        let ally = by_id("walter");
        assert_eq!(ally.memory.summary, "I saw the player harm Jesse Pinkman.");
        assert_eq!(ally.memory.memory_type, MemoryType::Observation);
        assert_eq!(ally.memory.importance, 80);
        assert_eq!(
            ally.memory.entities,
            BTreeSet::from([EntityId::player(), eid("jesse")])
        );
        assert_eq!(delta(ally), RelationshipDelta::new(-25, 10, -30));

        let bystander = by_id("hank");
        assert_eq!(bystander.memory.importance, 55);
        assert_eq!(delta(bystander), RelationshipDelta::new(-10, 15, 0));
    }

    #[test]
    fn location_audience_is_everyone_present_and_nobody_else() {
        let mut e = event(4, harmed("jesse"), Audience::Location);
        e.location = Some(LocationId::new("lab").unwrap());
        let ps = perceive(&e, &cast()).unwrap();
        assert_eq!(ids(&ps), ["jesse", "walter"]);
        assert_eq!(ps[1].memory.location, e.location);

        e.location = None;
        assert!(matches!(
            perceive(&e, &cast()),
            Err(NpcError::InvalidEvent(_))
        ));
    }

    #[test]
    fn reported_event_reaches_only_listeners_and_lands_softer() {
        let e = event(5, harmed("jesse"), reported("walter", &["gus", "jesse"]));
        let ps = perceive(&e, &cast()).unwrap();
        // jesse is the subject: he was there, he is not "told".
        assert_eq!(ids(&ps), ["gus"]);
        let p = &ps[0];
        assert_eq!(
            p.memory.summary,
            "Walter White told me that the player harmed Jesse Pinkman."
        );
        assert_eq!(p.memory.memory_type, MemoryType::Report);
        assert_eq!(p.memory.importance, 45);
        assert_eq!(p.memory.metadata["perception"], "reported");
        assert_eq!(p.memory.metadata["informant"], "walter");
        assert!(p.memory.entities.contains(&eid("walter")));
        assert_eq!(
            p.effects,
            [StateEffect::AdjustRelationship {
                toward: EntityId::player(),
                delta: RelationshipDelta::new(-5, 7, 0),
            }]
        );
    }

    #[test]
    fn fact_reveal_teaches_listener_and_eavesdroppers_only() {
        let reveal = NpcEventKind::FactRevealed {
            speaker: EntityId::player(),
            listener: cid("hank"),
            fact: secret_x(),
        };
        let ps = perceive(&event(6, reveal.clone(), witnesses(&["gus"])), &cast()).unwrap();
        assert_eq!(ids(&ps), ["gus", "hank"]);

        let hank = &ps[1];
        assert_eq!(
            hank.memory.summary,
            "The player told me: The lab is hidden under the laundry."
        );
        assert_eq!(hank.memory.memory_type, MemoryType::Revelation);
        assert_eq!(hank.memory.metadata["fact_id"], "secret:x");
        assert_eq!(
            hank.effects,
            [
                StateEffect::LearnFact {
                    fact: secret_x(),
                    source: KnowledgeSource::Told {
                        by: EntityId::player()
                    },
                },
                StateEffect::AdjustRelationship {
                    toward: EntityId::player(),
                    delta: RelationshipDelta::new(5, 0, 0),
                },
            ]
        );

        let gus = &ps[0];
        assert_eq!(
            gus.memory.summary,
            "I overheard the player tell Hank Schrader: The lab is hidden under the laundry."
        );
        assert_eq!(
            gus.effects,
            [StateEffect::LearnFact {
                fact: secret_x(),
                source: KnowledgeSource::Witnessed,
            }]
        );
    }

    #[test]
    fn npc_speaker_must_know_what_it_reveals() {
        let reveal = NpcEventKind::FactRevealed {
            speaker: eid("hank"),
            listener: cid("walter"),
            fact: secret_x(),
        };
        let e = event(7, reveal, Audience::Participants);
        let mut cast = cast();
        assert_eq!(
            perceive(&e, &cast),
            Err(NpcError::UnknownFact {
                character: "hank".into(),
                fact: "secret:x".into(),
            })
        );

        cast.get_mut(&cid("hank"))
            .unwrap()
            .learn_fact(secret_x(), KnowledgeSource::Witnessed, None, t(0))
            .unwrap();
        let ps = perceive(&e, &cast).unwrap();
        assert_eq!(ids(&ps), ["walter"]);
        assert_eq!(
            ps[0].memory.summary,
            "Hank Schrader told me: The lab is hidden under the laundry."
        );
    }

    #[test]
    fn death_is_known_only_to_those_who_saw_or_heard() {
        let mut cast = cast();
        cast.get_mut(&cid("walter"))
            .unwrap()
            .set_relationship(eid("jesse"), Relationship::new(40, 0, 60), t(0))
            .unwrap();
        let died = NpcEventKind::CharacterDied {
            character: cid("jesse"),
            killer: Some(eid("gus")),
        };
        let ps = perceive(
            &event(8, died.clone(), witnesses(&["walter", "gus"])),
            &cast,
        )
        .unwrap();
        // The dead form no memory; the killer is not his own bystander.
        assert_eq!(ids(&ps), ["walter"]);
        let p = &ps[0];
        assert_eq!(p.memory.summary, "I saw Gus Fring kill Jesse Pinkman.");
        assert_eq!(p.memory.importance, 95);
        assert_eq!(
            p.effects,
            [
                StateEffect::AdjustRelationship {
                    toward: eid("gus"),
                    delta: RelationshipDelta::new(-40, 25, -50),
                },
                StateEffect::LearnFact {
                    fact: Fact::new(death_fact_id(&cid("jesse")), "Jesse Pinkman is dead.")
                        .unwrap(),
                    source: KnowledgeSource::Witnessed,
                },
            ]
        );

        let natural = NpcEventKind::CharacterDied {
            character: cid("jesse"),
            killer: None,
        };
        let news = event(9, natural, reported("player", &["hank"]));
        assert!(
            matches!(perceive(&news, &cast), Err(NpcError::InvalidEvent(_))),
            "no rumours about the living"
        );
        cast.get_mut(&cid("jesse")).unwrap().kill(t(8));
        let ps = perceive(&news, &cast).unwrap();
        assert_eq!(
            ps[0].memory.summary,
            "The player told me that Jesse Pinkman died."
        );
        assert_eq!(
            ps[0].effects,
            [StateEffect::LearnFact {
                fact: Fact::new(death_fact_id(&cid("jesse")), "Jesse Pinkman is dead.").unwrap(),
                source: KnowledgeSource::Told {
                    by: EntityId::player()
                },
            }]
        );
    }

    #[test]
    fn mission_failure_and_world_events_teach_flags_to_witnesses() {
        let mut failed = event(
            10,
            NpcEventKind::MissionFailed {
                mission_id: eid("heist"),
                summary: "the alarm went off".into(),
            },
            Audience::Location,
        );
        failed.location = Some(LocationId::new("lab").unwrap());
        let ps = perceive(&failed, &cast()).unwrap();
        assert_eq!(ids(&ps), ["jesse", "walter"]);
        assert_eq!(
            ps[0].memory.summary,
            "I saw a mission fail: the alarm went off"
        );
        assert_eq!(
            ps[0].effects,
            [StateEffect::LearnFlag {
                key: FlagKey::new("mission_failed:heist").unwrap(),
                value: true,
            }]
        );

        let world = event(
            11,
            NpcEventKind::WorldEvent {
                summary: "The laundry burned down.".into(),
                entities: BTreeSet::from([eid("laundry")]),
                flag: Some(FlagChange {
                    key: FlagKey::new("laundry_destroyed").unwrap(),
                    value: true,
                }),
            },
            witnesses(&["gus"]),
        );
        let ps = perceive(&world, &cast()).unwrap();
        assert_eq!(ids(&ps), ["gus"]);
        assert_eq!(
            ps[0].memory.summary,
            "I witnessed: The laundry burned down."
        );
        assert_eq!(ps[0].memory.entities, BTreeSet::from([eid("laundry")]));
        assert_eq!(ps[0].memory.importance, 40);
    }

    #[test]
    fn the_dead_and_incapacitated_perceive_nothing() {
        let mut cast = cast();
        cast.get_mut(&cid("walter")).unwrap().kill(t(0));
        cast.get_mut(&cid("hank"))
            .unwrap()
            .set_status(LifeStatus::Incapacitated, t(0))
            .unwrap();
        let e = event(12, harmed("jesse"), witnesses(&["walter", "hank", "gus"]));
        assert_eq!(ids(&perceive(&e, &cast).unwrap()), ["gus", "jesse"]);

        // An unconscious target is harmed but remembers nothing.
        let e = event(13, harmed("hank"), Audience::Participants);
        assert!(perceive(&e, &cast).unwrap().is_empty());
    }

    #[test]
    fn rejects_malformed_events() {
        let mut cast = cast();
        let unknown = |r: Result<Vec<Perception>, NpcError>| {
            assert!(matches!(r, Err(NpcError::UnknownCharacter(_))), "{r:?}");
        };
        unknown(perceive(
            &event(20, harmed("nobody"), Audience::Participants),
            &cast,
        ));
        unknown(perceive(
            &event(21, harmed("jesse"), witnesses(&["nobody"])),
            &cast,
        ));
        unknown(perceive(
            &event(22, harmed("jesse"), reported("player", &["nobody"])),
            &cast,
        ));

        let invalid = |r: Result<Vec<Perception>, NpcError>| {
            assert!(matches!(r, Err(NpcError::InvalidEvent(_))), "{r:?}");
        };
        let self_harm = NpcEventKind::Harmed {
            actor: eid("jesse"),
            target: cid("jesse"),
        };
        invalid(perceive(
            &event(23, self_harm, Audience::Participants),
            &cast,
        ));
        invalid(perceive(
            &event(24, harmed("jesse"), reported("player", &[])),
            &cast,
        ));

        let blank = NpcEventKind::WorldEvent {
            summary: "   ".into(),
            entities: BTreeSet::new(),
            flag: None,
        };
        assert!(matches!(
            perceive(&event(25, blank, witnesses(&["gus"])), &cast),
            Err(NpcError::InvalidText { .. })
        ));

        cast.get_mut(&cid("jesse")).unwrap().kill(t(0));
        let dead = Err(NpcError::CharacterDead("jesse".into()));
        assert_eq!(
            perceive(&event(26, harmed("jesse"), Audience::Participants), &cast),
            dead
        );
        let dies_again = NpcEventKind::CharacterDied {
            character: cid("jesse"),
            killer: None,
        };
        assert_eq!(
            perceive(&event(27, dies_again, witnesses(&["gus"])), &cast),
            dead
        );
        let dead_speaker = NpcEventKind::Threatened {
            actor: eid("jesse"),
            target: cid("gus"),
        };
        assert_eq!(
            perceive(&event(28, dead_speaker, Audience::Participants), &cast),
            dead
        );
    }

    #[test]
    fn events_round_trip_as_json() {
        let mut e = event(
            30,
            NpcEventKind::FactRevealed {
                speaker: EntityId::player(),
                listener: cid("hank"),
                fact: secret_x(),
            },
            reported("walter", &["gus"]),
        );
        e.location = Some(LocationId::new("lab").unwrap());
        let value = serde_json::to_value(&e).unwrap();
        assert_eq!(value["kind"]["type"], "fact_revealed");
        assert_eq!(value["kind"]["fact"]["fact_id"], "secret:x");
        assert_eq!(value["audience"]["scope"], "reported");
        assert_eq!(value["audience"]["listeners"], json!(["gus"]));
        assert_eq!(
            serde_json::from_value::<NpcEvent>(value.clone()).unwrap(),
            e
        );

        let mut bad = value;
        bad["audience"] = json!({"scope": "everyone"});
        assert!(serde_json::from_value::<NpcEvent>(bad).is_err());
    }
}
