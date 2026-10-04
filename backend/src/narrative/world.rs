//! The narrative layer's view of the played world: what is true right now.
//!
//! This is the *actual* timeline. It only changes through [`NarrativeEvent`]s
//! and plan [`Effect`]s applied by the engine. The cast is fixed when the state
//! is created: nothing here can add a character or bring one back.
//!
//! [`NarrativeEvent`]: super::NarrativeEvent
//! [`Effect`]: super::Effect

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::model::{ActorId, CanonRelation, CharacterId, FactId, FlagId, LocationId, ObjectId};
use super::model::{PLAYER_ID, TruthId};

pub const RELATIONSHIP_MIN: i32 = -100;
pub const RELATIONSHIP_MAX: i32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CharacterStatus {
    #[default]
    Available,
    /// Alive but out of the story for now (captured, unconscious, gone).
    Unavailable,
    /// Terminal.
    Dead,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterFacts {
    #[serde(default)]
    pub status: CharacterStatus,
    #[serde(default)]
    pub location: Option<LocationId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipAxis {
    Trust,
    Fear,
    Affinity,
}

/// How one actor regards another. `fear` is 0..=100, the others -100..=100.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationshipFacts {
    #[serde(default)]
    pub trust: i32,
    #[serde(default)]
    pub fear: i32,
    #[serde(default)]
    pub affinity: i32,
}

impl RelationshipFacts {
    pub fn get(&self, axis: RelationshipAxis) -> i32 {
        match axis {
            RelationshipAxis::Trust => self.trust,
            RelationshipAxis::Fear => self.fear,
            RelationshipAxis::Affinity => self.affinity,
        }
    }

    fn clamped(self) -> Self {
        Self {
            trust: self.trust.clamp(RELATIONSHIP_MIN, RELATIONSHIP_MAX),
            fear: self.fear.clamp(0, RELATIONSHIP_MAX),
            affinity: self.affinity.clamp(RELATIONSHIP_MIN, RELATIONSHIP_MAX),
        }
    }
}

/// An underlying fact of the world ("the creature exists").
///
/// A truth is not an event. It keeps holding whether or not any planned beat
/// about it ever happens, and only an explicit `truth_ended` changes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldTruth {
    pub statement: String,
    pub holds: bool,
    pub canon_relation: CanonRelation,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldFacts {
    /// Reversible booleans. An unset flag reads as `false`.
    #[serde(default)]
    pub flags: BTreeMap<FlagId, bool>,
    #[serde(default)]
    pub characters: BTreeMap<CharacterId, CharacterFacts>,
    /// Every known location, including lost ones.
    #[serde(default)]
    pub locations: BTreeSet<LocationId>,
    /// Locations that can no longer be used. Permanent.
    #[serde(default)]
    pub lost_locations: BTreeSet<LocationId>,
    /// Every known object, including destroyed ones.
    #[serde(default)]
    pub objects: BTreeSet<ObjectId>,
    /// Permanent.
    #[serde(default)]
    pub destroyed_objects: BTreeSet<ObjectId>,
    /// Who has learned each fact. Knowledge is never removed.
    #[serde(default)]
    pub known_facts: BTreeMap<FactId, BTreeSet<ActorId>>,
    /// `relationships[from][to]`.
    #[serde(default)]
    pub relationships: BTreeMap<ActorId, BTreeMap<ActorId, RelationshipFacts>>,
    #[serde(default)]
    pub player_location: Option<LocationId>,
    #[serde(default)]
    pub truths: BTreeMap<TruthId, WorldTruth>,
}

impl WorldFacts {
    pub fn flag(&self, flag: &str) -> bool {
        self.flags.get(flag).copied().unwrap_or(false)
    }

    pub fn character_status(&self, character_id: &str) -> Option<CharacterStatus> {
        self.characters.get(character_id).map(|c| c.status)
    }

    /// True only for a known character who has died; factions and the player
    /// are never dead.
    pub fn is_dead(&self, actor: &str) -> bool {
        self.character_status(actor) == Some(CharacterStatus::Dead)
    }

    pub fn location_lost(&self, location_id: &str) -> bool {
        self.lost_locations.contains(location_id)
    }

    pub fn object_destroyed(&self, object_id: &str) -> bool {
        self.destroyed_objects.contains(object_id)
    }

    pub fn knows(&self, fact_id: &str, actor: &str) -> bool {
        self.known_facts
            .get(fact_id)
            .is_some_and(|actors| actors.contains(actor))
    }

    pub fn relationship(&self, from: &str, to: &str) -> RelationshipFacts {
        self.relationships
            .get(from)
            .and_then(|others| others.get(to))
            .copied()
            .unwrap_or_default()
    }

    /// `None` if no such truth has been established.
    pub fn truth_holds(&self, truth_id: &str) -> Option<bool> {
        self.truths.get(truth_id).map(|t| t.holds)
    }

    // -- Setup helpers: describe the world before the engine starts. ---------

    pub fn add_location(&mut self, location_id: impl Into<LocationId>) {
        self.locations.insert(location_id.into());
    }

    pub fn add_character(&mut self, character_id: impl Into<CharacterId>, location: Option<&str>) {
        self.characters.insert(
            character_id.into(),
            CharacterFacts {
                status: CharacterStatus::Available,
                location: location.map(str::to_owned),
            },
        );
    }

    pub fn add_object(&mut self, object_id: impl Into<ObjectId>) {
        self.objects.insert(object_id.into());
    }

    pub fn add_truth(
        &mut self,
        truth_id: impl Into<TruthId>,
        statement: impl Into<String>,
        canon_relation: CanonRelation,
    ) {
        self.truths.insert(
            truth_id.into(),
            WorldTruth {
                statement: statement.into(),
                holds: true,
                canon_relation,
            },
        );
    }

    // -- Mutations the engine applies after validating ids. -------------------

    pub(crate) fn reveal_fact(&mut self, fact_id: &str, to: &str) {
        self.known_facts
            .entry(fact_id.to_owned())
            .or_default()
            .insert(to.to_owned());
    }

    pub(crate) fn set_relationship(&mut self, from: &str, to: &str, facts: RelationshipFacts) {
        self.relationships
            .entry(from.to_owned())
            .or_default()
            .insert(to.to_owned(), facts.clamped());
    }

    pub(crate) fn adjust_relationship(
        &mut self,
        from: &str,
        to: &str,
        axis: RelationshipAxis,
        delta: i32,
    ) {
        let mut facts = self.relationship(from, to);
        match axis {
            RelationshipAxis::Trust => facts.trust = facts.trust.saturating_add(delta),
            RelationshipAxis::Fear => facts.fear = facts.fear.saturating_add(delta),
            RelationshipAxis::Affinity => facts.affinity = facts.affinity.saturating_add(delta),
        }
        self.set_relationship(from, to, facts);
    }

    /// Establish a truth that does not exist yet. An existing truth is left
    /// alone, so an ended truth stays ended.
    pub(crate) fn establish_truth(&mut self, truth_id: &str, statement: &str) {
        self.truths
            .entry(truth_id.to_owned())
            .or_insert_with(|| WorldTruth {
                statement: statement.to_owned(),
                holds: true,
                canon_relation: CanonRelation::Generated,
            });
    }

    pub(crate) fn end_truth(&mut self, truth_id: &str) {
        if let Some(truth) = self.truths.get_mut(truth_id) {
            truth.holds = false;
        }
    }

    /// Whether `id` names the player rather than a character.
    pub(crate) fn is_player(id: &str) -> bool {
        id == PLAYER_ID
    }
}
