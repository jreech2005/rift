//! Live Tiger Data check for gameplay telemetry.
//!
//! Not part of the default suite: it needs the `tiger` feature and a real
//! `TIGER_DATABASE_URL` (environment or repo-root `.env`). Run it with
//!
//! ```sh
//! cargo test --features tiger --test tiger_live -- --ignored --nocapture
//! ```
//!
//! Without the URL the test FAILS with `BLOCKED` — it never reports a
//! connection it did not make. It creates the telemetry schema if it is
//! missing, writes events for one random session id and deletes them again.
#![cfg(feature = "tiger")]

use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use rift_backend::telemetry::tiger::{TigerConfig, TigerTelemetry};
use rift_backend::telemetry::{
    TelemetryEvent, TelemetryEventKind, TelemetryReader, TelemetrySink, WINDOW_SECONDS,
};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a live Tiger Data service"]
async fn live_tiger_round_trip() {
    let _ = dotenvy::from_path(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env"));
    let config = match TigerConfig::from_env() {
        Ok(config) => config,
        Err(err) => panic!("Tiger live test BLOCKED: {err}"),
    };

    let tiger = TigerTelemetry::new(&config);
    let version = tiger.ping().await.expect("connect to Tiger Data");
    println!("connected: PostgreSQL {version}");
    let aggregate = tiger.ensure_schema().await.expect("create schema");
    println!("schema ready (continuous aggregate: {aggregate})");

    let session = Uuid::new_v4();
    let result = exercise(&tiger, session).await;
    // Always clean up, even if an assertion below failed.
    let deleted = tiger.delete_session(session).await.expect("clean up");
    result.expect("Tiger Data round trip");
    println!("Tiger live test OK ({deleted} rows written and deleted)");
}

async fn exercise(tiger: &TigerTelemetry, session: Uuid) -> Result<(), String> {
    let check = |ok: bool, what: &str| if ok { Ok(()) } else { Err(what.to_owned()) };
    let now = Utc::now();
    let event = |kind| TelemetryEvent::new(session, kind, now).actor("player");

    // Direct writes: errors surface here.
    for _ in 0..3 {
        tiger
            .write(
                &event(TelemetryEventKind::NpcInteraction)
                    .target(Some("hank_schrader"))
                    .value(1.0)
                    .metadata(serde_json::json!({ "disclosure": false })),
            )
            .await
            .map_err(|e| e.to_string())?;
    }
    for place in ["yard", "lab", "yard"] {
        tiger
            .write(&event(TelemetryEventKind::LocationEntered).location(Some(place)))
            .await
            .map_err(|e| e.to_string())?;
    }
    tiger
        .write(&event(TelemetryEventKind::PlayerDied))
        .await
        .map_err(|e| e.to_string())?;
    // Outside the window: must not count.
    let old = now - chrono::Duration::seconds(i64::from(WINDOW_SECONDS) + 60);
    tiger
        .write(&TelemetryEvent::new(
            session,
            TelemetryEventKind::PlayerDied,
            old,
        ))
        .await
        .map_err(|e| e.to_string())?;

    let summary = tiger
        .recent(session, now)
        .await
        .map_err(|e| e.to_string())?;
    println!("summary: {summary:?}");
    check(summary.npc_engagement == 60, "npc_engagement")?;
    check(summary.exploration_activity == 50, "exploration_activity")?;
    check(summary.recent_deaths == 1, "recent_deaths")?;
    check(summary.combat_intensity == 0, "combat_intensity")?;
    check(summary.prefers_dialogue(), "prefers_dialogue")?;

    // The gameplay path: queued, written in the background.
    for _ in 0..6 {
        tiger.record(event(TelemetryEventKind::PlayerDamaged));
    }
    for _ in 0..50 {
        let summary = tiger
            .recent(session, now)
            .await
            .map_err(|e| e.to_string())?;
        if summary.combat_intensity == 60 {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err("queued events were not written within 5 s".to_owned())
}
