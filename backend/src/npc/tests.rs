//! End-to-end scenarios across directory, adapter, store and context.

use serde_json::json;
use uuid::Uuid;

use super::events::test_support::{SESSION, event, reported, secret_x, witnesses};
use super::memory::test_support::{cid, eid, t};
use super::*;

/// Hank, Walter, Jesse and Gus, seeded from a WorldBible. Walter starts out
/// as Jesse's partner (an ally); nobody else has ties.
fn world() -> (NpcDirectory, InMemoryMemoryStore) {
    let bible = json!({"characters": [
        {"id": "hank", "name": "Hank Schrader", "role": "DEA agent",
         "known_facts": [{"text": "Blue meth is spreading in Albuquerque"}]},
        {"id": "walter", "name": "Walter White", "role": "Chemistry teacher",
         "known_facts": [{"text": "Walter White is Heisenberg"}],
         "relationships": [{"other_id": "jesse", "direction": "outgoing", "kind": "business partner"}]},
        {"id": "jesse", "name": "Jesse Pinkman",
         "known_facts": [{"text": "Walter White is Heisenberg"}]},
        {"id": "gus", "name": "Gus Fring", "known_facts": [{"text": "Chicken sells"}]}
    ]});
    let dir = NpcDirectory::new();
    dir.seed_from_world_bible(SESSION, &bible, t(0)).unwrap();
    (dir, InMemoryMemoryStore::new())
}

async fn memories_of(store: &InMemoryMemoryStore, id: &str) -> Vec<MemoryEntry> {
    store.query_recent(SESSION, &cid(id), 10).await.unwrap()
}

fn player_feelings(dir: &NpcDirectory, id: &str) -> Relationship {
    dir.get(SESSION, &cid(id))
        .unwrap()
        .relationship(&EntityId::player())
}

/// CRITICAL: a secret told privately to NPC A stays with NPC A until an
/// explicit reveal event carries it to NPC B.
#[tokio::test]
async fn private_secret_stays_private_until_revealed() {
    let (dir, store) = world();
    let secret = secret_x();
    let knows = |id: &str| dir.knows(SESSION, &cid(id), &secret.fact_id).unwrap();

    // Nobody knows X to begin with.
    assert!(!knows("hank") && !knows("walter"));

    // The player tells Hank, in private.
    let told_hank = event(
        1,
        NpcEventKind::FactRevealed {
            speaker: EntityId::player(),
            listener: cid("hank"),
            fact: secret.clone(),
        },
        Audience::Participants,
    );
    let formed = dir.record_event(&store, &told_hank).await.unwrap();

    // Hank knows X. Walter — and everybody else — does not.
    assert!(knows("hank"));
    assert!(!knows("walter") && !knows("jesse") && !knows("gus"));
    let hank = dir.get(SESSION, &cid("hank")).unwrap();
    let known = hank.known_fact(&secret.fact_id).unwrap();
    assert_eq!(
        known.source,
        KnowledgeSource::Told {
            by: EntityId::player()
        }
    );
    assert_eq!(known.source_event, Some(told_hank.event_id));

    // Hank, and only Hank, has a memory of the conversation.
    assert_eq!(formed.len(), 1);
    let hanks = memories_of(&store, "hank").await;
    assert_eq!(hanks, formed);
    assert_eq!(hanks[0].memory_type, MemoryType::Revelation);
    assert_eq!(hanks[0].event_id, Some(told_hank.event_id));
    assert_eq!(
        hanks[0].summary,
        "The player told me: The lab is hidden under the laundry."
    );
    assert!(memories_of(&store, "walter").await.is_empty());
    assert_eq!(store.len(), 1);

    // Walter's context for a conversation about the lab contains no trace of X.
    let about_lab = MemoryQuery::new(SESSION, cid("walter"))
        .with_entity(EntityId::player())
        .with_text("where is the lab hidden");
    let walter = dir.get(SESSION, &cid("walter")).unwrap();
    let ctx = build_context(&walter, &store, &about_lab).await.unwrap();
    assert!(ctx.memories.is_empty());
    assert!(ctx.known_facts.iter().all(|f| f.fact_id != secret.fact_id));
    assert!(!serde_json::to_string(&ctx).unwrap().contains("laundry"));

    // Later, Hank confronts Walter with it: an explicit reveal event to Walter.
    let told_walter = event(
        2,
        NpcEventKind::FactRevealed {
            speaker: eid("hank"),
            listener: cid("walter"),
            fact: secret.clone(),
        },
        Audience::Participants,
    );
    dir.record_event(&store, &told_walter).await.unwrap();

    assert!(knows("walter"));
    assert!(!knows("jesse") && !knows("gus"));
    let walter = dir.get(SESSION, &cid("walter")).unwrap();
    assert_eq!(
        walter.known_fact(&secret.fact_id).unwrap().source,
        KnowledgeSource::Told { by: eid("hank") }
    );
    let walters = memories_of(&store, "walter").await;
    assert_eq!(walters.len(), 1);
    assert_eq!(
        walters[0].summary,
        "Hank Schrader told me: The lab is hidden under the laundry."
    );
    let ctx = build_context(&walter, &store, &about_lab).await.unwrap();
    assert_eq!(ctx.known_facts[0].fact_id, secret.fact_id);
    assert_eq!(ctx.memories.len(), 1);
}

/// The whole secret scenario is reproducible bit for bit.
#[tokio::test]
async fn secret_scenario_is_deterministic() {
    async fn run() -> (Vec<CharacterState>, Vec<MemoryEntry>) {
        let (dir, store) = world();
        for (n, speaker, listener) in [(1, "player", "hank"), (2, "hank", "walter")] {
            let e = event(
                n,
                NpcEventKind::FactRevealed {
                    speaker: eid(speaker),
                    listener: cid(listener),
                    fact: secret_x(),
                },
                Audience::Participants,
            );
            dir.record_event(&store, &e).await.unwrap();
        }
        let mut memories = memories_of(&store, "hank").await;
        memories.extend(memories_of(&store, "walter").await);
        (dir.list(SESSION), memories)
    }
    assert_eq!(run().await, run().await);
}

/// An NPC cannot leak what it was never told.
#[tokio::test]
async fn npc_cannot_reveal_a_secret_it_does_not_know() {
    let (dir, store) = world();
    let leak = event(
        1,
        NpcEventKind::FactRevealed {
            speaker: eid("gus"),
            listener: cid("walter"),
            fact: secret_x(),
        },
        Audience::Participants,
    );
    assert_eq!(
        dir.record_event(&store, &leak).await,
        Err(NpcError::UnknownFact {
            character: "gus".into(),
            fact: "secret:x".into(),
        })
    );
    assert_eq!(
        dir.knows(SESSION, &cid("walter"), &secret_x().fact_id),
        Ok(false)
    );
    assert!(store.is_empty());
}

/// The player harms NPC A's ally: A, who saw it, remembers and turns on the
/// player; an unrelated NPC gets nothing until someone tells it.
#[tokio::test]
async fn witnessed_harm_changes_the_witness_and_nobody_else() {
    let (dir, store) = world();
    let walter_before = dir.get(SESSION, &cid("walter")).unwrap();
    let gus_before = dir.get(SESSION, &cid("gus")).unwrap();
    assert!(walter_before.relationship(&eid("jesse")).is_ally());
    assert_eq!(
        walter_before.disposition_toward_player(),
        Disposition::Neutral
    );

    // The player harms Jesse (Walter's ally) in front of Walter.
    let harm = NpcEventKind::Harmed {
        actor: EntityId::player(),
        target: cid("jesse"),
    };
    let seen = event(1, harm.clone(), witnesses(&["walter"]));
    dir.record_event(&store, &seen).await.unwrap();

    // Walter recorded what he saw...
    let walters = memories_of(&store, "walter").await;
    assert_eq!(walters.len(), 1);
    assert_eq!(walters[0].summary, "I saw the player harm Jesse Pinkman.");
    assert_eq!(walters[0].memory_type, MemoryType::Observation);
    assert_eq!(walters[0].event_id, Some(seen.event_id));
    assert!(walters[0].entities.contains(&eid("jesse")));
    assert!(walters[0].emotional_valence < 0);
    // ...and his disposition toward the player changed.
    assert_eq!(
        player_feelings(&dir, "walter"),
        Relationship::new(-25, 10, -30)
    );
    assert_eq!(
        dir.get(SESSION, &cid("walter"))
            .unwrap()
            .disposition_toward_player(),
        Disposition::Hostile
    );
    // The victim remembers it too, more strongly.
    assert_eq!(
        memories_of(&store, "jesse").await[0].summary,
        "The player harmed me."
    );
    assert_eq!(
        player_feelings(&dir, "jesse"),
        Relationship::new(-35, 30, -35)
    );

    // Non-witnesses: no memory, no state change at all.
    for id in ["gus", "hank"] {
        assert!(memories_of(&store, id).await.is_empty(), "{id}");
        assert_eq!(player_feelings(&dir, id), Relationship::default(), "{id}");
    }
    assert_eq!(dir.get(SESSION, &cid("gus")).unwrap(), gus_before);
    assert_eq!(store.len(), 2);

    // Information transfer: Walter tells Gus. Only now does Gus get a memory.
    let news = event(2, harm, reported("walter", &["gus"]));
    dir.record_event(&store, &news).await.unwrap();
    let gus = memories_of(&store, "gus").await;
    assert_eq!(gus.len(), 1);
    assert_eq!(
        gus[0].summary,
        "Walter White told me that the player harmed Jesse Pinkman."
    );
    assert_eq!(gus[0].memory_type, MemoryType::Report);
    assert_eq!(gus[0].metadata["informant"], "walter");
    assert_eq!(player_feelings(&dir, "gus"), Relationship::new(-5, 7, 0));
    // Hank still has not heard a thing, and the report did not hit Walter twice.
    assert!(memories_of(&store, "hank").await.is_empty());
    assert_eq!(
        player_feelings(&dir, "walter"),
        Relationship::new(-25, 10, -30)
    );
    assert_eq!(memories_of(&store, "walter").await.len(), 1);
}

/// Canon knowledge differs per NPC from the first tick, and world flags are
/// learned per NPC — never read from global session state.
#[tokio::test]
async fn npcs_start_with_and_keep_different_knowledge() {
    let (dir, store) = world();
    let heisenberg = state::canon_fact_id("Walter White is Heisenberg");
    assert_eq!(dir.knows(SESSION, &cid("walter"), &heisenberg), Ok(true));
    assert_eq!(dir.knows(SESSION, &cid("jesse"), &heisenberg), Ok(true));
    assert_eq!(dir.knows(SESSION, &cid("hank"), &heisenberg), Ok(false));

    let lab = LocationId::new("lab").unwrap();
    for id in ["walter", "jesse"] {
        dir.update(SESSION, &cid(id), |s| {
            s.set_location(Some(lab.clone()), t(1))
        })
        .unwrap();
    }
    let flag = FlagKey::new("lab_destroyed").unwrap();
    let mut fire = event(
        1,
        NpcEventKind::WorldEvent {
            summary: "The lab burned to the ground.".into(),
            entities: [eid("lab")].into(),
            flag: Some(FlagChange {
                key: flag.clone(),
                value: true,
            }),
        },
        Audience::Location,
    );
    fire.location = Some(lab);
    dir.record_event(&store, &fire).await.unwrap();

    let believes = |id: &str| dir.get(SESSION, &cid(id)).unwrap().known_flag(&flag);
    assert_eq!(believes("walter"), Some(true));
    assert_eq!(believes("jesse"), Some(true));
    assert_eq!(believes("hank"), None);
    assert_eq!(believes("gus"), None);
    assert!(memories_of(&store, "hank").await.is_empty());
}

/// Long histories never leave the store: retrieval and context stay bounded.
#[tokio::test]
async fn long_history_yields_bounded_context() {
    let (dir, store) = world();
    for n in 1..=120 {
        let e = event(
            n,
            NpcEventKind::WorldEvent {
                summary: format!("Customer number {n} ordered chicken."),
                entities: Default::default(),
                flag: None,
            },
            witnesses(&["gus"]),
        );
        dir.record_event(&store, &e).await.unwrap();
    }
    let threat = event(
        121,
        NpcEventKind::Threatened {
            actor: EntityId::player(),
            target: cid("gus"),
        },
        Audience::Participants,
    );
    dir.record_event(&store, &threat).await.unwrap();
    assert_eq!(store.len(), 121);

    let gus = dir.get(SESSION, &cid("gus")).unwrap();
    let query = MemoryQuery::new(SESSION, cid("gus"))
        .with_entity(EntityId::player())
        .with_recent_event(threat.event_id)
        .with_limit(1_000);
    let ctx = build_context(&gus, &store, &query).await.unwrap();
    assert_eq!(ctx.memories.len(), memory::MAX_QUERY_LIMIT);
    assert_eq!(ctx.memories[0].memory.summary, "The player threatened me.");
    assert_eq!(ctx.disposition_toward_player, Disposition::Hostile);
    assert!(serde_json::to_string(&ctx).unwrap().len() < 16_384);

    assert_eq!(
        store
            .query_recent(SESSION, &cid("gus"), 5)
            .await
            .unwrap()
            .len(),
        5
    );
}

/// Invalid identifiers are rejected at every entry point.
#[test]
fn invalid_identifiers_are_rejected_everywhere() {
    assert!(CharacterId::new("walter white").is_err());
    assert!(EntityId::new("").is_err());
    assert!(FactId::new("secret'; --").is_err());
    assert!(LocationId::new("x".repeat(65)).is_err());

    let bad_event = json!({
        "event_id": Uuid::from_u128(1),
        "session_id": SESSION,
        "world_time": 1,
        "timestamp": "2026-10-03T12:00:00Z",
        "kind": {"type": "harmed", "actor": "player", "target": "jesse pinkman"},
        "audience": {"scope": "participants"}
    });
    assert!(serde_json::from_value::<NpcEvent>(bad_event).is_err());

    let dir = NpcDirectory::new();
    let bible = json!({"characters": [{"id": "Not An Id", "name": "X"}]});
    assert!(matches!(
        dir.seed_from_world_bible(SESSION, &bible, t(0)),
        Err(NpcError::InvalidIdentifier { .. })
    ));
    assert!(dir.list(SESSION).is_empty());
}
