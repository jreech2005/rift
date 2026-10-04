//! In-memory telemetry: the default, and what every offline test uses.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use super::{
    RecentCounts, TelemetryEvent, TelemetryEventKind, TelemetryFuture, TelemetryReader,
    TelemetrySink, WINDOW_SECONDS, summarize,
};
use crate::director::PlayerTelemetry;

/// Events kept per session. Older events are dropped first.
pub const DEFAULT_EVENTS_PER_SESSION: usize = 512;

/// Sessions kept. When full, events of new sessions are dropped.
pub const MAX_SESSIONS: usize = 1024;

/// Sink and reader over a bounded per-session ring of events.
#[derive(Debug)]
pub struct InMemoryTelemetry {
    sessions: Mutex<HashMap<Uuid, VecDeque<TelemetryEvent>>>,
    capacity: usize,
}

impl Default for InMemoryTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryTelemetry {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_EVENTS_PER_SESSION)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            sessions: Mutex::default(),
            capacity: capacity.max(1),
        }
    }

    /// Every event kept for a session, oldest first.
    pub fn events(&self, session_id: Uuid) -> Vec<TelemetryEvent> {
        self.sessions
            .lock()
            .expect("telemetry lock poisoned")
            .get(&session_id)
            .map(|events| events.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// The session's window ending at `now`, summarised.
    pub fn summary(&self, session_id: Uuid, now: DateTime<Utc>) -> PlayerTelemetry {
        let since = now - Duration::seconds(i64::from(WINDOW_SECONDS));
        let sessions = self.sessions.lock().expect("telemetry lock poisoned");
        let mut counts = RecentCounts::default();
        let mut locations = BTreeSet::new();
        let recent = sessions
            .get(&session_id)
            .into_iter()
            .flatten()
            .filter(|e| e.timestamp > since && e.timestamp <= now);
        for event in recent {
            match event.kind {
                TelemetryEventKind::NpcInteraction => counts.npc_interactions += 1,
                TelemetryEventKind::LocationEntered => {
                    if let Some(location) = event.location.as_deref() {
                        locations.insert(location);
                    }
                }
                TelemetryEventKind::PlayerDamaged => counts.damage_events += 1,
                TelemetryEventKind::PlayerDied => counts.deaths += 1,
                TelemetryEventKind::EnemyKilled => counts.kills += 1,
                TelemetryEventKind::PlayerAction
                | TelemetryEventKind::DirectorInvoked
                | TelemetryEventKind::NarrativeReplan => {}
            }
        }
        counts.distinct_locations = locations.len() as u64;
        summarize(counts)
    }
}

impl TelemetrySink for InMemoryTelemetry {
    fn record(&self, event: TelemetryEvent) {
        let mut sessions = self.sessions.lock().expect("telemetry lock poisoned");
        if !sessions.contains_key(&event.session_id) && sessions.len() >= MAX_SESSIONS {
            return;
        }
        let events = sessions.entry(event.session_id).or_default();
        if events.len() == self.capacity {
            events.pop_front();
        }
        events.push_back(event);
    }
}

impl TelemetryReader for InMemoryTelemetry {
    fn recent<'a>(
        &'a self,
        session_id: Uuid,
        now: DateTime<Utc>,
    ) -> TelemetryFuture<'a, PlayerTelemetry> {
        Box::pin(std::future::ready(Ok(self.summary(session_id, now))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(session_id: Uuid, kind: TelemetryEventKind, at: DateTime<Utc>) -> TelemetryEvent {
        TelemetryEvent::new(session_id, kind, at)
    }

    #[tokio::test]
    async fn summarises_only_the_window_of_one_session() {
        let telemetry = InMemoryTelemetry::new();
        let (session, other) = (Uuid::new_v4(), Uuid::new_v4());
        let now = Utc::now();
        let old = now - Duration::seconds(i64::from(WINDOW_SECONDS) + 1);

        telemetry.record(event(session, TelemetryEventKind::NpcInteraction, now));
        telemetry.record(event(session, TelemetryEventKind::NpcInteraction, old));
        telemetry.record(event(other, TelemetryEventKind::PlayerDied, now));
        for place in ["yard", "yard", "lab"] {
            telemetry.record(
                event(session, TelemetryEventKind::LocationEntered, now).location(Some(place)),
            );
        }

        let summary = telemetry.recent(session, now).await.unwrap();
        assert_eq!(summary.npc_engagement, 20);
        assert_eq!(summary.exploration_activity, 50);
        assert_eq!(summary.recent_deaths, 0);
        assert_eq!(summary.window_seconds, WINDOW_SECONDS);

        let unknown = telemetry.recent(Uuid::new_v4(), now).await.unwrap();
        assert_eq!(unknown.npc_engagement, 0);
    }

    #[test]
    fn keeps_a_bounded_number_of_events() {
        let telemetry = InMemoryTelemetry::with_capacity(3);
        let session = Uuid::new_v4();
        let now = Utc::now();
        for i in 0..10 {
            telemetry.record(event(session, TelemetryEventKind::PlayerAction, now).value(i.into()));
        }
        let kept = telemetry.events(session);
        assert_eq!(kept.len(), 3);
        assert_eq!(kept[0].numeric_value, Some(7.0));
    }

    #[test]
    fn keeps_a_bounded_number_of_sessions() {
        let telemetry = InMemoryTelemetry::new();
        let now = Utc::now();
        for _ in 0..MAX_SESSIONS {
            telemetry.record(event(Uuid::new_v4(), TelemetryEventKind::PlayerAction, now));
        }
        let late = Uuid::new_v4();
        telemetry.record(event(late, TelemetryEventKind::PlayerAction, now));
        assert!(telemetry.events(late).is_empty());
    }
}
