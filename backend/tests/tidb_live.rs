//! Live TiDB check for the NPC memory/state store.
//!
//! Not part of the default suite: it needs the `tidb` feature and real
//! `TIDB_*` credentials (repo-root `.env`). Run it with
//!
//! ```sh
//! cargo test --features tidb --test tidb_live -- --ignored --nocapture
//! ```
//!
//! Without credentials the test FAILS with `BLOCKED` — it never reports a
//! connection it did not make. It only touches disposable tables named
//! `rift_test_<random>_npc_*` and drops them afterwards.
#![cfg(feature = "tidb")]

use std::path::Path;

use chrono::Utc;
use rift_backend::npc::tidb::{TiDbConfig, TiDbStore};
use rift_backend::npc::{
    CharacterId, CharacterState, EntityId, MemoryEntry, MemoryQuery, MemoryStore, MemoryType,
};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires live TiDB credentials"]
async fn live_tidb_round_trip() {
    let _ = dotenvy::from_path(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env"));
    let config = match TiDbConfig::from_env() {
        Ok(config) => config,
        Err(err) => panic!("TiDB live test BLOCKED: {err}"),
    };

    let prefix = format!("rift_test_{}_", &Uuid::new_v4().simple().to_string()[..8]);
    let store = TiDbStore::with_table_prefix(&config, &prefix).unwrap();
    let version = store.ping().await.expect("connect to TiDB");
    println!("connected: {version}");
    store
        .ensure_schema()
        .await
        .expect("create disposable tables");

    let result = exercise(&store).await;
    // Always clean up, even if an assertion below failed.
    store.drop_tables().await.expect("drop disposable tables");
    store.disconnect().await.expect("disconnect");
    result.expect("TiDB round trip");
    println!("TiDB live test OK ({prefix}npc_* created and dropped)");
}

async fn exercise(store: &TiDbStore) -> Result<(), String> {
    let check = |ok: bool, what: &str| if ok { Ok(()) } else { Err(what.to_owned()) };
    let fail = |e: rift_backend::npc::StoreError| e.to_string();
    let session = Uuid::new_v4();
    let hank = CharacterId::new("hank").unwrap();
    let walter = CharacterId::new("walter").unwrap();
    let now = Utc::now();

    let mut memories = Vec::new();
    for (n, summary, importance) in [
        (1, "Paperwork all day.", 10),
        (2, "The player threatened Jesse at the lab.", 70),
        (3, "Lunch.", 5),
    ] {
        let mut m = MemoryEntry::new(
            Uuid::new_v4(),
            session,
            hank.clone(),
            MemoryType::Observation,
            summary,
            importance,
            n,
            now,
        )
        .unwrap();
        if n == 2 {
            m.entities.insert(EntityId::new("jesse").unwrap());
        }
        store.store_memory(&m).await.map_err(fail)?;
        store.store_memory(&m).await.map_err(fail)?; // idempotent
        memories.push(m);
    }

    let got = store
        .get_memory(memories[1].memory_id)
        .await
        .map_err(fail)?;
    check(got.as_ref() == Some(&memories[1]), "get_memory round trip")?;

    let recent = store.query_recent(session, &hank, 2).await.map_err(fail)?;
    let times: Vec<u64> = recent.iter().map(|m| m.world_time).collect();
    check(times == [3, 2], "query_recent order and bound")?;
    let none = store
        .query_recent(session, &walter, 5)
        .await
        .map_err(fail)?;
    check(none.is_empty(), "another NPC sees no memories")?;

    let query = MemoryQuery::new(session, hank.clone())
        .with_entity(EntityId::new("jesse").unwrap())
        .with_text("what happened at the lab");
    let relevant = store.query_relevant(&query).await.map_err(fail)?;
    check(
        relevant.first().map(|s| s.memory.memory_id) == Some(memories[1].memory_id),
        "query_relevant ranks the matching memory first",
    )?;

    let mut stolen = memories[0].clone();
    stolen.character_id = walter.clone();
    check(
        store.store_memory(&stolen).await.is_err(),
        "memory id cannot change owner",
    )?;

    check(
        store
            .invalidate_memory(memories[1].memory_id)
            .await
            .map_err(fail)?,
        "invalidate returns true",
    )?;
    let recent = store.query_recent(session, &hank, 10).await.map_err(fail)?;
    check(recent.len() == 2, "invalidated memory is not retrieved")?;

    let state = CharacterState::new(session, hank.clone(), "Hank Schrader", now).unwrap();
    store.save_character_state(&state).await.map_err(fail)?;
    let loaded = store
        .load_character_state(session, &hank)
        .await
        .map_err(fail)?;
    check(
        loaded.as_ref() == Some(&state),
        "character state round trip",
    )?;
    Ok(())
}
