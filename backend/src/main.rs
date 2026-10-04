use std::path::{Path, PathBuf};
use std::sync::Arc;

use rift_backend::config::Config;
use rift_backend::director::{DirectorEngine, FallbackDirector, GeminiDirector, ProviderErrorKind};
use rift_backend::npc::MemoryStore;
use rift_backend::runtime::{Runtime, RuntimeWorld};
use rift_backend::session::SessionStore;
use rift_backend::voice::VoiceService;
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
/// but does not load is an error; a missing Gemini key or TiDB is not.
async fn build_runtime() -> Result<Runtime, Box<dyn std::error::Error>> {
    let mut runtime = Runtime::new(SessionStore::new())
        .with_director(director())
        .with_memory_store(memory_store().await);

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
    if let Some(voice) = voice() {
        runtime = runtime.with_voice(voice);
    }
    Ok(runtime)
}

/// ElevenLabs NPC voice when a key and at least one voice are configured.
/// Otherwise dialogue is text only; that is never an error.
fn voice() -> Option<VoiceService> {
    match VoiceService::elevenlabs_from_env() {
        Ok(voice) => {
            let npcs: Vec<&str> = voice.voiced_npcs().collect();
            info!(?npcs, "voice: elevenlabs");
            Some(voice)
        }
        Err(err) => {
            info!(reason = %err.detail, "voice: disabled, dialogue is text only");
            None
        }
    }
}

/// Gemini with the deterministic rules as fallback when a key is configured,
/// the deterministic rules alone otherwise.
fn director() -> DirectorEngine {
    match GeminiDirector::from_env() {
        Ok(gemini) => {
            info!(
                model = gemini.model(),
                "director: gemini with deterministic fallback"
            );
            DirectorEngine::new(Arc::new(gemini)).with_fallback(Arc::new(FallbackDirector))
        }
        Err(err) => {
            if err.kind == ProviderErrorKind::NotConfigured {
                info!("director: deterministic rules (no GEMINI_API_KEY)");
            } else {
                warn!(error = %err, "director: gemini unavailable, using deterministic rules");
            }
            DirectorEngine::deterministic()
        }
    }
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
