//! `MemoryEntry` V1 and deterministic relevance scoring.
//!
//! A memory is one NPC's record of one thing it experienced, saw or was told.
//! Retrieval is always bounded ([`MAX_QUERY_LIMIT`]) and ranked by an integer
//! score, so the same store contents and query always return the same list.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::ids::{CharacterId, EntityId, LocationId};
use super::{MAX_TEXT_LEN, NpcError, clean_text};

pub const MEMORY_SCHEMA_VERSION: u32 = 1;

/// Hard cap on memories returned by any query.
pub const MAX_QUERY_LIMIT: usize = 10;
/// Limit used when a query does not ask for one.
pub const DEFAULT_QUERY_LIMIT: usize = 5;
pub const MAX_IMPORTANCE: u8 = 100;
pub const MAX_MEMORY_ENTITIES: usize = 16;
pub const MAX_METADATA_KEYS: usize = 16;
/// Query keywords considered by the scorer.
pub const MAX_QUERY_KEYWORDS: usize = 16;

/// Namespace for deriving memory ids from `(event_id, character_id)`.
const MEMORY_ID_NAMESPACE: Uuid = Uuid::from_u128(0x7b3e_91c4_5d02_4f8a_b6e7_1a9c_3f50_d2e8);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    /// Something done to or with this NPC.
    Interaction,
    /// Something this NPC saw happen to someone or something else.
    Observation,
    /// A fact this NPC was told or overheard.
    Revelation,
    /// Second-hand news of an event this NPC did not see.
    Report,
}

impl MemoryType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interaction => "interaction",
            Self::Observation => "observation",
            Self::Revelation => "revelation",
            Self::Report => "report",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryEntry {
    pub schema_version: u32,
    pub memory_id: Uuid,
    pub session_id: Uuid,
    /// The NPC that holds this memory.
    pub character_id: CharacterId,
    /// The event this memory was extracted from, if any.
    #[serde(default)]
    pub event_id: Option<Uuid>,
    pub memory_type: MemoryType,
    /// Short natural-language summary from the NPC's point of view.
    pub summary: String,
    /// Everyone and everything involved (entity ids).
    #[serde(default)]
    pub entities: BTreeSet<EntityId>,
    /// Where it happened, if known.
    #[serde(default)]
    pub location: Option<LocationId>,
    /// 0 (trivia) ..= 100 (life-changing).
    pub importance: u8,
    /// -100 (traumatic) ..= 100 (joyful).
    pub emotional_valence: i8,
    /// Session event sequence at which it happened (`WorldEvent::sequence`).
    pub world_time: u64,
    /// Wall-clock time the memory was formed.
    pub created_at: DateTime<Utc>,
    /// `false` once invalidated; invalid memories are never retrieved.
    pub valid: bool,
    /// Small free-form extras (e.g. `fact_id`, `informant`).
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

impl MemoryEntry {
    /// Build a valid memory with no entities, location or metadata.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        memory_id: Uuid,
        session_id: Uuid,
        character_id: CharacterId,
        memory_type: MemoryType,
        summary: impl AsRef<str>,
        importance: u8,
        world_time: u64,
        created_at: DateTime<Utc>,
    ) -> Result<Self, NpcError> {
        let entry = Self {
            schema_version: MEMORY_SCHEMA_VERSION,
            memory_id,
            session_id,
            character_id,
            event_id: None,
            memory_type,
            summary: clean_text("memory summary", summary.as_ref(), MAX_TEXT_LEN)?,
            entities: BTreeSet::new(),
            location: None,
            importance,
            emotional_valence: 0,
            world_time,
            created_at,
            valid: true,
            metadata: BTreeMap::new(),
        };
        entry.validate()?;
        Ok(entry)
    }

    /// Deterministic memory id for the memory `character_id` forms of `event_id`.
    /// Storing the same event's memory twice is therefore idempotent.
    pub fn id_for_event(event_id: Uuid, character_id: &CharacterId) -> Uuid {
        let mut name = event_id.as_bytes().to_vec();
        name.extend_from_slice(character_id.as_str().as_bytes());
        Uuid::new_v5(&MEMORY_ID_NAMESPACE, &name)
    }

    /// Check every bound. Stores call this before accepting an entry.
    pub fn validate(&self) -> Result<(), NpcError> {
        if self.schema_version != MEMORY_SCHEMA_VERSION {
            return Err(NpcError::UnsupportedSchemaVersion(self.schema_version));
        }
        let summary = clean_text("memory summary", &self.summary, MAX_TEXT_LEN)?;
        if summary != self.summary {
            return Err(NpcError::InvalidText {
                field: "memory summary",
                reason: "must be trimmed".into(),
            });
        }
        if self.importance > MAX_IMPORTANCE {
            return Err(NpcError::LimitExceeded {
                what: "memory importance",
                max: MAX_IMPORTANCE as usize,
            });
        }
        if !(-100..=100).contains(&self.emotional_valence) {
            return Err(NpcError::LimitExceeded {
                what: "memory emotional_valence magnitude",
                max: 100,
            });
        }
        if self.entities.len() > MAX_MEMORY_ENTITIES {
            return Err(NpcError::LimitExceeded {
                what: "memory entities",
                max: MAX_MEMORY_ENTITIES,
            });
        }
        if self.metadata.len() > MAX_METADATA_KEYS {
            return Err(NpcError::LimitExceeded {
                what: "memory metadata keys",
                max: MAX_METADATA_KEYS,
            });
        }
        Ok(())
    }
}

/// What an NPC is being asked to recall. All signals are optional; with none,
/// ranking falls back to importance and recency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryQuery {
    pub session_id: Uuid,
    pub character_id: CharacterId,
    /// Where the NPC is now.
    #[serde(default)]
    pub location: Option<LocationId>,
    /// Who or what the current situation involves.
    #[serde(default)]
    pub entities: BTreeSet<EntityId>,
    /// The event being reacted to, if any.
    #[serde(default)]
    pub recent_event: Option<Uuid>,
    /// Free text / keywords describing the situation.
    #[serde(default)]
    pub text: Option<String>,
    /// Current session event sequence, for recency. Defaults to the newest
    /// candidate memory.
    #[serde(default)]
    pub world_time: Option<u64>,
    /// Requested result size; clamped to `1..=MAX_QUERY_LIMIT`.
    pub limit: usize,
}

impl MemoryQuery {
    pub fn new(session_id: Uuid, character_id: CharacterId) -> Self {
        Self {
            session_id,
            character_id,
            location: None,
            entities: BTreeSet::new(),
            recent_event: None,
            text: None,
            world_time: None,
            limit: DEFAULT_QUERY_LIMIT,
        }
    }

    pub fn with_location(mut self, location: LocationId) -> Self {
        self.location = Some(location);
        self
    }

    pub fn with_entity(mut self, entity: EntityId) -> Self {
        self.entities.insert(entity);
        self
    }

    pub fn with_recent_event(mut self, event_id: Uuid) -> Self {
        self.recent_event = Some(event_id);
        self
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    pub fn with_world_time(mut self, world_time: u64) -> Self {
        self.world_time = Some(world_time);
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// The effective, bounded result size.
    pub fn bounded_limit(&self) -> usize {
        clamp_limit(self.limit)
    }

    /// Lowercased, de-duplicated keywords from `text`.
    pub fn keywords(&self) -> Vec<String> {
        keywords(self.text.as_deref().unwrap_or_default())
    }
}

/// Clamp a requested result size into `1..=MAX_QUERY_LIMIT`.
pub fn clamp_limit(limit: usize) -> usize {
    limit.clamp(1, MAX_QUERY_LIMIT)
}

/// A retrieved memory with the score that ranked it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredMemory {
    pub memory: MemoryEntry,
    pub score: u32,
}

const STOPWORDS: [&str; 24] = [
    "the", "and", "that", "this", "with", "was", "were", "for", "you", "your", "about", "from",
    "have", "has", "had", "what", "who", "how", "why", "did", "does", "are", "but", "not",
];

/// Tokenize free text into scoring keywords: lowercase alphanumeric runs of
/// 3+ chars, minus stopwords, de-duplicated in order, capped.
pub fn keywords(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for token in text
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
    {
        if token.chars().count() < 3 || STOPWORDS.contains(&token.as_str()) || out.contains(&token)
        {
            continue;
        }
        out.push(token);
        if out.len() == MAX_QUERY_KEYWORDS {
            break;
        }
    }
    out
}

const ENTITY_WEIGHT: u32 = 40;
const MAX_ENTITY_HITS: u32 = 3;
const KEYWORD_WEIGHT: u32 = 15;
const MAX_KEYWORD_HITS: u32 = 4;
const LOCATION_BONUS: u32 = 20;
const RECENT_EVENT_BONUS: u32 = 50;
const RECENCY_WINDOW: u64 = 30;

/// Deterministic relevance of `memory` to `query`.
///
/// ```text
/// importance                         0..=100
/// + 40 per shared entity             (max 3)
/// + 15 per query keyword in summary  (max 4)
/// + 20 if it happened at the query location
/// + 50 if it is the memory of `recent_event`
/// + recency: 30 minus events elapsed (floor 0)
/// ```
///
/// `now` is the current session event sequence.
pub fn score_memory(
    memory: &MemoryEntry,
    query: &MemoryQuery,
    keywords: &[String],
    now: u64,
) -> u32 {
    let mut score = u32::from(memory.importance.min(MAX_IMPORTANCE));

    let entity_hits = query
        .entities
        .iter()
        .filter(|e| memory.entities.contains(*e))
        .count() as u32;
    score += ENTITY_WEIGHT * entity_hits.min(MAX_ENTITY_HITS);

    if !keywords.is_empty() {
        let memory_words = self::keywords_unbounded(&memory.summary);
        let keyword_hits = keywords
            .iter()
            .filter(|k| memory_words.contains(k.as_str()))
            .count() as u32;
        score += KEYWORD_WEIGHT * keyword_hits.min(MAX_KEYWORD_HITS);
    }

    if query.location.is_some() && query.location == memory.location {
        score += LOCATION_BONUS;
    }
    if query.recent_event.is_some() && query.recent_event == memory.event_id {
        score += RECENT_EVENT_BONUS;
    }

    let age = now.saturating_sub(memory.world_time);
    score += RECENCY_WINDOW.saturating_sub(age) as u32;
    score
}

fn keywords_unbounded(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Newest first; ties broken by creation time, then id, so order is total.
pub fn recency_order(a: &MemoryEntry, b: &MemoryEntry) -> std::cmp::Ordering {
    b.world_time
        .cmp(&a.world_time)
        .then(b.created_at.cmp(&a.created_at))
        .then(a.memory_id.cmp(&b.memory_id))
}

/// Rank candidate memories for a query and keep the bounded top results.
///
/// Candidates belonging to another session or character, and invalidated
/// memories, are dropped — a store bug can never leak another NPC's memory
/// through this function. Shared by every [`super::store::MemoryStore`]
/// implementation so they rank identically.
pub fn rank_memories(
    candidates: impl IntoIterator<Item = MemoryEntry>,
    query: &MemoryQuery,
) -> Vec<ScoredMemory> {
    let candidates: Vec<MemoryEntry> = candidates
        .into_iter()
        .filter(|m| {
            m.valid && m.session_id == query.session_id && m.character_id == query.character_id
        })
        .collect();
    let now = query
        .world_time
        .unwrap_or_else(|| candidates.iter().map(|m| m.world_time).max().unwrap_or(0));
    let keywords = query.keywords();
    let mut scored: Vec<ScoredMemory> = candidates
        .into_iter()
        .map(|memory| ScoredMemory {
            score: score_memory(&memory, query, &keywords, now),
            memory,
        })
        .collect();
    scored.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| recency_order(&a.memory, &b.memory))
    });
    scored.truncate(query.bounded_limit());
    scored
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use chrono::TimeZone;

    pub fn t(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + secs, 0).unwrap()
    }

    pub fn cid(id: &str) -> CharacterId {
        CharacterId::new(id).unwrap()
    }

    pub fn eid(id: &str) -> EntityId {
        EntityId::new(id).unwrap()
    }

    /// A memory for `character` with id `n`, formed at world time `n`.
    pub fn memory(character: &str, n: u64, summary: &str, importance: u8) -> MemoryEntry {
        MemoryEntry::new(
            Uuid::from_u128(u128::from(n)),
            Uuid::nil(),
            cid(character),
            MemoryType::Observation,
            summary,
            importance,
            n,
            t(n as i64),
        )
        .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use serde_json::json;

    #[test]
    fn creation_and_serialization() {
        let mut m = memory("hank", 3, "  The player threatened me.  ", 75);
        assert_eq!(m.summary, "The player threatened me.");
        m.event_id = Some(Uuid::from_u128(99));
        m.entities.insert(EntityId::player());
        m.location = Some(LocationId::new("dea_office").unwrap());
        m.emotional_valence = -70;
        m.metadata.insert("fact_id".into(), json!("secret:x"));
        m.validate().unwrap();

        let value = serde_json::to_value(&m).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["character_id"], "hank");
        assert_eq!(value["memory_type"], "observation");
        assert_eq!(value["entities"], json!(["player"]));
        assert_eq!(value["world_time"], 3);
        assert_eq!(value["valid"], true);
        assert_eq!(serde_json::from_value::<MemoryEntry>(value).unwrap(), m);
    }

    #[test]
    fn validation_rejects_out_of_bounds_entries() {
        let good = memory("hank", 1, "ok", 10);
        assert!(
            MemoryEntry::new(
                Uuid::nil(),
                Uuid::nil(),
                cid("hank"),
                MemoryType::Report,
                "   ",
                1,
                1,
                t(0)
            )
            .is_err()
        );

        let mut m = good.clone();
        m.importance = 101;
        assert!(m.validate().is_err());

        let mut m = good.clone();
        m.emotional_valence = -101;
        assert!(m.validate().is_err());

        let mut m = good.clone();
        m.summary = "x".repeat(MAX_TEXT_LEN + 1);
        assert!(m.validate().is_err());

        let mut m = good.clone();
        m.schema_version = 2;
        assert_eq!(m.validate(), Err(NpcError::UnsupportedSchemaVersion(2)));

        let mut m = good.clone();
        for i in 0..=MAX_MEMORY_ENTITIES {
            m.entities.insert(eid(&format!("e{i}")));
        }
        assert!(m.validate().is_err());

        let mut bad = serde_json::to_value(&good).unwrap();
        bad["character_id"] = json!("no spaces allowed");
        assert!(serde_json::from_value::<MemoryEntry>(bad).is_err());
    }

    #[test]
    fn event_memory_ids_are_deterministic_and_per_character() {
        let event = Uuid::from_u128(42);
        let a = MemoryEntry::id_for_event(event, &cid("hank"));
        assert_eq!(a, MemoryEntry::id_for_event(event, &cid("hank")));
        assert_ne!(a, MemoryEntry::id_for_event(event, &cid("walter")));
        assert_ne!(
            a,
            MemoryEntry::id_for_event(Uuid::from_u128(43), &cid("hank"))
        );
    }

    #[test]
    fn keywords_are_normalized() {
        assert_eq!(
            keywords("What did the PLAYER say about the lab? The lab!"),
            ["player", "say", "lab"]
        );
        assert!(keywords("").is_empty());
        let many = (0..40).map(|i| format!("word{i}")).collect::<Vec<_>>();
        assert_eq!(keywords(&many.join(" ")).len(), MAX_QUERY_KEYWORDS);
    }

    #[test]
    fn limit_is_clamped() {
        let q = MemoryQuery::new(Uuid::nil(), cid("hank"));
        assert_eq!(q.bounded_limit(), DEFAULT_QUERY_LIMIT);
        assert_eq!(q.clone().with_limit(0).bounded_limit(), 1);
        assert_eq!(q.with_limit(10_000).bounded_limit(), MAX_QUERY_LIMIT);
    }

    #[test]
    fn score_components() {
        let mut m = memory("hank", 10, "The player threatened Jesse at the lab.", 50);
        m.entities.extend([EntityId::player(), eid("jesse")]);
        m.location = Some(LocationId::new("lab").unwrap());
        m.event_id = Some(Uuid::from_u128(5));
        let base = MemoryQuery::new(Uuid::nil(), cid("hank"));

        // importance 50 + recency (30 - 20 elapsed) = 60
        assert_eq!(score_memory(&m, &base, &[], 30), 60);
        // far in the past: recency floors at 0
        assert_eq!(score_memory(&m, &base, &[], 1_000), 50);

        let q = base
            .clone()
            .with_entity(eid("jesse"))
            .with_entity(eid("gus"));
        assert_eq!(score_memory(&m, &q, &[], 1_000), 90);

        let q = base.clone().with_text("who threatened jesse near the LAB");
        assert_eq!(score_memory(&m, &q, &q.keywords(), 1_000), 50 + 45);

        let q = base.clone().with_location(LocationId::new("lab").unwrap());
        assert_eq!(score_memory(&m, &q, &[], 1_000), 70);

        let q = base.clone().with_recent_event(Uuid::from_u128(5));
        assert_eq!(score_memory(&m, &q, &[], 1_000), 100);

        // A memory with no location/event never matches an unset query field.
        let plain = memory("hank", 10, "Nothing much.", 50);
        assert_eq!(score_memory(&plain, &base, &[], 1_000), 50);
    }

    #[test]
    fn ranking_is_deterministic_bounded_and_scoped() {
        let mut candidates: Vec<MemoryEntry> = (1..=30)
            .map(|n| memory("hank", n, &format!("Routine patrol number {n}."), 20))
            .collect();
        let mut relevant = memory("hank", 2, "The player threatened Jesse.", 20);
        relevant.memory_id = Uuid::from_u128(1_000);
        relevant.entities.insert(eid("jesse"));
        candidates.push(relevant.clone());
        // Never returned: wrong NPC, wrong session, invalidated.
        let mut other_npc = memory("walter", 30, "Jesse came by.", 100);
        other_npc.memory_id = Uuid::from_u128(1_001);
        other_npc.entities.insert(eid("jesse"));
        let mut other_session = other_npc.clone();
        other_session.character_id = cid("hank");
        other_session.session_id = Uuid::from_u128(9);
        other_session.memory_id = Uuid::from_u128(1_002);
        let mut invalid = other_npc.clone();
        invalid.character_id = cid("hank");
        invalid.valid = false;
        invalid.memory_id = Uuid::from_u128(1_003);
        candidates.extend([other_npc, other_session, invalid]);

        let query = MemoryQuery::new(Uuid::nil(), cid("hank"))
            .with_entity(eid("jesse"))
            .with_limit(50);
        let ranked = rank_memories(candidates.clone(), &query);
        assert_eq!(ranked.len(), MAX_QUERY_LIMIT);
        assert_eq!(ranked[0].memory, relevant);
        assert!(ranked.iter().all(|s| s.memory.character_id == cid("hank")
            && s.memory.session_id == Uuid::nil()
            && s.memory.valid));
        assert!(ranked.windows(2).all(|w| w[0].score >= w[1].score));
        // After the relevant one, the newest routine memories win on recency.
        assert_eq!(ranked[1].memory.world_time, 30);

        candidates.reverse();
        assert_eq!(rank_memories(candidates, &query), ranked);
    }

    #[test]
    fn ties_break_newest_first_then_by_id() {
        let old = memory("hank", 1, "Same.", 10);
        let mut new = memory("hank", 1, "Same.", 10);
        new.memory_id = Uuid::from_u128(2);
        new.created_at = t(50);
        let mut twin = new.clone();
        twin.memory_id = Uuid::from_u128(3);
        let query = MemoryQuery::new(Uuid::nil(), cid("hank")).with_world_time(100);
        let ranked = rank_memories([twin.clone(), old.clone(), new.clone()], &query);
        let ids: Vec<Uuid> = ranked.iter().map(|s| s.memory.memory_id).collect();
        assert_eq!(ids, [new.memory_id, twin.memory_id, old.memory_id]);
    }
}
