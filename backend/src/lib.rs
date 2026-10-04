//! Rift realtime backend: authoritative in-memory game sessions served over a
//! persistent WebSocket (protocol V1, JSON).

pub mod action;
pub mod config;
pub mod npc;
pub mod director;
pub mod protocol;
pub mod session;
pub mod ws;

use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::session::SessionStore;

/// Shared application state. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct AppState {
    pub sessions: SessionStore,
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
