//! `NpcContext`: the bounded slice of one NPC handed to a later consumer
//! (dialogue, Director). It contains only what *this* NPC knows and remembers,
//! and every list in it has a hard cap — an NPC's full history never leaves
//! the store.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::NpcError;
use super::ids::{CharacterId, EntityId, FactId, FlagKey, LocationId};
use super::memory::{MemoryQuery, ScoredMemory};
use super::relationship::{Disposition, Relationship};
use super::state::{CharacterState, LifeStatus};
use super::store::MemoryStore;

pub const MAX_CONTEXT_FACTS: usize = 12;
pub const MAX_CONTEXT_FLAGS: usize = 16;
pub const MAX_CONTEXT_RELATIONSHIPS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextFact {
    pub fact_id: FactId,
    pub statement: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NpcContext {
    pub character_id: CharacterId,
    pub name: String,
    pub role: Option<String>,
    pub status: LifeStatus,
    pub location: Option<LocationId>,
    pub disposition_toward_player: Disposition,
    /// Feelings toward the player and the entities named in the query.
    pub relationships: BTreeMap<EntityId, Relationship>,
    pub goals: Vec<String>,
    /// Facts this NPC knows, most relevant to the query first.
    pub known_facts: Vec<ContextFact>,
    /// World flags as this NPC believes them.
    pub known_flags: BTreeMap<FlagKey, bool>,
    /// Retrieved memories, best first; at most `MAX_QUERY_LIMIT`.
    pub memories: Vec<ScoredMemory>,
}

fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Up to [`MAX_CONTEXT_FACTS`] of the NPC's facts: those sharing words with
/// the query text or entity ids first, then the most recently learned.
fn select_facts(state: &CharacterState, query: &MemoryQuery) -> Vec<ContextFact> {
    let mut wanted: BTreeSet<String> = query.keywords().into_iter().collect();
    for entity in &query.entities {
        wanted.extend(words(entity.as_str()));
    }
    let mut facts: Vec<_> = state
        .knowledge()
        .iter()
        .map(|(id, fact)| {
            let mut have = words(&fact.statement);
            have.extend(words(id.as_str()));
            let hits = wanted.iter().filter(|w| have.contains(*w)).count();
            (hits, fact.learned_at, id, fact)
        })
        .collect();
    facts.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(b.2)));
    facts
        .into_iter()
        .take(MAX_CONTEXT_FACTS)
        .map(|(_, _, id, fact)| ContextFact {
            fact_id: id.clone(),
            statement: fact.statement.clone(),
        })
        .collect()
}

/// Assemble the bounded context of `state` for the situation in `query`.
///
/// `query` must be about this NPC; memories are fetched with
/// [`MemoryStore::query_relevant`], so nothing another NPC remembers can
/// appear.
pub async fn build_context(
    state: &CharacterState,
    store: &dyn MemoryStore,
    query: &MemoryQuery,
) -> Result<NpcContext, NpcError> {
    if &query.character_id != state.character_id() || query.session_id != state.session_id() {
        return Err(NpcError::InvalidQuery(format!(
            "memory query for {} does not match character {}",
            query.character_id,
            state.character_id()
        )));
    }
    let memories = store.query_relevant(query).await?;

    let player = EntityId::player();
    let mut relationships = BTreeMap::from([(player.clone(), state.relationship(&player))]);
    for entity in &query.entities {
        if relationships.len() >= MAX_CONTEXT_RELATIONSHIPS {
            break;
        }
        if let Some(relationship) = state.relationships().get(entity) {
            relationships.insert(entity.clone(), *relationship);
        }
    }

    Ok(NpcContext {
        character_id: state.character_id().clone(),
        name: state.name().to_owned(),
        role: state.role().map(str::to_owned),
        status: state.status(),
        location: state.location().cloned(),
        disposition_toward_player: state.disposition_toward_player(),
        relationships,
        goals: state.goals().to_vec(),
        known_facts: select_facts(state, query),
        known_flags: state
            .known_flags()
            .iter()
            .take(MAX_CONTEXT_FLAGS)
            .map(|(key, value)| (key.clone(), *value))
            .collect(),
        memories,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::npc::events::test_support::{SESSION, npc};
    use crate::npc::memory::test_support::{cid, eid, memory, t};
    use crate::npc::memory::{MAX_QUERY_LIMIT, MemoryEntry};
    use crate::npc::relationship::RelationshipDelta;
    use crate::npc::state::{Fact, KnowledgeSource};
    use crate::npc::store::InMemoryMemoryStore;
    use crate::npc::store::test_support::UnavailableStore;

    fn mem(character: &str, n: u64, summary: &str) -> MemoryEntry {
        let mut m = memory(character, n, summary, 20);
        m.session_id = SESSION;
        m
    }

    #[tokio::test]
    async fn context_is_bounded_and_relevant() {
        let mut hank = npc("hank", "Hank Schrader", Some("dea_office"));
        for i in 0..40 {
            let fact = Fact::new(
                FactId::new(format!("trivia:{i}")).unwrap(),
                format!("Case file {i} is closed."),
            )
            .unwrap();
            hank.learn_fact(fact, KnowledgeSource::Canon, None, t(i))
                .unwrap();
        }
        let lab = Fact::new(
            FactId::new("secret:lab").unwrap(),
            "The lab is hidden under the laundry.",
        )
        .unwrap();
        hank.learn_fact(lab, KnowledgeSource::Witnessed, None, t(0))
            .unwrap();
        for i in 0..30 {
            hank.learn_flag(FlagKey::new(format!("flag_{i:02}")).unwrap(), true, t(1))
                .unwrap();
        }
        hank.adjust_relationship(&eid("walter"), RelationshipDelta::new(30, 0, 40), t(2))
            .unwrap();
        hank.adjust_relationship(&eid("gus"), RelationshipDelta::new(-30, 0, -40), t(2))
            .unwrap();
        hank.add_goal("Catch Heisenberg", t(3)).unwrap();

        let store = InMemoryMemoryStore::new();
        for n in 1..=50 {
            store
                .store_memory(&mem("hank", n, &format!("Paperwork day {n}.")))
                .await
                .unwrap();
        }
        let mut m = mem("hank", 3, "Walter White asked odd questions about the lab.");
        m.memory_id = uuid::Uuid::from_u128(900);
        m.entities.insert(eid("walter"));
        store.store_memory(&m).await.unwrap();

        let query = MemoryQuery::new(SESSION, cid("hank"))
            .with_entity(eid("walter"))
            .with_text("where is the lab")
            .with_limit(100);
        let ctx = build_context(&hank, &store, &query).await.unwrap();

        assert_eq!(ctx.name, "Hank Schrader");
        assert_eq!(ctx.location.as_ref().unwrap().as_str(), "dea_office");
        assert_eq!(ctx.goals, ["Catch Heisenberg"]);
        assert_eq!(ctx.disposition_toward_player, Disposition::Neutral);
        // Player + queried entities only; gus was not asked about.
        assert_eq!(
            ctx.relationships
                .keys()
                .map(EntityId::as_str)
                .collect::<Vec<_>>(),
            ["player", "walter"]
        );

        assert_eq!(ctx.known_facts.len(), MAX_CONTEXT_FACTS);
        assert_eq!(ctx.known_facts[0].fact_id.as_str(), "secret:lab");
        // Remaining slots: most recently learned first.
        assert_eq!(ctx.known_facts[1].fact_id.as_str(), "trivia:39");
        assert_eq!(ctx.known_flags.len(), MAX_CONTEXT_FLAGS);

        assert_eq!(ctx.memories.len(), MAX_QUERY_LIMIT);
        assert_eq!(ctx.memories[0].memory.memory_id, m.memory_id);

        // Deterministic and serializable.
        assert_eq!(build_context(&hank, &store, &query).await.unwrap(), ctx);
        let json = serde_json::to_string(&ctx).unwrap();
        assert_eq!(serde_json::from_str::<NpcContext>(&json).unwrap(), ctx);
    }

    #[tokio::test]
    async fn context_never_contains_another_npcs_knowledge_or_memories() {
        let mut hank = npc("hank", "Hank Schrader", None);
        let walter = npc("walter", "Walter White", None);
        let secret = Fact::new(FactId::new("secret:x").unwrap(), "The secret.").unwrap();
        hank.learn_fact(secret, KnowledgeSource::Witnessed, None, t(1))
            .unwrap();
        let store = InMemoryMemoryStore::new();
        store
            .store_memory(&mem("hank", 1, "The player told me the secret."))
            .await
            .unwrap();

        let query = MemoryQuery::new(SESSION, cid("walter")).with_text("secret");
        let ctx = build_context(&walter, &store, &query).await.unwrap();
        assert!(ctx.known_facts.is_empty());
        assert!(ctx.memories.is_empty());

        // A query for one NPC cannot be used to build another NPC's context.
        assert!(matches!(
            build_context(&hank, &store, &query).await,
            Err(NpcError::InvalidQuery(_))
        ));
    }

    #[tokio::test]
    async fn context_propagates_storage_errors() {
        let hank = npc("hank", "Hank Schrader", None);
        let query = MemoryQuery::new(SESSION, cid("hank"));
        assert!(matches!(
            build_context(&hank, &UnavailableStore, &query).await,
            Err(NpcError::Store(_))
        ));
    }
}
