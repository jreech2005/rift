//! Gameplay telemetry (Phase 3): what the player has been doing *recently*.
//!
//! ```text
//! runtime events ─► TelemetrySink ─► Tiger Data (or memory)
//!                                        │ recent window
//!                   TelemetryReader ◄────┘
//!                        │ PlayerTelemetry (a few bounded scores)
//!                        ▼
//!                  DirectorContext
//! ```
//!
//! TiDB (`npc::tidb`) is the world's long-term memory. This module is the
//! other half: a high-frequency event stream and a small summary of the last
//! few minutes of it. Raw events never reach the Director.
//!
//! Telemetry is never on the gameplay path. [`TelemetrySink::record`] cannot
//! fail or block, and the summary is read off the read path, under a timeout,
//! with "no telemetry" as the answer to every failure.
//!
//! See `docs/TELEMETRY.md`.

pub mod memory;
pub mod tiger;

use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::director::PlayerTelemetry;

pub use memory::InMemoryTelemetry;

/// The window a [`PlayerTelemetry`] summarises.
pub const WINDOW_SECONDS: u32 = 300;

/// Highest value of a [`PlayerTelemetry`] score.
pub const MAX_SCORE: u8 = 100;

// How much one event moves a score. Small integers on purpose: the scores
// are for explainable rules, not statistics.
const ENGAGEMENT_PER_INTERACTION: u64 = 20;
const EXPLORATION_PER_LOCATION: u64 = 25;
const COMBAT_PER_DAMAGE: u64 = 10;
const COMBAT_PER_KILL: u64 = 15;

/// What happened. The first five are emitted by the runtime today. The combat
/// kinds are accepted, stored and aggregated, but nothing emits them yet: the
/// game has no combat to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryEventKind {
    PlayerAction,
    NpcInteraction,
    LocationEntered,
    DirectorInvoked,
    NarrativeReplan,
    PlayerDamaged,
    PlayerDied,
    EnemyKilled,
}

impl TelemetryEventKind {
    /// The `event_type` column value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PlayerAction => "player_action",
            Self::NpcInteraction => "npc_interaction",
            Self::LocationEntered => "location_entered",
            Self::DirectorInvoked => "director_invoked",
            Self::NarrativeReplan => "narrative_replan",
            Self::PlayerDamaged => "player_damaged",
            Self::PlayerDied => "player_died",
            Self::EnemyKilled => "enemy_killed",
        }
    }
}

/// One row of the telemetry stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetryEvent {
    pub timestamp: DateTime<Utc>,
    pub session_id: Uuid,
    pub kind: TelemetryEventKind,
    pub actor_id: Option<String>,
    pub target_id: Option<String>,
    pub location: Option<String>,
    pub numeric_value: Option<f64>,
    /// Small structured detail. Never secrets, never free player text.
    pub metadata: Value,
}

impl TelemetryEvent {
    pub fn new(session_id: Uuid, kind: TelemetryEventKind, timestamp: DateTime<Utc>) -> Self {
        Self {
            timestamp,
            session_id,
            kind,
            actor_id: None,
            target_id: None,
            location: None,
            numeric_value: None,
            metadata: Value::Object(serde_json::Map::new()),
        }
    }

    pub fn actor(mut self, actor_id: impl Into<String>) -> Self {
        self.actor_id = Some(actor_id.into());
        self
    }

    pub fn target(mut self, target_id: Option<impl Into<String>>) -> Self {
        self.target_id = target_id.map(Into::into);
        self
    }

    pub fn location(mut self, location: Option<impl Into<String>>) -> Self {
        self.location = location.map(Into::into);
        self
    }

    pub fn value(mut self, numeric_value: f64) -> Self {
        self.numeric_value = Some(numeric_value);
        self
    }

    pub fn metadata(mut self, metadata: Value) -> Self {
        self.metadata = metadata;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TelemetryError {
    /// The backend could not be reached or timed out.
    #[error("telemetry unavailable: {0}")]
    Unavailable(String),
    /// The backend answered with an error.
    #[error("telemetry backend error: {0}")]
    Backend(String),
}

pub type TelemetryFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, TelemetryError>> + Send + 'a>>;

/// Where gameplay events go.
pub trait TelemetrySink: Send + Sync {
    /// Record one event. Called on the gameplay path, so it must return
    /// immediately and cannot fail: an implementation that cannot keep up or
    /// reach its backend drops the event.
    fn record(&self, event: TelemetryEvent);
}

/// Where the recent summary comes from.
pub trait TelemetryReader: Send + Sync {
    /// The session's last [`WINDOW_SECONDS`] up to `now`, summarised.
    fn recent<'a>(
        &'a self,
        session_id: Uuid,
        now: DateTime<Utc>,
    ) -> TelemetryFuture<'a, PlayerTelemetry>;
}

/// Raw counts over one window. Both backends reduce their events to this, so
/// they agree on every score.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecentCounts {
    pub npc_interactions: u64,
    pub distinct_locations: u64,
    pub damage_events: u64,
    pub deaths: u64,
    pub kills: u64,
}

fn score(points: u64) -> u8 {
    u8::try_from(points.min(u64::from(MAX_SCORE))).unwrap_or(MAX_SCORE)
}

/// Normalise window counts into the scores the Director sees.
pub fn summarize(counts: RecentCounts) -> PlayerTelemetry {
    PlayerTelemetry {
        window_seconds: WINDOW_SECONDS,
        combat_intensity: score(
            counts
                .damage_events
                .saturating_mul(COMBAT_PER_DAMAGE)
                .saturating_add(counts.kills.saturating_mul(COMBAT_PER_KILL)),
        ),
        recent_deaths: score(counts.deaths),
        npc_engagement: score(
            counts
                .npc_interactions
                .saturating_mul(ENGAGEMENT_PER_INTERACTION),
        ),
        exploration_activity: score(
            counts
                .distinct_locations
                .saturating_mul(EXPLORATION_PER_LOCATION),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_recent_is_all_zero() {
        let quiet = summarize(RecentCounts::default());
        assert_eq!(
            quiet,
            PlayerTelemetry {
                window_seconds: WINDOW_SECONDS,
                ..PlayerTelemetry::default()
            }
        );
        assert!(!quiet.prefers_dialogue());
    }

    #[test]
    fn counts_become_bounded_scores() {
        let summary = summarize(RecentCounts {
            npc_interactions: 2,
            distinct_locations: 3,
            damage_events: 3,
            deaths: 1,
            kills: 2,
        });
        assert_eq!(summary.npc_engagement, 40);
        assert_eq!(summary.exploration_activity, 75);
        assert_eq!(summary.combat_intensity, 60);
        assert_eq!(summary.recent_deaths, 1);
    }

    #[test]
    fn scores_are_capped() {
        let summary = summarize(RecentCounts {
            npc_interactions: u64::MAX,
            distinct_locations: 1_000,
            damage_events: u64::MAX,
            deaths: 5_000,
            kills: u64::MAX,
        });
        for value in [
            summary.npc_engagement,
            summary.exploration_activity,
            summary.combat_intensity,
            summary.recent_deaths,
        ] {
            assert_eq!(value, MAX_SCORE);
        }
    }

    #[test]
    fn event_kinds_serialize_as_their_column_value() {
        for kind in [
            TelemetryEventKind::PlayerAction,
            TelemetryEventKind::NpcInteraction,
            TelemetryEventKind::LocationEntered,
            TelemetryEventKind::DirectorInvoked,
            TelemetryEventKind::NarrativeReplan,
            TelemetryEventKind::PlayerDamaged,
            TelemetryEventKind::PlayerDied,
            TelemetryEventKind::EnemyKilled,
        ] {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::json!(kind.as_str())
            );
        }
    }
}
