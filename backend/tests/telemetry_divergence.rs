//! Phase 3 acceptance: recent player telemetry changes what the Director
//! does.
//!
//! Same world, same story state, same player action. The only thing that
//! differs between the scenarios is what the telemetry layer says the player
//! has been doing for the last few minutes. Offline and deterministic: the
//! Director is the rule set and telemetry is the in-memory implementation.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rift_backend::action::{ActionType, ValidatedAction, WorldEventType};
use rift_backend::director::{DirectorAction, DirectorEngine, PlayerTelemetry};
use rift_backend::narrative::NarrativeState;
use rift_backend::runtime::{Runtime, RuntimeOutcome, RuntimeWorld};
use rift_backend::session::SessionStore;
use rift_backend::telemetry::{
    InMemoryTelemetry, TelemetryError, TelemetryEvent, TelemetryEventKind, TelemetryFuture,
    TelemetryReader, TelemetrySink,
};
use uuid::Uuid;

const WORLD_BIBLE: &str = include_str!("fixtures/director/world_bible_breaking_bad.json");
const SCENARIO: &str = include_str!("fixtures/runtime/scenario_burner_phone.json");

const HANK: &str = "hank_schrader";
const TELL_HANK: &str = "Walter has a burner phone hidden under his mattress.";
const SMALL_TALK: &str = "How are things at the office?";

fn world() -> RuntimeWorld {
    RuntimeWorld::from_world_bible(WORLD_BIBLE)
        .and_then(|world| world.with_scenario_json(SCENARIO))
        .expect("fixtures load")
}

struct Rig {
    runtime: Runtime,
    telemetry: Arc<InMemoryTelemetry>,
    session_id: Uuid,
}

fn rig() -> Rig {
    let telemetry = Arc::new(InMemoryTelemetry::new());
    rig_reading(telemetry.clone(), telemetry)
}

/// A rig that writes to in-memory telemetry and reads from `reader`.
fn rig_reading(telemetry: Arc<InMemoryTelemetry>, reader: Arc<dyn TelemetryReader>) -> Rig {
    let runtime = Runtime::new(SessionStore::new())
        .with_world(world())
        .with_director(DirectorEngine::deterministic())
        .with_telemetry(telemetry.clone(), reader);
    let session_id = runtime.create_session().session_id;
    Rig {
        runtime,
        telemetry,
        session_id,
    }
}

impl Rig {
    async fn act(
        &self,
        action_type: ActionType,
        target: Option<&str>,
        content: Option<&str>,
    ) -> RuntimeOutcome {
        let action = ValidatedAction {
            action_id: Uuid::new_v4(),
            session_id: self.session_id,
            actor_id: "player".into(),
            action_type,
            target: target.map(str::to_owned),
            content: content.map(str::to_owned),
        };
        self.runtime
            .process_player_action(&action)
            .await
            .expect("action accepted")
    }

    async fn tell_hank(&self) -> RuntimeOutcome {
        self.act(ActionType::Speak, Some(HANK), Some(TELL_HANK))
            .await
    }

    fn seed(&self, kind: TelemetryEventKind, count: usize) {
        for _ in 0..count {
            self.telemetry
                .record(TelemetryEvent::new(self.session_id, kind, Utc::now()).actor("player"));
        }
    }

    fn narrative(&self) -> NarrativeState {
        self.runtime.narrative(self.session_id).unwrap()
    }

    fn kinds(&self) -> Vec<TelemetryEventKind> {
        self.telemetry
            .events(self.session_id)
            .iter()
            .map(|event| event.kind)
            .collect()
    }
}

/// The applied decision's action types, in order.
#[track_caller]
fn decided(outcome: &RuntimeOutcome) -> Vec<&'static str> {
    assert!(outcome.director().is_applied(), "{:?}", outcome.director());
    outcome
        .director()
        .decision()
        .unwrap()
        .actions
        .iter()
        .map(DirectorAction::type_name)
        .collect()
}

fn event_types(outcome: &RuntimeOutcome) -> Vec<WorldEventType> {
    outcome.events().map(|event| event.event_type).collect()
}

// By the time the Director is asked, the narrative layer has already failed
// the objective and invalidated the mission. What is left to decide is how
// the world reacts, and that is where telemetry comes in.
const ESCALATION: [&str; 4] = [
    "set_world_flag",
    "trigger_world_event",
    "set_objective",
    "request_replan",
];

const BREATHING_ROOM: [&str; 4] = [
    "set_world_flag",
    "start_dialogue",
    "set_objective",
    "request_replan",
];

#[tokio::test]
async fn same_story_different_telemetry_different_director_decision() {
    // Scenario A: a quiet few minutes. The disclosure escalates.
    let calm = rig();
    // Scenario B: the same session start, but the player has been taking a
    // beating. Nothing else differs.
    let pressured = rig();
    pressured.seed(TelemetryEventKind::PlayerDamaged, 4);
    pressured.seed(TelemetryEventKind::PlayerDied, 2);

    assert_eq!(
        calm.narrative(),
        pressured.narrative(),
        "same narrative state"
    );
    assert!(
        !calm
            .telemetry
            .summary(calm.session_id, Utc::now())
            .prefers_dialogue()
    );
    let pressure = pressured
        .telemetry
        .summary(pressured.session_id, Utc::now());
    assert_eq!(pressure.combat_intensity, 40);
    assert_eq!(pressure.recent_deaths, 2);
    assert!(pressure.under_pressure());

    let a = calm.tell_hank().await;
    let b = pressured.tell_hank().await;

    assert_eq!(decided(&a), ESCALATION);
    assert_eq!(decided(&b), BREATHING_ROOM);

    // Both decisions came from the same provider and were validated and
    // applied; only the telemetry differed.
    for outcome in [&a, &b] {
        let decision = outcome.director().decision().unwrap();
        assert_eq!(decision.metadata.provider, "fallback");
    }

    // The difference reaches the game client as different world events.
    let (a_events, b_events) = (event_types(&a), event_types(&b));
    assert!(a_events.contains(&WorldEventType::WorldEventTriggered));
    assert!(!a_events.contains(&WorldEventType::DialogueStarted));
    assert!(b_events.contains(&WorldEventType::DialogueStarted));
    assert!(!b_events.contains(&WorldEventType::WorldEventTriggered));

    // Hank is the one who talks, and the story still diverged either way.
    let decision = b.director().decision().unwrap();
    assert!(decision.actions.iter().any(|action| matches!(
        action,
        DirectorAction::StartDialogue { npc_id, .. } if npc_id == HANK
    )));
    assert_eq!(calm.narrative().plan, pressured.narrative().plan);
}

#[tokio::test]
async fn combat_intensity_alone_is_enough() {
    let rig = rig();
    rig.seed(TelemetryEventKind::PlayerDamaged, 3);
    rig.seed(TelemetryEventKind::EnemyKilled, 2);
    assert_eq!(decided(&rig.tell_hank().await), BREATHING_ROOM);
}

#[tokio::test]
async fn telemetry_the_runtime_itself_recorded_changes_the_decision() {
    // Nothing is seeded: the pressure comes from what the player really did.
    let rig = rig();
    for _ in 0..3 {
        let chat = rig
            .act(ActionType::Speak, Some(HANK), Some(SMALL_TALK))
            .await;
        assert!(chat.follow_up.is_none(), "small talk is not a story event");
    }
    let summary = rig.telemetry.summary(rig.session_id, Utc::now());
    assert_eq!(summary.npc_engagement, 60);
    assert!(summary.socially_engaged() && !summary.under_pressure());

    assert_eq!(decided(&rig.tell_hank().await), BREATHING_ROOM);
}

#[tokio::test]
async fn telemetry_of_another_session_does_not_leak() {
    let telemetry = Arc::new(InMemoryTelemetry::new());
    let rig = rig_reading(telemetry.clone(), telemetry.clone());
    for _ in 0..5 {
        telemetry.record(TelemetryEvent::new(
            Uuid::new_v4(),
            TelemetryEventKind::PlayerDied,
            Utc::now(),
        ));
    }
    assert_eq!(decided(&rig.tell_hank().await), ESCALATION);
}

#[tokio::test]
async fn the_runtime_records_what_actually_happens() {
    let rig = rig();
    assert!(rig.kinds().is_empty());

    rig.act(ActionType::Inspect, Some("test_door"), None).await;
    assert_eq!(rig.kinds(), [TelemetryEventKind::PlayerAction]);

    rig.act(ActionType::Move, Some("somewhere_new"), None).await;
    let events = rig.telemetry.events(rig.session_id);
    assert_eq!(events[2].kind, TelemetryEventKind::LocationEntered);
    assert_eq!(events[2].location.as_deref(), Some("somewhere_new"));
    assert_eq!(events[2].target_id.as_deref(), Some("somewhere_new"));

    // Talking to nobody, or to a door, is not an NPC interaction.
    rig.act(ActionType::Speak, None, Some("Hello?")).await;
    rig.act(ActionType::Interact, Some("test_door"), None).await;
    assert!(!rig.kinds().contains(&TelemetryEventKind::NpcInteraction));

    let before = rig.kinds().len();
    rig.tell_hank().await;
    let events = rig.telemetry.events(rig.session_id);
    let new: Vec<_> = events[before..].iter().map(|event| event.kind).collect();
    assert_eq!(new[0], TelemetryEventKind::PlayerAction);
    assert_eq!(new[1], TelemetryEventKind::NpcInteraction);
    assert!(new.contains(&TelemetryEventKind::DirectorInvoked));
    assert!(new.contains(&TelemetryEventKind::NarrativeReplan));

    let interaction = &events[before + 1];
    assert_eq!(interaction.target_id.as_deref(), Some(HANK));
    assert_eq!(interaction.metadata["disclosure"], true);
    let invoked = events
        .iter()
        .find(|event| event.kind == TelemetryEventKind::DirectorInvoked)
        .unwrap();
    assert_eq!(invoked.metadata["trigger"], "player_disclosure");
    assert_eq!(invoked.metadata["result"], "applied");
    assert_eq!(invoked.metadata["with_telemetry"], true);

    // Telemetry carries structure, never what the player said.
    let recorded = serde_json::to_string(&events).unwrap();
    assert!(!recorded.contains("burner phone"));
    assert!(!recorded.contains("Hello?"));

    // None of the combat kinds: the game has no combat to report.
    for kind in [
        TelemetryEventKind::PlayerDamaged,
        TelemetryEventKind::PlayerDied,
        TelemetryEventKind::EnemyKilled,
    ] {
        assert!(!rig.kinds().contains(&kind));
    }
}

#[tokio::test]
async fn a_rejected_action_records_nothing() {
    let rig = rig();
    let action = ValidatedAction {
        action_id: Uuid::new_v4(),
        session_id: rig.session_id,
        actor_id: "player".into(),
        action_type: ActionType::Inspect,
        target: Some("test_door".into()),
        content: None,
    };
    rig.runtime.process_player_action(&action).await.unwrap();
    // The same action id again is a duplicate.
    assert!(rig.runtime.process_player_action(&action).await.is_err());
    assert_eq!(rig.kinds(), [TelemetryEventKind::PlayerAction]);
}

#[tokio::test]
async fn sessions_without_a_world_are_recorded_too() {
    let telemetry = Arc::new(InMemoryTelemetry::new());
    let runtime =
        Runtime::new(SessionStore::new()).with_telemetry(telemetry.clone(), telemetry.clone());
    let session_id = runtime.create_session().session_id;
    let action = ValidatedAction {
        action_id: Uuid::new_v4(),
        session_id,
        actor_id: "player".into(),
        action_type: ActionType::Move,
        target: Some("yard".into()),
        content: None,
    };
    runtime.process_player_action(&action).await.unwrap();
    let kinds: Vec<_> = telemetry
        .events(session_id)
        .iter()
        .map(|event| event.kind)
        .collect();
    assert_eq!(
        kinds,
        [
            TelemetryEventKind::PlayerAction,
            TelemetryEventKind::LocationEntered
        ]
    );
}

/// A telemetry backend that is down.
struct Unavailable;

impl TelemetryReader for Unavailable {
    fn recent<'a>(
        &'a self,
        _session_id: Uuid,
        _now: DateTime<Utc>,
    ) -> TelemetryFuture<'a, PlayerTelemetry> {
        Box::pin(std::future::ready(Err(TelemetryError::Unavailable(
            "connection refused".to_owned(),
        ))))
    }
}

/// A telemetry backend that never answers.
struct Hanging;

impl TelemetryReader for Hanging {
    fn recent<'a>(
        &'a self,
        _session_id: Uuid,
        _now: DateTime<Utc>,
    ) -> TelemetryFuture<'a, PlayerTelemetry> {
        Box::pin(std::future::pending())
    }
}

/// A telemetry backend that returns nonsense.
struct OutOfRange;

impl TelemetryReader for OutOfRange {
    fn recent<'a>(
        &'a self,
        _session_id: Uuid,
        _now: DateTime<Utc>,
    ) -> TelemetryFuture<'a, PlayerTelemetry> {
        Box::pin(std::future::ready(Ok(PlayerTelemetry {
            window_seconds: 0,
            combat_intensity: 255,
            recent_deaths: 255,
            npc_engagement: 255,
            exploration_activity: 255,
        })))
    }
}

#[tokio::test]
async fn telemetry_failure_never_fails_the_action_or_the_director() {
    let readers: [Arc<dyn TelemetryReader>; 2] = [Arc::new(Unavailable), Arc::new(Hanging)];
    for reader in readers {
        let rig = rig_reading(Arc::new(InMemoryTelemetry::new()), reader);
        // Heavy pressure was recorded, but it cannot be read back.
        rig.seed(TelemetryEventKind::PlayerDied, 5);

        let outcome = rig.tell_hank().await;
        assert_eq!(outcome.event.event_type, WorldEventType::SpeechAcknowledged);
        // The Director decided as if there were no telemetry at all.
        assert_eq!(decided(&outcome), ESCALATION);
    }
}

#[tokio::test]
async fn out_of_range_telemetry_is_clamped_not_fatal() {
    let rig = rig_reading(Arc::new(InMemoryTelemetry::new()), Arc::new(OutOfRange));
    assert_eq!(decided(&rig.tell_hank().await), BREATHING_ROOM);
}
