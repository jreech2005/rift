use std::path::{Path, PathBuf};
use std::sync::Arc;

use rift_backend::config::Config;
use rift_backend::director::{DirectorEngine, DirectorSettings};
use rift_backend::npc::MemoryStore;
use rift_backend::runtime::{Runtime, RuntimeWorld};
use rift_backend::session::SessionStore;
use rift_backend::telemetry::{InMemoryTelemetry, TelemetryReader, TelemetrySink};
use rift_backend::{AppState, app};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load the repo-root .env (then a local one) if present. Values are never logged.
    let root_env = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env");
    let _ = dotenvy::from_path(&root_env);
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env()?;
    let runtime = build_runtime().await?;
    let listener = tokio::net::TcpListener::bind(config.addr()).await?;
    info!(addr = %listener.local_addr()?, "rift-backend listening");

    axum::serve(listener, app(AppState::new(runtime)))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    info!("rift-backend stopped");
    Ok(())
}

/// WorldBible to serve (`cache/universes/<id>.json`). Unset: no world, plain
/// protocol V1 sessions.
const WORLD_BIBLE_ENV: &str = "RIFT_WORLD_BIBLE";
/// Optional authored scenario (narrative plan + secrets) for that WorldBible.
const SCENARIO_ENV: &str = "RIFT_SCENARIO";

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Wire the Phase 2 runtime from the environment. A world that is configured
/// but does not load is an error; a missing Gemini key, TiDB or Tiger Data
/// is not.
async fn build_runtime() -> Result<Runtime, Box<dyn std::error::Error>> {
    let (sink, reader) = telemetry().await;
    let mut runtime = Runtime::new(SessionStore::new())
        .with_director(director())
        .with_memory_store(memory_store().await)
        .with_telemetry(sink, reader);

    match env_path(WORLD_BIBLE_ENV) {
        Some(bible) => {
            let world = RuntimeWorld::from_files(&bible, env_path(SCENARIO_ENV).as_deref())?;
            info!(
                universe_id = world.universe_id(),
                secrets = world.secrets().len(),
                "world loaded"
            );
            runtime = runtime.with_world(world);
        }
        None => info!("{WORLD_BIBLE_ENV} is not set: sessions run without a world"),
    }
    Ok(runtime)
}

/// The configured LLM failover chain with the deterministic rules behind it,
/// or the deterministic rules alone when no LLM key is set. Reads the
/// environment only: no request is made at startup.
fn director() -> DirectorEngine {
    let settings = DirectorSettings::from_env();
    if settings.llms.is_empty() {
        info!("director: deterministic rules (no LLM configured)");
    } else {
        info!(
            chain = %settings.labels().join(" -> "),
            transient_retries = settings.transient_retries,
            budget_ms = settings.budget.as_millis() as u64,
            "director: LLM failover chain with deterministic fallback"
        );
    }
    settings.into_engine()
}

#[cfg(feature = "tidb")]
async fn memory_store() -> Arc<dyn MemoryStore> {
    use rift_backend::npc::tidb::{TiDbConfig, TiDbStore};

    let config = match TiDbConfig::from_env() {
        Ok(config) => config,
        Err(err) => {
            info!(reason = %err, "npc memory: in-memory store (TiDB is not configured)");
            return Arc::new(rift_backend::npc::InMemoryMemoryStore::new());
        }
    };
    let store = match TiDbStore::new(&config) {
        Ok(store) => store,
        Err(err) => {
            warn!(error = %err, "npc memory: TiDB store unavailable, using in-memory store");
            return Arc::new(rift_backend::npc::InMemoryMemoryStore::new());
        }
    };
    match store.ensure_schema().await {
        Ok(()) => {
            info!("npc memory: TiDB");
            Arc::new(store)
        }
        Err(err) => {
            warn!(error = %err, "npc memory: TiDB unreachable, using in-memory store");
            Arc::new(rift_backend::npc::InMemoryMemoryStore::new())
        }
    }
}

#[cfg(not(feature = "tidb"))]
async fn memory_store() -> Arc<dyn MemoryStore> {
    info!("npc memory: in-memory store");
    Arc::new(rift_backend::npc::InMemoryMemoryStore::new())
}

type Telemetry = (Arc<dyn TelemetrySink>, Arc<dyn TelemetryReader>);

fn in_memory_telemetry() -> Telemetry {
    let telemetry = Arc::new(InMemoryTelemetry::new());
    (telemetry.clone(), telemetry)
}

#[cfg(feature = "tiger")]
async fn telemetry() -> Telemetry {
    use rift_backend::telemetry::tiger::{TigerConfig, TigerTelemetry};

    let config = match TigerConfig::from_env() {
        Ok(config) => config,
        Err(err) => {
            info!(reason = %err, "telemetry: in-memory (Tiger Data is not configured)");
            return in_memory_telemetry();
        }
    };
    let tiger = TigerTelemetry::new(&config);
    match tiger.ensure_schema().await {
        Ok(aggregate) => {
            info!(continuous_aggregate = aggregate, "telemetry: Tiger Data");
            let tiger = Arc::new(tiger);
            (tiger.clone(), tiger)
        }
        Err(err) => {
            warn!(error = %err, "telemetry: Tiger Data unreachable, using in-memory");
            in_memory_telemetry()
        }
    }
}

#[cfg(not(feature = "tiger"))]
async fn telemetry() -> Telemetry {
    info!("telemetry: in-memory");
    in_memory_telemetry()
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            warn!(error = %err, "failed to listen for ctrl-c");
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(err) => warn!(error = %err, "failed to listen for SIGTERM"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    info!("shutdown signal received");
}
