//! `CharacterState` V1: the authoritative runtime state of one NPC.
//!
//! Knowledge is explicit and per-NPC. Nothing here reads `GameSession`: an NPC
//! knows a fact or a world flag only after [`CharacterState::learn_fact`] /
//! [`CharacterState::learn_flag`] was called for *that* NPC. Two NPCs in the
//! same session can therefore know different things.
//!
//! Every mutation takes `now`, bumps `version` and is deterministic.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::ids::{CharacterId, EntityId, FactId, FlagKey, LocationId};
use super::relationship::{Disposition, Relationship, RelationshipDelta};
use super::{MAX_TEXT_LEN, NpcError, clean_text, truncate_text};

pub const CHARACTER_STATE_SCHEMA_VERSION: u32 = 1;

pub const MAX_GOALS: usize = 8;
pub const MAX_KNOWN_FACTS: usize = 256;
pub const MAX_KNOWN_FLAGS: usize = 256;
pub const MAX_RELATIONSHIPS: usize = 64;
pub const MAX_NAME_LEN: usize = 120;

/// Namespace for deriving stable fact ids from canon statements.
const CANON_FACT_NAMESPACE: Uuid = Uuid::from_u128(0x2c0f_6e1d_84a7_4b5e_9f31_7d2a_c4e8_05b9);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifeStatus {
    Alive,
    /// Alive but out of action (unconscious, captured...). Still perceives nothing.
    Incapacitated,
    /// Terminal.
    Dead,
}

/// A discrete, identifiable piece of knowledge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fact {
    pub fact_id: FactId,
    pub statement: String,
}

impl Fact {
    pub fn new(fact_id: FactId, statement: impl AsRef<str>) -> Result<Self, NpcError> {
        Ok(Self {
            fact_id,
            statement: clean_text("fact statement", statement.as_ref(), MAX_TEXT_LEN)?,
        })
    }
}

/// How an NPC came to know something.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KnowledgeSource {
    /// Known from the start (WorldBible `known_facts`).
    Canon,
    /// Saw or overheard it first-hand.
    Witnessed,
    /// Was told by someone.
    Told { by: EntityId },
}

/// A fact as held by one NPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnownFact {
    pub statement: String,
    pub source: KnowledgeSource,
    pub learned_at: DateTime<Utc>,
    /// The event that taught the NPC this fact, if any.
    #[serde(default)]
    pub source_event: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterState {
    schema_version: u32,
    session_id: Uuid,
    character_id: CharacterId,
    name: String,
    /// One-line reference, e.g. "DEA agent". Not used by any rule.
    #[serde(default)]
    role: Option<String>,
    status: LifeStatus,
    #[serde(default)]
    location: Option<LocationId>,
    /// Feelings toward other entities, keyed by entity id (`"player"` included).
    #[serde(default)]
    relationships: BTreeMap<EntityId, Relationship>,
    #[serde(default)]
    goals: Vec<String>,
    /// Everything this NPC knows. Absence means "does not know".
    #[serde(default)]
    knowledge: BTreeMap<FactId, KnownFact>,
    /// World flags *as this NPC believes them*; may lag or differ from the
    /// authoritative `GameSession::world_flags`.
    #[serde(default)]
    known_flags: BTreeMap<FlagKey, bool>,
    /// Bumped on every mutation.
    version: u64,
    updated_at: DateTime<Utc>,
}

impl CharacterState {
    pub fn new(
        session_id: Uuid,
        character_id: CharacterId,
        name: impl AsRef<str>,
        now: DateTime<Utc>,
    ) -> Result<Self, NpcError> {
        Ok(Self {
            schema_version: CHARACTER_STATE_SCHEMA_VERSION,
            session_id,
            character_id,
            name: clean_text("name", name.as_ref(), MAX_NAME_LEN)?,
            role: None,
            status: LifeStatus::Alive,
            location: None,
            relationships: BTreeMap::new(),
            goals: Vec::new(),
            knowledge: BTreeMap::new(),
            known_flags: BTreeMap::new(),
            version: 1,
            updated_at: now,
        })
    }

    /// Build the initial state of an NPC from its WorldBible character.
    ///
    /// Canon `known_facts` become [`KnowledgeSource::Canon`] knowledge with a
    /// fact id derived from the statement, so two characters given the same
    /// canon statement share a fact id. Outgoing relationships get a coarse
    /// starting value from their `kind` label.
    pub fn from_seed(
        session_id: Uuid,
        seed: &CharacterSeed,
        now: DateTime<Utc>,
    ) -> Result<Self, NpcError> {
        let mut state = Self::new(
            session_id,
            CharacterId::new(seed.id.as_str())?,
            &seed.name,
            now,
        )?;
        state.role = seed
            .role
            .as_deref()
            .map(|r| truncate_text(r, MAX_TEXT_LEN))
            .filter(|r| !r.is_empty());
        for goal in seed.goals.iter().take(MAX_GOALS) {
            let goal = truncate_text(&goal.text, MAX_TEXT_LEN);
            if !goal.is_empty() && !state.goals.contains(&goal) {
                state.goals.push(goal);
            }
        }
        for claim in &seed.known_facts {
            let statement = truncate_text(&claim.text, MAX_TEXT_LEN);
            if statement.is_empty() {
                continue;
            }
            state.knowledge.insert(
                canon_fact_id(&statement),
                KnownFact {
                    statement,
                    source: KnowledgeSource::Canon,
                    learned_at: now,
                    source_event: None,
                },
            );
        }
        for rel in seed.relationships.iter().take(MAX_RELATIONSHIPS) {
            if rel.direction != "outgoing" {
                continue;
            }
            let other = EntityId::new(rel.other_id.as_str())?;
            state
                .relationships
                .entry(other)
                .or_insert_with(|| relationship_for_kind(&rel.kind));
        }
        Ok(state)
    }

    /// Check invariants that plain deserialization cannot enforce. Call this
    /// on any state loaded from storage.
    pub fn validate(&self) -> Result<(), NpcError> {
        if self.schema_version != CHARACTER_STATE_SCHEMA_VERSION {
            return Err(NpcError::UnsupportedSchemaVersion(self.schema_version));
        }
        clean_text("name", &self.name, MAX_NAME_LEN)?;
        for (what, len, max) in [
            ("goals", self.goals.len(), MAX_GOALS),
            ("known facts", self.knowledge.len(), MAX_KNOWN_FACTS),
            ("known flags", self.known_flags.len(), MAX_KNOWN_FLAGS),
            ("relationships", self.relationships.len(), MAX_RELATIONSHIPS),
        ] {
            if len > max {
                return Err(NpcError::LimitExceeded { what, max });
            }
        }
        for goal in &self.goals {
            clean_text("goal", goal, MAX_TEXT_LEN)?;
        }
        for fact in self.knowledge.values() {
            clean_text("fact statement", &fact.statement, MAX_TEXT_LEN)?;
        }
        Ok(())
    }

    // -- read access ---------------------------------------------------------

    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    pub fn character_id(&self) -> &CharacterId {
        &self.character_id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn role(&self) -> Option<&str> {
        self.role.as_deref()
    }

    pub fn status(&self) -> LifeStatus {
        self.status
    }

    pub fn is_alive(&self) -> bool {
        self.status != LifeStatus::Dead
    }

    /// Whether this NPC can currently witness events or be told things.
    pub fn can_perceive(&self) -> bool {
        self.status == LifeStatus::Alive
    }

    pub fn location(&self) -> Option<&LocationId> {
        self.location.as_ref()
    }

    pub fn goals(&self) -> &[String] {
        &self.goals
    }

    pub fn knowledge(&self) -> &BTreeMap<FactId, KnownFact> {
        &self.knowledge
    }

    pub fn known_flags(&self) -> &BTreeMap<FlagKey, bool> {
        &self.known_flags
    }

    pub fn relationships(&self) -> &BTreeMap<EntityId, Relationship> {
        &self.relationships
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }

    /// Does this NPC know the fact? Never consults global session state.
    pub fn knows(&self, fact_id: &FactId) -> bool {
        self.knowledge.contains_key(fact_id)
    }

    pub fn known_fact(&self, fact_id: &FactId) -> Option<&KnownFact> {
        self.knowledge.get(fact_id)
    }

    /// The value of a world flag as this NPC believes it; `None` if the NPC
    /// has never heard of it.
    pub fn known_flag(&self, key: &FlagKey) -> Option<bool> {
        self.known_flags.get(key).copied()
    }

    /// Feelings toward `other`; neutral if they have no history.
    pub fn relationship(&self, other: &EntityId) -> Relationship {
        self.relationships.get(other).copied().unwrap_or_default()
    }

    pub fn disposition_toward(&self, other: &EntityId) -> Disposition {
        self.relationship(other).disposition()
    }

    pub fn disposition_toward_player(&self) -> Disposition {
        self.disposition_toward(&EntityId::player())
    }

    // -- mutations -----------------------------------------------------------

    fn touch(&mut self, now: DateTime<Utc>) {
        self.version += 1;
        self.updated_at = now;
    }

    fn require_not_dead(&self) -> Result<(), NpcError> {
        if self.status == LifeStatus::Dead {
            return Err(NpcError::CharacterDead(self.character_id.to_string()));
        }
        Ok(())
    }

    /// Change life status. `Dead` is terminal. Returns whether anything changed.
    pub fn set_status(&mut self, status: LifeStatus, now: DateTime<Utc>) -> Result<bool, NpcError> {
        if self.status == status {
            return Ok(false);
        }
        self.require_not_dead()?;
        self.status = status;
        self.touch(now);
        Ok(true)
    }

    /// Mark the NPC dead. Idempotent; returns whether it was alive before.
    pub fn kill(&mut self, now: DateTime<Utc>) -> bool {
        if self.status == LifeStatus::Dead {
            return false;
        }
        self.status = LifeStatus::Dead;
        self.touch(now);
        true
    }

    /// Move the NPC. Dead characters stay where they fell.
    pub fn set_location(
        &mut self,
        location: Option<LocationId>,
        now: DateTime<Utc>,
    ) -> Result<bool, NpcError> {
        self.require_not_dead()?;
        if self.location == location {
            return Ok(false);
        }
        self.location = location;
        self.touch(now);
        Ok(true)
    }

    /// The NPC learns a fact. Returns `false` (and changes nothing) if it
    /// already knew it — the original source and time are kept.
    pub fn learn_fact(
        &mut self,
        fact: Fact,
        source: KnowledgeSource,
        source_event: Option<Uuid>,
        now: DateTime<Utc>,
    ) -> Result<bool, NpcError> {
        self.require_not_dead()?;
        if self.knowledge.contains_key(&fact.fact_id) {
            return Ok(false);
        }
        if self.knowledge.len() >= MAX_KNOWN_FACTS {
            return Err(NpcError::LimitExceeded {
                what: "known facts",
                max: MAX_KNOWN_FACTS,
            });
        }
        let statement = clean_text("fact statement", &fact.statement, MAX_TEXT_LEN)?;
        self.knowledge.insert(
            fact.fact_id,
            KnownFact {
                statement,
                source,
                learned_at: now,
                source_event,
            },
        );
        self.touch(now);
        Ok(true)
    }

    /// The NPC stops knowing a fact (it was disproved, retconned, or wiped).
    /// Returns whether it knew it.
    pub fn forget_fact(&mut self, fact_id: &FactId, now: DateTime<Utc>) -> bool {
        let removed = self.knowledge.remove(fact_id).is_some();
        if removed {
            self.touch(now);
        }
        removed
    }

    /// The NPC learns (or updates its belief about) a world flag. Returns
    /// whether its belief changed.
    pub fn learn_flag(
        &mut self,
        key: FlagKey,
        value: bool,
        now: DateTime<Utc>,
    ) -> Result<bool, NpcError> {
        self.require_not_dead()?;
        if self.known_flags.get(&key) == Some(&value) {
            return Ok(false);
        }
        if !self.known_flags.contains_key(&key) && self.known_flags.len() >= MAX_KNOWN_FLAGS {
            return Err(NpcError::LimitExceeded {
                what: "known flags",
                max: MAX_KNOWN_FLAGS,
            });
        }
        self.known_flags.insert(key, value);
        self.touch(now);
        Ok(true)
    }

    pub fn forget_flag(&mut self, key: &FlagKey, now: DateTime<Utc>) -> bool {
        let removed = self.known_flags.remove(key).is_some();
        if removed {
            self.touch(now);
        }
        removed
    }

    /// Replace how the NPC feels about `other`.
    pub fn set_relationship(
        &mut self,
        other: EntityId,
        relationship: Relationship,
        now: DateTime<Utc>,
    ) -> Result<(), NpcError> {
        self.require_not_dead()?;
        self.check_relationship_capacity(&other)?;
        self.relationships.insert(other, relationship);
        self.touch(now);
        Ok(())
    }

    /// Shift how the NPC feels about `other`, saturating at the bounds.
    /// Returns the resulting relationship.
    pub fn adjust_relationship(
        &mut self,
        other: &EntityId,
        delta: RelationshipDelta,
        now: DateTime<Utc>,
    ) -> Result<Relationship, NpcError> {
        self.require_not_dead()?;
        if other == &self.character_id {
            return Err(NpcError::InvalidEvent(
                "a character has no relationship with itself".into(),
            ));
        }
        self.check_relationship_capacity(other)?;
        let before = self.relationship(other);
        let mut after = before;
        after.apply(delta);
        if after != before || !self.relationships.contains_key(other) {
            self.relationships.insert(other.clone(), after);
            self.touch(now);
        }
        Ok(after)
    }

    fn check_relationship_capacity(&self, other: &EntityId) -> Result<(), NpcError> {
        if !self.relationships.contains_key(other) && self.relationships.len() >= MAX_RELATIONSHIPS
        {
            return Err(NpcError::LimitExceeded {
                what: "relationships",
                max: MAX_RELATIONSHIPS,
            });
        }
        Ok(())
    }

    /// Add a current goal. Returns `false` if the NPC already has it.
    pub fn add_goal(
        &mut self,
        goal: impl AsRef<str>,
        now: DateTime<Utc>,
    ) -> Result<bool, NpcError> {
        self.require_not_dead()?;
        let goal = clean_text("goal", goal.as_ref(), MAX_TEXT_LEN)?;
        if self.goals.contains(&goal) {
            return Ok(false);
        }
        if self.goals.len() >= MAX_GOALS {
            return Err(NpcError::LimitExceeded {
                what: "goals",
                max: MAX_GOALS,
            });
        }
        self.goals.push(goal);
        self.touch(now);
        Ok(true)
    }

    /// Drop a goal (achieved or abandoned). Returns whether the NPC had it.
    pub fn remove_goal(&mut self, goal: &str, now: DateTime<Utc>) -> bool {
        let before = self.goals.len();
        self.goals.retain(|g| g != goal.trim());
        let removed = self.goals.len() != before;
        if removed {
            self.touch(now);
        }
        removed
    }
}

/// Stable fact id for a canon statement: same statement, same id.
pub fn canon_fact_id(statement: &str) -> FactId {
    let folded = statement
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let digest = Uuid::new_v5(&CANON_FACT_NAMESPACE, folded.as_bytes()).simple();
    FactId::new(format!("canon:{digest}")).expect("canon fact ids are valid identifiers")
}

/// Coarse starting feelings from a WorldBible relationship label.
fn relationship_for_kind(kind: &str) -> Relationship {
    const HOSTILE: [&str; 8] = [
        "enemy",
        "rival",
        "nemesis",
        "adversar",
        "antagonist",
        "hunt",
        "hate",
        "betray",
    ];
    const WARM: [&str; 16] = [
        "ally", "friend", "partner", "family", "brother", "sister", "wife", "husband", "spouse",
        "child", "daughter", "father", "mother", "mentor", "protege", "lover",
    ];
    let kind = kind.to_lowercase();
    if HOSTILE.iter().any(|w| kind.contains(w)) {
        Relationship::new(-30, 10, -40)
    } else if WARM.iter().any(|w| kind.contains(w)) {
        Relationship::new(30, 0, 40)
    } else {
        Relationship::default()
    }
}

// ---------------------------------------------------------------------------
// WorldBible seed
// ---------------------------------------------------------------------------

/// The subset of a WorldBible V1 `Character`
/// (`shared/schemas/universe/v1/world_bible.schema.json`) the NPC layer needs.
/// Unknown fields are ignored so the bible can evolve.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CharacterSeed {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub goals: Vec<SeedClaim>,
    #[serde(default)]
    pub known_facts: Vec<SeedClaim>,
    #[serde(default)]
    pub relationships: Vec<SeedRelationship>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SeedClaim {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SeedRelationship {
    pub other_id: String,
    /// `"outgoing"` or `"incoming"`; only outgoing ones seed feelings.
    pub direction: String,
    pub kind: String,
}

/// Extract the character seeds from a WorldBible V1 JSON document.
pub fn seeds_from_world_bible(bible: &serde_json::Value) -> Result<Vec<CharacterSeed>, NpcError> {
    let characters = bible
        .get("characters")
        .ok_or_else(|| NpcError::InvalidWorldBible("missing `characters`".into()))?;
    serde_json::from_value(characters.clone())
        .map_err(|e| NpcError::InvalidWorldBible(format!("invalid `characters`: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn t(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + secs, 0).unwrap()
    }

    fn npc(id: &str) -> CharacterState {
        CharacterState::new(Uuid::nil(), CharacterId::new(id).unwrap(), id, t(0)).unwrap()
    }

    fn fact(id: &str) -> Fact {
        Fact::new(FactId::new(id).unwrap(), format!("statement of {id}")).unwrap()
    }

    #[test]
    fn serialization_round_trips() {
        let mut s = npc("hank");
        s.set_location(Some(LocationId::new("dea_office").unwrap()), t(1))
            .unwrap();
        s.learn_fact(
            fact("secret:x"),
            KnowledgeSource::Told {
                by: EntityId::player(),
            },
            Some(Uuid::from_u128(7)),
            t(2),
        )
        .unwrap();
        s.learn_flag(FlagKey::new("lab_destroyed").unwrap(), true, t(3))
            .unwrap();
        s.adjust_relationship(&EntityId::player(), RelationshipDelta::new(10, 0, 5), t(4))
            .unwrap();
        s.add_goal("Find Heisenberg", t(5)).unwrap();

        let value = serde_json::to_value(&s).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["character_id"], "hank");
        assert_eq!(value["status"], "alive");
        assert_eq!(value["location"], "dea_office");
        assert_eq!(value["knowledge"]["secret:x"]["source"]["kind"], "told");
        assert_eq!(value["knowledge"]["secret:x"]["source"]["by"], "player");
        assert_eq!(value["known_flags"]["lab_destroyed"], true);
        assert_eq!(value["relationships"]["player"]["trust"], 10);
        assert_eq!(value["version"], 6);

        let back: CharacterState = serde_json::from_value(value).unwrap();
        assert_eq!(back, s);
        back.validate().unwrap();
    }

    #[test]
    fn deserialization_rejects_bad_data() {
        let good = serde_json::to_value(npc("hank")).unwrap();

        let mut bad_id = good.clone();
        bad_id["character_id"] = json!("not valid!");
        assert!(serde_json::from_value::<CharacterState>(bad_id).is_err());

        let mut unknown = good.clone();
        unknown["omniscient"] = json!(true);
        assert!(serde_json::from_value::<CharacterState>(unknown).is_err());

        let mut future = good.clone();
        future["schema_version"] = json!(2);
        let future: CharacterState = serde_json::from_value(future).unwrap();
        assert_eq!(
            future.validate(),
            Err(NpcError::UnsupportedSchemaVersion(2))
        );
    }

    #[test]
    fn alive_dead_transitions() {
        let mut s = npc("gus");
        assert!(s.is_alive() && s.can_perceive());
        assert!(s.set_status(LifeStatus::Incapacitated, t(1)).unwrap());
        assert!(s.is_alive() && !s.can_perceive());
        assert!(!s.set_status(LifeStatus::Incapacitated, t(2)).unwrap());
        assert!(s.set_status(LifeStatus::Alive, t(3)).unwrap());

        assert!(s.kill(t(4)));
        assert_eq!(s.status(), LifeStatus::Dead);
        assert!(!s.is_alive());
        let version = s.version();
        assert!(!s.kill(t(5)), "killing twice is a no-op");
        assert_eq!(s.version(), version);

        // Dead is terminal: no resurrection, no movement, no new knowledge.
        let dead = NpcError::CharacterDead("gus".into());
        assert_eq!(s.set_status(LifeStatus::Alive, t(6)), Err(dead.clone()));
        assert_eq!(
            s.set_location(Some(LocationId::new("lab").unwrap()), t(6)),
            Err(dead.clone())
        );
        assert_eq!(
            s.learn_fact(fact("f"), KnowledgeSource::Witnessed, None, t(6)),
            Err(dead.clone())
        );
        assert_eq!(
            s.adjust_relationship(&EntityId::player(), RelationshipDelta::new(1, 0, 0), t(6)),
            Err(dead)
        );
        assert_eq!(s.version(), version);
    }

    #[test]
    fn location_update_bumps_version() {
        let mut s = npc("jesse");
        assert_eq!(s.location(), None);
        let lab = LocationId::new("lab").unwrap();
        assert!(s.set_location(Some(lab.clone()), t(10)).unwrap());
        assert_eq!(s.location(), Some(&lab));
        assert_eq!((s.version(), s.updated_at()), (2, t(10)));
        assert!(!s.set_location(Some(lab), t(11)).unwrap());
        assert_eq!((s.version(), s.updated_at()), (2, t(10)));
        assert!(s.set_location(None, t(12)).unwrap());
        assert_eq!(s.location(), None);
    }

    #[test]
    fn learn_and_forget_fact() {
        let mut s = npc("hank");
        let id = FactId::new("secret:x").unwrap();
        assert!(!s.knows(&id));
        assert!(
            s.learn_fact(fact("secret:x"), KnowledgeSource::Witnessed, None, t(1))
                .unwrap()
        );
        assert!(s.knows(&id));
        assert_eq!(
            s.known_fact(&id).unwrap().statement,
            "statement of secret:x"
        );
        assert!(s.forget_fact(&id, t(2)));
        assert!(!s.knows(&id));
        assert!(!s.forget_fact(&id, t(3)));
        assert_eq!(s.version(), 3);
    }

    #[test]
    fn duplicate_facts_keep_the_original() {
        let mut s = npc("hank");
        s.learn_fact(fact("secret:x"), KnowledgeSource::Witnessed, None, t(1))
            .unwrap();
        let version = s.version();
        let again = Fact::new(FactId::new("secret:x").unwrap(), "a different wording").unwrap();
        let learned = s
            .learn_fact(
                again,
                KnowledgeSource::Told {
                    by: EntityId::player(),
                },
                None,
                t(2),
            )
            .unwrap();
        assert!(!learned);
        assert_eq!(s.knowledge().len(), 1);
        assert_eq!(s.version(), version);
        let known = s.known_fact(&FactId::new("secret:x").unwrap()).unwrap();
        assert_eq!(known.source, KnowledgeSource::Witnessed);
        assert_eq!(known.learned_at, t(1));
    }

    #[test]
    fn knowledge_is_isolated_between_npcs() {
        let mut hank = npc("hank");
        let walter = npc("walter");
        let id = FactId::new("secret:x").unwrap();
        let flag = FlagKey::new("lab_destroyed").unwrap();
        hank.learn_fact(fact("secret:x"), KnowledgeSource::Witnessed, None, t(1))
            .unwrap();
        hank.learn_flag(flag.clone(), true, t(1)).unwrap();
        assert!(hank.knows(&id));
        assert!(!walter.knows(&id));
        assert_eq!(hank.known_flag(&flag), Some(true));
        assert_eq!(walter.known_flag(&flag), None);
    }

    #[test]
    fn flags_can_change_and_be_forgotten() {
        let mut s = npc("hank");
        let flag = FlagKey::new("door_open").unwrap();
        assert!(s.learn_flag(flag.clone(), true, t(1)).unwrap());
        assert!(!s.learn_flag(flag.clone(), true, t(2)).unwrap());
        assert!(s.learn_flag(flag.clone(), false, t(3)).unwrap());
        assert_eq!(s.known_flag(&flag), Some(false));
        assert!(s.forget_flag(&flag, t(4)));
        assert_eq!(s.known_flag(&flag), None);
    }

    #[test]
    fn relationship_updates_and_disposition() {
        let mut s = npc("jesse");
        let player = EntityId::player();
        assert_eq!(s.disposition_toward_player(), Disposition::Neutral);
        let r = s
            .adjust_relationship(&player, RelationshipDelta::new(15, -5, 20), t(1))
            .unwrap();
        assert_eq!((r.trust(), r.fear(), r.affinity()), (15, 0, 20));
        assert_eq!(s.disposition_toward_player(), Disposition::Friendly);
        s.adjust_relationship(&player, RelationshipDelta::new(-200, 200, -200), t(2))
            .unwrap();
        assert_eq!(s.relationship(&player), Relationship::new(-100, 100, -100));
        assert_eq!(s.disposition_toward_player(), Disposition::Hostile);

        let me = EntityId::new("jesse").unwrap();
        assert!(matches!(
            s.adjust_relationship(&me, RelationshipDelta::new(1, 0, 0), t(3)),
            Err(NpcError::InvalidEvent(_))
        ));
    }

    #[test]
    fn limits_are_enforced() {
        let mut s = npc("hank");
        for i in 0..MAX_GOALS {
            assert!(s.add_goal(format!("goal {i}"), t(1)).unwrap());
        }
        assert!(!s.add_goal("goal 0", t(1)).unwrap());
        assert!(matches!(
            s.add_goal("one too many", t(1)),
            Err(NpcError::LimitExceeded { what: "goals", .. })
        ));
        assert!(s.remove_goal("goal 0", t(2)));
        assert!(s.add_goal("one too many", t(3)).unwrap());

        for i in 0..MAX_KNOWN_FACTS {
            s.learn_fact(
                fact(&format!("f{i}")),
                KnowledgeSource::Witnessed,
                None,
                t(4),
            )
            .unwrap();
        }
        assert!(matches!(
            s.learn_fact(fact("overflow"), KnowledgeSource::Witnessed, None, t(4)),
            Err(NpcError::LimitExceeded {
                what: "known facts",
                ..
            })
        ));
        assert!(s.add_goal("   ", t(5)).is_err());
        assert!(Fact::new(FactId::new("f").unwrap(), "x".repeat(MAX_TEXT_LEN + 1)).is_err());
    }

    #[test]
    fn builds_from_world_bible_character() {
        let bible = json!({
            "universe_id": "breaking_bad",
            "characters": [
                {
                    "id": "hank_schrader",
                    "name": "Hank Schrader",
                    "role": "DEA agent hunting Heisenberg",
                    "personality_traits": ["brash"],
                    "goals": [{"text": "Catch Heisenberg", "classification": "canon", "source_refs": ["s1"]}],
                    "known_facts": [
                        {"text": "Blue meth is spreading in Albuquerque", "classification": "canon", "source_refs": ["s1"]},
                        {"text": "Walter White is a chemistry teacher", "classification": "canon", "source_refs": ["s1"]}
                    ],
                    "status": "Investigating",
                    "classification": "canon",
                    "source_refs": ["s1"],
                    "relationships": [
                        {"other_id": "walter_white", "direction": "outgoing", "kind": "brother-in-law", "description": "d", "classification": "canon", "source_refs": []},
                        {"other_id": "gus_fring", "direction": "outgoing", "kind": "rival investigator target", "description": "d", "classification": "canon", "source_refs": []},
                        {"other_id": "jesse_pinkman", "direction": "incoming", "kind": "enemy", "description": "d", "classification": "canon", "source_refs": []}
                    ]
                },
                {
                    "id": "walter_white",
                    "name": "Walter White",
                    "role": "Chemistry teacher",
                    "goals": [],
                    "known_facts": [
                        {"text": "walter white is a  chemistry teacher", "classification": "canon", "source_refs": ["s1"]},
                        {"text": "Walter White is Heisenberg", "classification": "canon", "source_refs": ["s1"]}
                    ]
                }
            ]
        });
        let seeds = seeds_from_world_bible(&bible).unwrap();
        assert_eq!(seeds.len(), 2);
        let hank = CharacterState::from_seed(Uuid::nil(), &seeds[0], t(0)).unwrap();
        let walter = CharacterState::from_seed(Uuid::nil(), &seeds[1], t(0)).unwrap();
        hank.validate().unwrap();

        assert_eq!(hank.character_id().as_str(), "hank_schrader");
        assert_eq!(hank.name(), "Hank Schrader");
        assert_eq!(hank.role(), Some("DEA agent hunting Heisenberg"));
        assert_eq!(hank.goals(), ["Catch Heisenberg"]);
        assert_eq!(hank.status(), LifeStatus::Alive);

        // Shared canon statements share a fact id; private canon stays private.
        let teacher = canon_fact_id("Walter White is a chemistry teacher");
        let heisenberg = canon_fact_id("Walter White is Heisenberg");
        assert!(hank.knows(&teacher) && walter.knows(&teacher));
        assert!(walter.knows(&heisenberg));
        assert!(!hank.knows(&heisenberg));
        assert_eq!(
            hank.known_fact(&teacher).unwrap().source,
            KnowledgeSource::Canon
        );

        let walt = EntityId::new("walter_white").unwrap();
        assert!(hank.relationship(&walt).is_ally());
        assert_eq!(
            hank.disposition_toward(&EntityId::new("gus_fring").unwrap()),
            Disposition::Hostile
        );
        // Incoming relationships say how *others* feel; they seed nothing.
        assert!(
            !hank
                .relationships()
                .contains_key(&EntityId::new("jesse_pinkman").unwrap())
        );
    }

    #[test]
    fn rejects_malformed_world_bible() {
        assert!(matches!(
            seeds_from_world_bible(&json!({})),
            Err(NpcError::InvalidWorldBible(_))
        ));
        assert!(matches!(
            seeds_from_world_bible(&json!({"characters": [{"id": "x"}]})),
            Err(NpcError::InvalidWorldBible(_))
        ));
        let seed = CharacterSeed {
            id: "Bad Id".into(),
            name: "x".into(),
            role: None,
            goals: vec![],
            known_facts: vec![],
            relationships: vec![],
        };
        assert!(matches!(
            CharacterState::from_seed(Uuid::nil(), &seed, t(0)),
            Err(NpcError::InvalidIdentifier { .. })
        ));
    }
}
