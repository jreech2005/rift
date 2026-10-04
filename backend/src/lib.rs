//! Rift realtime backend: authoritative in-memory game sessions served over a
//! persistent WebSocket (protocol V1, JSON).

pub mod action;
pub mod config;
pub mod director;
pub mod narrative;
pub mod npc;
pub mod protocol;
pub mod runtime;
pub mod session;
pub mod ws;

use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::runtime::Runtime;
use crate::session::SessionStore;

/// Shared application state. Cheap to clone.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The authoritative sessions. The same store the runtime drives.
    pub sessions: SessionStore,
    pub runtime: Runtime,
}

impl AppState {
    pub fn new(runtime: Runtime) -> Self {
        Self {
            sessions: runtime.sessions().clone(),
            runtime,
        }
    }
}

impl Default for AppState {
    /// No world, deterministic Director, in-memory stores: plain protocol V1.
    fn default() -> Self {
        Self::new(Runtime::default())
    }
}

/// Build the HTTP/WebSocket router.
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ws", get(ws::ws_handler))
        .with_state(state)
}

async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "rift-backend",
        "protocol_version": protocol::PROTOCOL_VERSION,
    }))
}
