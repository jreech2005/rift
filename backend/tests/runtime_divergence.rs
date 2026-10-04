//! Phase 2 runtime, end to end: a player action runs through the session,
//! NPC perception, the narrative layer and the Director, and comes back as
//! world events. No network: the Director is scripted or deterministic and
//! memory is the in-memory store.

use std::sync::Arc;
use std::time::Duration;

use rift_backend::action::{ActionType, ValidatedAction, WorldEventType};
use rift_backend::director::{
    DirectorAction, DirectorEngine, DirectorError, FailoverPolicy, FailoverProvider,
    FallbackDirector, IssueCode, Leg, MissionStatus as ViewMissionStatus,
    ObjectiveStatus as ViewObjectiveStatus, ProviderError, ProviderErrorKind, ScriptedProvider,
    ScriptedResponse, Trigger,
};
use rift_backend::narrative::{
    CanonPolicy, MissionStatus, NarrativeState, ObjectiveStatus, ReplanReason,
};
use rift_backend::npc::{
    CharacterId, CharacterState, Disposition, EntityId, FactId, InMemoryMemoryStore, MemoryEntry,
    MemoryStore,
};
use rift_backend::protocol::ErrorCode;
use rift_backend::runtime::{ApplyError, DirectorReport, Runtime, RuntimeError, RuntimeWorld};
use rift_backend::session::{GameSession, SessionStore};
use uuid::Uuid;

const WORLD_BIBLE: &str = include_str!("fixtures/director/world_bible_breaking_bad.json");
const SCENARIO: &str = include_str!("fixtures/runtime/scenario_burner_phone.json");
const DECISION: &str = include_str!("fixtures/runtime/decision_after_disclosure.json");

const SECRET: &str = "secret:burner_phone";
const HANK: &str = "hank_schrader";
const WALTER: &str = "walter_white";
const JESSE: &str = "jesse_pinkman";
const OBJECTIVE: &str = "hide_burner_phone";
const MISSION: &str = "protect_walters_cover";
const TELL_HANK: &str = "Walter has a burner phone hidden under his mattress.";

fn world() -> RuntimeWorld {
    RuntimeWorld::from_world_bible(WORLD_BIBLE)
        .and_then(|world| world.with_scenario_json(SCENARIO))
        .expect("fixtures load")
}

struct Rig {
    runtime: Runtime,
    store: Arc<InMemoryMemoryStore>,
    session_id: Uuid,
}

fn rig(director: DirectorEngine) -> Rig {
    let store = Arc::new(InMemoryMemoryStore::new());
    let runtime = Runtime::new(SessionStore::new())
        .with_world(world())
        .with_director(director)
        .with_memory_store(store.clone());
    let session_id = runtime.create_session().session_id;
    Rig {
        runtime,
        store,
        session_id,
    }
}

/// A rig whose Director answers with `responses`, and nothing else.
fn scripted(responses: Vec<ScriptedResponse>) -> (Rig, Arc<ScriptedProvider>) {
    let provider = Arc::new(ScriptedProvider::new(responses));
    (rig(DirectorEngine::new(provider.clone())), provider)
}

fn text(text: &str) -> ScriptedResponse {
    ScriptedResponse::Text(text.to_owned())
}

fn action(
    session_id: Uuid,
    action_type: ActionType,
    target: Option<&str>,
    content: Option<&str>,
) -> ValidatedAction {
    ValidatedAction {
        action_id: Uuid::new_v4(),
        session_id,
        actor_id: "player".into(),
        action_type,
        target: target.map(str::to_owned),
        content: content.map(str::to_owned),
    }
}

fn tell_hank(session_id: Uuid) -> ValidatedAction {
    action(session_id, ActionType::Speak, Some(HANK), Some(TELL_HANK))
}

fn cid(id: &str) -> CharacterId {
    CharacterId::new(id).unwrap()
}

impl Rig {
    fn knows_secret(&self, npc: &str) -> bool {
        self.runtime
            .npcs()
            .knows(self.session_id, &cid(npc), &FactId::new(SECRET).unwrap())
            .unwrap()
    }

    async fn memories(&self, npc: &str) -> Vec<MemoryEntry> {
        self.store
            .query_recent(self.session_id, &cid(npc), 50)
            .await
            .unwrap()
    }

    fn narrative(&self) -> NarrativeState {
        self.runtime.narrative(self.session_id).unwrap()
    }

    fn session(&self) -> GameSession {
        self.runtime.sessions().get(self.session_id).unwrap()
    }

    fn npc(&self, npc: &str) -> CharacterState {
        self.runtime.npcs().get(self.session_id, &cid(npc)).unwrap()
    }

    /// Everything authoritative, for "nothing changed" assertions.
    fn snapshot(&self) -> (GameSession, NarrativeState, Vec<CharacterState>) {
        (
            self.session(),
            self.narrative(),
            self.runtime.npcs().list(self.session_id),
        )
    }

    fn objective(&self, objective_id: &str) -> ObjectiveStatus {
        self.narrative()
            .plan
            .objective(objective_id)
            .unwrap()
            .1
            .status
    }

    fn mission(&self, mission_id: &str) -> MissionStatus {
        self.narrative().plan.mission(mission_id).unwrap().status
    }

    fn policy(&self, checkpoint_id: &str) -> CanonPolicy {
        self.narrative()
            .plan
            .checkpoint(checkpoint_id)
            .unwrap()
            .policy
    }
}

#[test]
fn a_session_starts_its_story() {
    let rig = rig(DirectorEngine::deterministic());
    let session = rig.session();
    assert_eq!(
        session.current_location.as_deref(),
        Some("albuquerque_hospital")
    );
    assert_eq!(session.event_count, 0);
    assert_eq!(rig.mission(MISSION), MissionStatus::Active);
    assert_eq!(rig.objective(OBJECTIVE), ObjectiveStatus::Active);
    assert_eq!(rig.runtime.npcs().list(rig.session_id).len(), 3);
    // On stage: whoever is in the room with the player.
    let active = rig.runtime.active_npcs(rig.session_id);
    assert!(active.contains(HANK) && active.contains(WALTER) && !active.contains(JESSE));
    assert!(!rig.knows_secret(HANK));
}

/// The Rift thesis: the player was asked to hide Walter's phone and tells
/// Hank instead. The world reacts to what happened, not to the plan.
#[tokio::test]
async fn telling_hank_diverges_the_story_end_to_end() {
    let (rig, provider) = scripted(vec![text(DECISION)]);
    let sid = rig.session_id;

    // --- Deterministic half: session, NPC, narrative. No Director yet. ---
    let applied = rig.runtime.apply_player_action(&tell_hank(sid)).unwrap();
    assert_eq!(provider.call_count(), 0);
    assert_eq!(applied.event.event_type, WorldEventType::SpeechAcknowledged);
    assert_eq!(applied.event.sequence, 1);

    // NPC: Hank, and only Hank, learned it.
    assert!(rig.knows_secret(HANK));
    assert!(!rig.knows_secret(WALTER));
    assert!(!rig.knows_secret(JESSE));
    let follow_up = applied
        .follow_up
        .clone()
        .expect("a disclosure is meaningful");
    assert_eq!(follow_up.memories.len(), 1);
    assert_eq!(follow_up.memories[0].character_id, cid(HANK));

    // Narrative: the objective failed and took the mission with it; nothing
    // was conjured to keep the original plot alive.
    assert_eq!(rig.objective(OBJECTIVE), ObjectiveStatus::Failed);
    assert_eq!(
        rig.objective("earn_walters_trust"),
        ObjectiveStatus::Invalidated
    );
    assert_eq!(rig.mission(MISSION), MissionStatus::Failed);
    let replan = applied
        .replan
        .clone()
        .expect("a failed mission asks for a replan");
    assert_eq!(replan.reason, ReplanReason::MissionFailed);
    assert_eq!(rig.narrative().plan.missions.len(), 1);
    assert_eq!(rig.narrative().world.characters.len(), 3);
    // Canon is not forced: the beat that needed the secret kept cannot happen
    // as written, and the one the disclosure makes possible is now ready.
    assert_eq!(rig.policy("walter_keeps_his_cover"), CanonPolicy::Replace);
    assert_eq!(rig.policy("quiet_discharge"), CanonPolicy::Delete);
    assert_eq!(
        rig.policy("hank_investigates_walter"),
        CanonPolicy::Preserve
    );
    let ready: Vec<String> = rig
        .narrative()
        .ready_checkpoints()
        .into_iter()
        .map(|beat| beat.checkpoint_id)
        .collect();
    assert_eq!(ready, ["hank_investigates_walter"]);
    // The plan's consequences are world state and reach the client.
    assert_eq!(
        rig.session().world_flags.get("hank_suspects_walter"),
        Some(&true)
    );
    let kinds: Vec<_> = applied.consequences.iter().map(|e| e.event_type).collect();
    assert!(kinds.contains(&WorldEventType::MissionUpdated));
    assert!(kinds.contains(&WorldEventType::ObjectiveUpdated));
    assert!(
        applied
            .consequences
            .iter()
            .all(|e| e.payload["source"] == "narrative")
    );

    // Director context: built after all of the above.
    let ctx = follow_up.context.clone().expect("director context");
    ctx.validate().expect("context is valid");
    assert_eq!(
        ctx.trigger,
        Trigger::PlayerDisclosure {
            npc_id: HANK.into(),
            objective_id: Some(OBJECTIVE.into()),
        }
    );
    assert_eq!(ctx.event_count, rig.session().event_count);
    assert_eq!(
        ctx.objective(OBJECTIVE).unwrap().status,
        ViewObjectiveStatus::Failed
    );
    assert_eq!(
        ctx.objective(OBJECTIVE).unwrap().giver_npc_id.as_deref(),
        Some(WALTER)
    );
    assert_eq!(
        ctx.mission(MISSION).unwrap().status,
        ViewMissionStatus::Failed
    );
    assert_eq!(ctx.world_flags.get("hank_suspects_walter"), Some(&true));
    let summary = ctx.narrative.as_ref().unwrap().summary.clone().unwrap();
    assert!(summary.contains("walter_keeps_his_cover"), "{summary}");
    assert!(summary.contains("hank_investigates_walter"), "{summary}");
    // The permitted NPC view: where they are and how they stand, never what
    // they know.
    assert_eq!(ctx.npcs.len(), 3);
    assert!(ctx.npc(HANK).unwrap().active && ctx.npc(HANK).unwrap().alive);
    let ctx_json = serde_json::to_string(&ctx).unwrap();
    assert!(!ctx_json.contains(SECRET));
    assert!(!ctx_json.contains("is hiding a burner phone"));

    // Before the slow half runs, nobody else has any memory of it.
    assert!(rig.memories(HANK).await.is_empty());

    // --- Slow half: memories, one Director decision, validated apply. ---
    let before_director = rig.session().event_count;
    let outcome = rig.runtime.complete(follow_up).await;
    assert_eq!(provider.call_count(), 1);
    assert!(outcome.memory_errors.is_empty());
    let DirectorReport::Applied { decision, deferred } = &outcome.director else {
        panic!("decision should be applied: {:?}", outcome.director);
    };
    assert_eq!(decision.metadata.provider, "scripted");
    assert_eq!(decision.metadata.based_on_event_count, before_director);
    assert_eq!(decision.actions.len(), 6);
    assert_eq!(deferred.len(), 1);
    assert_eq!(rig.runtime.replan_notes(sid), *deferred);

    // The decision changed authoritative state...
    let session = rig.session();
    assert_eq!(
        session.world_flags.get("hank_knows_about_phone"),
        Some(&true)
    );
    assert!(rig.narrative().world.flag("hank_knows_about_phone"));
    assert_eq!(
        rig.npc(HANK)
            .disposition_toward(&EntityId::new(WALTER).unwrap()),
        Disposition::Wary
    );
    assert_eq!(
        rig.objective("choose_what_to_tell_hank"),
        ObjectiveStatus::Active
    );

    // ...and came back as world events for Unreal, in session order.
    let kinds: Vec<_> = outcome.events.iter().map(|e| e.event_type).collect();
    assert_eq!(
        kinds,
        [
            WorldEventType::WorldFlagChanged,
            WorldEventType::NpcDispositionChanged,
            WorldEventType::WorldEventTriggered,
            WorldEventType::DialogueStarted,
            WorldEventType::ObjectiveUpdated,
        ]
    );
    assert!(
        outcome
            .events
            .iter()
            .all(|e| e.payload["source"] == "director")
    );
    let sequences: Vec<u64> = outcome.events.iter().map(|e| e.sequence).collect();
    let expected: Vec<u64> = (before_director + 1..=session.event_count).collect();
    assert_eq!(sequences, expected);
    assert_eq!(outcome.events[2].payload["event"], "confrontation_begins");
    assert_eq!(
        outcome.events[4].target.as_deref(),
        Some("choose_what_to_tell_hank")
    );
    assert_eq!(outcome.events[4].payload["status"], "active");

    // Memory: Hank remembers being told, then the confrontation; Walter only
    // the confrontation. Jesse was not there.
    assert_eq!(rig.memories(HANK).await.len(), 2);
    assert_eq!(rig.memories(WALTER).await.len(), 1);
    assert!(rig.memories(JESSE).await.is_empty());
    assert_eq!(outcome.memories_stored, 3);

    // Privacy survived the whole chain.
    assert!(!rig.knows_secret(WALTER));
    assert!(!rig.knows_secret(JESSE));
    // The original path was not quietly restored.
    assert_eq!(rig.objective(OBJECTIVE), ObjectiveStatus::Failed);
    assert_eq!(rig.mission(MISSION), MissionStatus::Failed);
    assert_eq!(rig.policy("walter_keeps_his_cover"), CanonPolicy::Replace);
}

#[tokio::test]
async fn the_deterministic_director_answers_a_divergence() {
    let rig = rig(DirectorEngine::deterministic());
    let outcome = rig
        .runtime
        .process_player_action(&tell_hank(rig.session_id))
        .await
        .unwrap();

    let DirectorReport::Applied { decision, deferred } = outcome.director() else {
        panic!("fallback decision should apply: {:?}", outcome.director());
    };
    assert_eq!(decision.metadata.provider, "fallback");
    assert_eq!(deferred.len(), 1);
    // The narrative layer had already failed the objective, so the Director
    // does not fail it again; it deals with the fallout.
    assert!(!decision.actions.iter().any(|a| matches!(
        a,
        DirectorAction::FailObjective { .. } | DirectorAction::InvalidateMission { .. }
    )));
    assert_eq!(
        rig.session().world_flags.get("disclosed_to:hank_schrader"),
        Some(&true)
    );
    assert_eq!(
        rig.objective(&format!("aftermath_{OBJECTIVE}")),
        ObjectiveStatus::Active
    );
    assert!(outcome.events().count() > 1 + outcome.consequences.len());
    assert!(rig.knows_secret(HANK) && !rig.knows_secret(WALTER) && !rig.knows_secret(JESSE));
}

/// The expected path still works: hiding the phone completes the objective
/// and nothing diverges.
#[tokio::test]
async fn hiding_the_phone_follows_the_plan() {
    let (rig, provider) = scripted(vec![text(
        r#"{"reason_code":"no_change","actions":[],"narrative_summary":null,"confidence":1.0}"#,
    )]);
    let hide = action(rig.session_id, ActionType::Interact, Some("mattress"), None);
    let applied = rig.runtime.apply_player_action(&hide).unwrap();

    assert_eq!(
        applied.event.event_type,
        WorldEventType::InteractionAcknowledged
    );
    assert!(applied.replan.is_none());
    assert_eq!(rig.objective(OBJECTIVE), ObjectiveStatus::Completed);
    assert_eq!(rig.objective("earn_walters_trust"), ObjectiveStatus::Active);
    assert_eq!(rig.mission(MISSION), MissionStatus::Active);
    assert!(rig.narrative().divergences().is_empty());
    assert!(!rig.knows_secret(HANK));

    let follow_up = applied.follow_up.unwrap();
    assert!(follow_up.memories.is_empty());
    assert_eq!(
        follow_up.context.as_ref().unwrap().trigger,
        Trigger::ObjectiveCompleted {
            objective_id: OBJECTIVE.into()
        }
    );
    let outcome = rig.runtime.complete(follow_up).await;
    assert!(outcome.director.is_applied());
    assert!(outcome.events.is_empty());
    assert_eq!(provider.call_count(), 1);
}

#[tokio::test]
async fn an_action_that_changes_nothing_does_not_call_the_director() {
    let (rig, provider) = scripted(vec![text(DECISION)]);
    let sid = rig.session_id;
    for action in [
        action(sid, ActionType::Inspect, Some("test_door"), None),
        action(sid, ActionType::Interact, Some("test_door"), None),
        action(
            sid,
            ActionType::Speak,
            Some(HANK),
            Some("Visiting hours are over."),
        ),
        action(sid, ActionType::Move, Some("the_roof"), None),
    ] {
        let outcome = rig.runtime.process_player_action(&action).await.unwrap();
        assert!(outcome.consequences.is_empty());
        assert!(outcome.follow_up.is_none());
        assert!(matches!(outcome.director(), DirectorReport::NotInvoked));
    }
    assert_eq!(provider.call_count(), 0);
    assert_eq!(rig.session().event_count, 4);
    assert_eq!(rig.objective(OBJECTIVE), ObjectiveStatus::Active);
    assert!(rig.store.is_empty());
}

#[tokio::test]
async fn a_duplicate_action_does_not_run_orchestration_twice() {
    let (rig, provider) = scripted(vec![text(DECISION), text(DECISION)]);
    let tell = tell_hank(rig.session_id);
    rig.runtime.process_player_action(&tell).await.unwrap();
    assert_eq!(provider.call_count(), 1);
    let before = rig.snapshot();
    let stored = rig.store.len();

    let err = rig.runtime.process_player_action(&tell).await.unwrap_err();
    assert_eq!(err.to_protocol().code, ErrorCode::DuplicateAction);
    assert_eq!(provider.call_count(), 1);
    assert_eq!(rig.snapshot(), before);
    assert_eq!(rig.store.len(), stored);
}

#[tokio::test]
async fn a_rejected_action_mutates_nothing() {
    let (rig, provider) = scripted(vec![text(DECISION)]);
    let before = rig.snapshot();

    // Unknown session.
    let err = rig
        .runtime
        .process_player_action(&tell_hank(Uuid::new_v4()))
        .await
        .unwrap_err();
    assert!(matches!(&err, RuntimeError::Protocol(e) if e.code == ErrorCode::SessionNotFound));

    // A replayed action id, even with content that would otherwise diverge
    // the story.
    let inspect = action(rig.session_id, ActionType::Inspect, Some("chart"), None);
    rig.runtime.process_player_action(&inspect).await.unwrap();
    let after_inspect = rig.snapshot();
    let replay = ValidatedAction {
        action_id: inspect.action_id,
        ..tell_hank(rig.session_id)
    };
    let err = rig
        .runtime
        .process_player_action(&replay)
        .await
        .unwrap_err();
    assert_eq!(err.to_protocol().code, ErrorCode::DuplicateAction);

    assert_eq!(rig.snapshot(), after_inspect);
    assert_eq!(after_inspect.1, before.1, "narrative untouched");
    assert_eq!(after_inspect.2, before.2, "npcs untouched");
    assert!(!rig.knows_secret(HANK));
    assert_eq!(provider.call_count(), 0);
    assert!(rig.store.is_empty());
}

fn unavailable() -> ScriptedResponse {
    ScriptedResponse::Error(
        ProviderError::new("gemini", ProviderErrorKind::Unavailable, "upstream 503")
            .with_status(503),
    )
}

#[tokio::test]
async fn provider_failure_falls_back_without_corrupting_state() {
    let provider = Arc::new(ScriptedProvider::new(vec![unavailable()]));
    let rig = rig(DirectorEngine::new(provider.clone()).with_fallback(Arc::new(FallbackDirector)));

    let outcome = rig
        .runtime
        .process_player_action(&tell_hank(rig.session_id))
        .await
        .unwrap();
    assert_eq!(provider.call_count(), 1, "a provider error is not retried");
    let DirectorReport::Applied { decision, .. } = outcome.director() else {
        panic!("fallback should apply: {:?}", outcome.director());
    };
    assert_eq!(decision.metadata.provider, "fallback");
    assert!(
        decision
            .metadata
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("503")
    );
    assert_eq!(rig.objective(OBJECTIVE), ObjectiveStatus::Failed);
    assert_eq!(
        rig.session().world_flags.get("disclosed_to:hank_schrader"),
        Some(&true)
    );
}

/// The whole chain down: one player action is one Director invocation, one
/// bounded pass over the legs and one deterministic decision. Applying that
/// decision does not invoke the Director again.
#[tokio::test]
async fn a_failed_llm_chain_is_bounded_and_never_reinvokes_the_director() {
    let leg = || Arc::new(ScriptedProvider::new(vec![unavailable(); 8]));
    let (primary, backup, other) = (leg(), leg(), leg());
    let timeout = Duration::from_secs(1);
    let chain = FailoverProvider::new(vec![
        Leg::new(primary.clone(), "primary", timeout).with_retries(1),
        Leg::new(backup.clone(), "backup", timeout),
        Leg::new(other.clone(), "other", timeout),
    ])
    .with_policy(FailoverPolicy {
        backoff: Duration::from_millis(5),
        max_backoff: Duration::from_millis(5),
        budget: timeout,
    });
    let rig = rig(DirectorEngine::new(Arc::new(chain)).with_fallback(Arc::new(FallbackDirector)));

    let outcome = rig
        .runtime
        .process_player_action(&tell_hank(rig.session_id))
        .await
        .unwrap();
    let DirectorReport::Applied { decision, .. } = outcome.director() else {
        panic!("fallback should apply: {:?}", outcome.director());
    };
    assert_eq!(decision.metadata.provider, "fallback");
    assert_eq!(decision.metadata.attempts, 1);
    assert_eq!(rig.objective(OBJECTIVE), ObjectiveStatus::Failed);
    assert_eq!(
        (
            primary.call_count(),
            backup.call_count(),
            other.call_count()
        ),
        (2, 1, 1),
        "one pass: applying the decision must not call the Director again"
    );
}

#[tokio::test]
async fn a_director_with_no_answer_leaves_the_action_standing() {
    for kind in [
        ProviderErrorKind::Unavailable,
        ProviderErrorKind::RateLimited,
        ProviderErrorKind::Timeout,
    ] {
        let (rig, provider) = scripted(vec![ScriptedResponse::Error(ProviderError::new(
            "gemini", kind, "down",
        ))]);
        let applied = rig
            .runtime
            .apply_player_action(&tell_hank(rig.session_id))
            .unwrap();
        let after_action = rig.snapshot();

        let outcome = rig.runtime.complete(applied.follow_up.unwrap()).await;
        assert!(
            matches!(&outcome.director, DirectorReport::Failed(DirectorError::Provider(e)) if e.kind == kind)
        );
        assert!(outcome.events.is_empty());
        assert_eq!(provider.call_count(), 1);
        // The session survives and the deterministic consequences stand.
        assert_eq!(rig.snapshot(), after_action);
        assert_eq!(rig.objective(OBJECTIVE), ObjectiveStatus::Failed);
        assert!(rig.knows_secret(HANK));
        assert_eq!(rig.memories(HANK).await.len(), 1);
        // And the game goes on.
        let next = action(rig.session_id, ActionType::Inspect, Some("chart"), None);
        assert!(rig.runtime.process_player_action(&next).await.is_ok());
    }
}

#[tokio::test]
async fn invalid_director_output_is_never_applied() {
    // Both attempts try to rewrite a flag owned by the player-action rules.
    let bad = r#"{"reason_code":"world_reaction","narrative_summary":"x","confidence":0.5,
        "actions":[{"type":"set_world_flag","action_id":"a1","flag":"hank_knows"},
                   {"type":"clear_world_flag","action_id":"a2","flag":"interacted:mattress"}]}"#;
    let (rig, provider) = scripted(vec![text(bad), text(bad)]);
    let applied = rig
        .runtime
        .apply_player_action(&tell_hank(rig.session_id))
        .unwrap();
    let after_action = rig.snapshot();

    let outcome = rig.runtime.complete(applied.follow_up.unwrap()).await;
    assert!(matches!(
        outcome.director,
        DirectorReport::Failed(DirectorError::InvalidDecision { attempts: 2, .. })
    ));
    assert_eq!(provider.call_count(), 2, "one repair attempt, no more");
    assert_eq!(rig.snapshot(), after_action);
    assert!(!rig.session().world_flags.contains_key("hank_knows"));
}

/// One incoming action, at most one decision: what the Director's actions
/// produce never comes back around as a trigger.
#[tokio::test]
async fn the_director_runs_at_most_once_per_action() {
    // A second response is available; it must not be consumed.
    let (rig, provider) = scripted(vec![text(DECISION), text(DECISION)]);
    let outcome = rig
        .runtime
        .process_player_action(&tell_hank(rig.session_id))
        .await
        .unwrap();
    assert!(outcome.director().is_applied());
    // The decision set an objective, raised a world event and asked for a
    // replan. None of that invoked the Director again.
    assert_eq!(provider.call_count(), 1);
    assert_eq!(rig.runtime.replan_notes(rig.session_id).len(), 1);

    let look = action(rig.session_id, ActionType::Inspect, Some("chart"), None);
    rig.runtime.process_player_action(&look).await.unwrap();
    assert_eq!(provider.call_count(), 1);
}

#[tokio::test]
async fn a_stale_decision_is_rejected_whole() {
    let (rig, _) = scripted(vec![text(DECISION)]);
    let outcome = rig
        .runtime
        .process_player_action(&tell_hank(rig.session_id))
        .await
        .unwrap();
    let decision = outcome.director().decision().unwrap().clone();
    let before = rig.snapshot();

    // The same decision again: valid when it was made, stale now (its
    // objective already exists). Its first four actions would still apply on
    // their own; none of them may.
    let err = rig.runtime.apply_decision(&decision).unwrap_err();
    let ApplyError::Invalid(issues) = &err else {
        panic!("expected a validation rejection, got {err:?}");
    };
    assert!(issues.iter().any(|i| i.code == IssueCode::DuplicateId));
    assert_eq!(rig.snapshot(), before);
    assert_eq!(rig.runtime.replan_notes(rig.session_id).len(), 1);
}

#[tokio::test]
async fn a_tampered_decision_is_rejected_whole() {
    let (rig, _) = scripted(vec![text(DECISION)]);
    let applied = rig
        .runtime
        .apply_player_action(&tell_hank(rig.session_id))
        .unwrap();
    // Make a decision without applying it.
    let ctx = applied.follow_up.unwrap().context.unwrap();
    let engine = DirectorEngine::new(Arc::new(ScriptedProvider::texts([DECISION])));
    let decision = engine.decide(&ctx).await.unwrap();
    let before = rig.snapshot();

    let tamper = |action: DirectorAction| {
        let mut tampered = decision.clone();
        tampered.actions.push(action);
        tampered
    };
    let cases = [
        (
            tamper(DirectorAction::SetWorldFlag {
                action_id: "x1".into(),
                flag: "interacted:test_door".into(),
            }),
            IssueCode::InvalidValue,
        ),
        (
            tamper(DirectorAction::StartDialogue {
                action_id: "x2".into(),
                npc_id: "gus_fring".into(),
                opening_line: "We have not met.".into(),
            }),
            IssueCode::UnknownReference,
        ),
        (
            // The original path may not be restored: the objective is over.
            tamper(DirectorAction::CompleteObjective {
                action_id: "x3".into(),
                objective_id: OBJECTIVE.into(),
            }),
            IssueCode::InvalidCombination,
        ),
    ];
    for (tampered, code) in cases {
        let err = rig.runtime.apply_decision(&tampered).unwrap_err();
        assert!(
            matches!(&err, ApplyError::Invalid(issues) if issues.iter().any(|i| i.code == code)),
            "{err:?}"
        );
        assert_eq!(rig.snapshot(), before, "nothing may be applied");
    }

    let mut elsewhere = decision.clone();
    elsewhere.universe_id = "another_universe".into();
    assert!(matches!(
        rig.runtime.apply_decision(&elsewhere),
        Err(ApplyError::WrongUniverse { .. })
    ));
    let mut nowhere = decision.clone();
    nowhere.session_id = Uuid::new_v4();
    assert!(matches!(
        rig.runtime.apply_decision(&nowhere),
        Err(ApplyError::NoStory(_))
    ));
    assert_eq!(rig.snapshot(), before);

    // Untouched, it applies.
    assert!(rig.runtime.apply_decision(&decision).is_ok());
}

#[tokio::test]
async fn the_dead_stay_dead() {
    let (rig, _) = scripted(vec![]);
    rig.runtime
        .npcs()
        .update(rig.session_id, &cid(WALTER), |s| {
            s.kill(chrono::Utc::now());
            Ok(())
        })
        .unwrap();
    let applied = rig
        .runtime
        .apply_player_action(&tell_hank(rig.session_id))
        .unwrap();
    let ctx = applied.follow_up.unwrap().context.unwrap();
    assert!(!ctx.npc(WALTER).unwrap().alive && !ctx.npc(WALTER).unwrap().active);

    let engine = DirectorEngine::new(Arc::new(ScriptedProvider::texts([DECISION, DECISION])));
    // The scripted decision has Walter witness a confrontation.
    let err = engine.decide(&ctx).await.unwrap_err();
    assert!(matches!(err, DirectorError::InvalidDecision { .. }));

    // Talking to the dead discloses nothing.
    let before = rig.narrative();
    let to_walter = action(
        rig.session_id,
        ActionType::Speak,
        Some(WALTER),
        Some(TELL_HANK),
    );
    let applied = rig.runtime.apply_player_action(&to_walter).unwrap();
    assert!(applied.follow_up.is_none());
    assert_eq!(rig.narrative(), before);
}

/// Private knowledge stays private through the whole chain, including what
/// the Director itself reveals.
#[tokio::test]
async fn revealed_information_reaches_only_its_recipient() {
    let whisper = "Hank Schrader has started asking about a hidden phone.";
    let decision = format!(
        r#"{{"reason_code":"npc_reaction","narrative_summary":"Word gets to Jesse.","confidence":0.7,
        "actions":[
          {{"type":"reveal_information","action_id":"a1","recipient_id":"{JESSE}","text":"{whisper}","source_npc_id":"{WALTER}"}},
          {{"type":"reveal_information","action_id":"a2","recipient_id":"player","text":"Somewhere a phone starts ringing."}}
        ]}}"#
    );
    let (rig, _) = scripted(vec![text(&decision)]);
    let outcome = rig
        .runtime
        .process_player_action(&tell_hank(rig.session_id))
        .await
        .unwrap();
    assert!(outcome.director().is_applied(), "{:?}", outcome.director());

    let told = |npc: &str| {
        rig.npc(npc)
            .knowledge()
            .values()
            .any(|fact| fact.statement == whisper)
    };
    assert!(told(JESSE));
    // Walter was named as the source but does not know it, so he is not made
    // to know it; Hank was never part of it.
    assert!(!told(WALTER) && !told(HANK));
    assert_eq!(rig.memories(JESSE).await.len(), 1);
    assert!(rig.memories(WALTER).await.is_empty());
    // And the player's secret went no further than Hank.
    assert!(rig.knows_secret(HANK) && !rig.knows_secret(JESSE) && !rig.knows_secret(WALTER));

    // The client is told that Jesse learned something, not what.
    let events = &outcome.follow_up.as_ref().unwrap().events;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event_type, WorldEventType::InformationRevealed);
    assert_eq!(events[0].target.as_deref(), Some(JESSE));
    assert!(events[0].payload.get("text").is_none());
    assert!(!events[0].payload.to_string().contains("asking about"));
    assert_eq!(events[1].target.as_deref(), Some("player"));
    assert_eq!(
        events[1].payload["text"],
        "Somewhere a phone starts ringing."
    );
}

/// Without a world the runtime is the plain protocol V1 path: the behaviour
/// `make smoke` and Unreal's `Rift.NetSmoke` rely on.
#[tokio::test]
async fn a_session_without_a_world_takes_the_plain_path() {
    let runtime = Runtime::default();
    let sid = runtime.create_session().session_id;
    let door = action(sid, ActionType::Interact, Some("test_door"), None);
    let outcome = runtime.process_player_action(&door).await.unwrap();

    assert_eq!(
        outcome.event.event_type,
        WorldEventType::InteractionAcknowledged
    );
    assert_eq!(outcome.event.target.as_deref(), Some("test_door"));
    assert_eq!(
        outcome.event.event_id,
        rift_backend::action::event_id_for(door.action_id)
    );
    assert!(outcome.consequences.is_empty() && outcome.follow_up.is_none());
    assert_eq!(runtime.sessions().get(sid).unwrap().event_count, 1);
    assert!(runtime.narrative(sid).is_none());

    let err = runtime.process_player_action(&door).await.unwrap_err();
    assert_eq!(err.to_protocol().code, ErrorCode::DuplicateAction);
}

#[test]
fn a_bad_scenario_is_refused_at_load() {
    let base = || RuntimeWorld::from_world_bible(WORLD_BIBLE).unwrap();
    let edit = |f: &dyn Fn(&mut serde_json::Value)| {
        let mut scenario: serde_json::Value = serde_json::from_str(SCENARIO).unwrap();
        f(&mut scenario);
        base().with_scenario_json(&scenario.to_string())
    };
    assert!(edit(&|_| {}).is_ok());
    assert!(edit(&|s| s["plan"]["universe_id"] = "elsewhere".into()).is_err());
    assert!(edit(&|s| s["secrets"][0]["keywords"] = serde_json::json!([])).is_err());
    assert!(edit(&|s| s["secrets"][0]["keywords"] = serde_json::json!(["it"])).is_err());
    assert!(edit(&|s| s["secrets"][0]["objective_id"] = "no_such_objective".into()).is_err());
    assert!(edit(&|s| s["secrets"][0]["fact_id"] = "not a fact id".into()).is_err());
    assert!(edit(&|s| s["world"]["player_location"] = "atlantis".into()).is_err());
    // The WorldBible alone is a playable, if minimal, world.
    assert!(base().secrets().is_empty());
}

/// Every Director action kind lands in authoritative state and, where the
/// client has something to show, in a world event.
#[tokio::test]
async fn every_director_action_kind_is_applied() {
    let no_change =
        r#"{"reason_code":"no_change","actions":[],"narrative_summary":null,"confidence":1.0}"#;
    let (rig, _) = scripted(vec![text(no_change)]);
    let sid = rig.session_id;
    let hide = action(sid, ActionType::Interact, Some("mattress"), None);
    let outcome = rig.runtime.process_player_action(&hide).await.unwrap();
    let base = outcome.director().decision().unwrap().clone();
    let decision = |actions: Vec<DirectorAction>| {
        let mut decision = base.clone();
        decision.decision_id = Uuid::new_v4();
        decision.actions = actions;
        decision
    };
    let id = |n: u32| format!("a{n}");

    let first = rig
        .runtime
        .apply_decision(&decision(vec![
            DirectorAction::ActivateNpc {
                action_id: id(1),
                npc_id: JESSE.into(),
                location_id: "albuquerque_hospital".into(),
            },
            DirectorAction::MoveNpc {
                action_id: id(2),
                npc_id: HANK.into(),
                location_id: "pinkman_residence".into(),
                reason: "Hank follows a hunch.".into(),
            },
            DirectorAction::SetNpcDisposition {
                action_id: id(3),
                npc_id: WALTER.into(),
                toward: "player".into(),
                disposition: rift_backend::director::Disposition::Hostile,
                reason: "Walter decides the orderly knows too much.".into(),
            },
            DirectorAction::SetWorldFlag {
                action_id: id(4),
                flag: "ward_locked_down".into(),
            },
            DirectorAction::InvalidateMission {
                action_id: id(5),
                mission_id: MISSION.into(),
                reason: "Walter no longer wants help.".into(),
            },
            DirectorAction::SetObjective {
                action_id: id(6),
                objective_id: "lie_low".into(),
                title: "Lie low".into(),
                description: "Stay out of Walter's way until the shift ends.".into(),
                mission_id: None,
            },
            DirectorAction::SetObjective {
                action_id: id(7),
                objective_id: "call_for_help".into(),
                title: "Call for help".into(),
                description: "Find someone who can get you out of this.".into(),
                mission_id: None,
            },
            DirectorAction::RequestReplan {
                action_id: id(8),
                reason: "The hospital arc is over.".into(),
                mission_id: None,
            },
        ]))
        .unwrap();

    assert!(rig.runtime.active_npcs(sid).contains(JESSE));
    assert_eq!(
        rig.npc(JESSE).location().map(|l| l.as_str()),
        Some("albuquerque_hospital")
    );
    assert_eq!(
        rig.npc(HANK).location().map(|l| l.as_str()),
        Some("pinkman_residence")
    );
    let narrative = rig.narrative();
    assert_eq!(
        narrative.world.characters[HANK].location.as_deref(),
        Some("pinkman_residence")
    );
    assert_eq!(
        rig.npc(WALTER).disposition_toward_player(),
        Disposition::Hostile
    );
    assert_eq!(
        rig.session().world_flags.get("ward_locked_down"),
        Some(&true)
    );
    assert!(narrative.world.flag("ward_locked_down"));
    // Invalidated, not failed: its failure effects did not fire.
    assert_eq!(rig.mission(MISSION), MissionStatus::Invalidated);
    assert_eq!(
        rig.objective("earn_walters_trust"),
        ObjectiveStatus::Invalidated
    );
    assert_eq!(rig.objective(OBJECTIVE), ObjectiveStatus::Completed);
    assert_eq!(rig.objective("lie_low"), ObjectiveStatus::Active);
    assert_eq!(first.deferred.len(), 1);
    let kinds: Vec<_> = first.events.iter().map(|e| e.event_type).collect();
    assert_eq!(
        kinds,
        [
            WorldEventType::NpcActivated,
            WorldEventType::NpcMoved,
            WorldEventType::NpcDispositionChanged,
            WorldEventType::WorldFlagChanged,
            WorldEventType::MissionUpdated,
            WorldEventType::ObjectiveUpdated,
            WorldEventType::ObjectiveUpdated,
            WorldEventType::ObjectiveUpdated,
        ]
    );
    assert_eq!(first.events[4].payload["status"], "invalidated");

    let second = rig
        .runtime
        .apply_decision(&decision(vec![
            DirectorAction::ClearWorldFlag {
                action_id: id(1),
                flag: "ward_locked_down".into(),
            },
            DirectorAction::CompleteObjective {
                action_id: id(2),
                objective_id: "lie_low".into(),
            },
            DirectorAction::FailObjective {
                action_id: id(3),
                objective_id: "call_for_help".into(),
                reason: "Nobody picks up.".into(),
            },
            DirectorAction::TriggerWorldEvent {
                action_id: id(4),
                event: rift_backend::director::WorldEventKind::AlarmRaised,
                description: "A code alarm sounds down the corridor.".into(),
                location_id: Some("albuquerque_hospital".into()),
                npc_ids: Vec::new(),
            },
            DirectorAction::StartDialogue {
                action_id: id(5),
                npc_id: JESSE.into(),
                opening_line: "Yo, what did you do?".into(),
            },
            DirectorAction::RevealInformation {
                action_id: id(6),
                recipient_id: "player".into(),
                text: "The alarm is for Walter's room.".into(),
                source_npc_id: Some(JESSE.into()),
            },
        ]))
        .unwrap();

    assert_eq!(
        rig.session().world_flags.get("ward_locked_down"),
        Some(&false)
    );
    assert!(!rig.narrative().world.flag("ward_locked_down"));
    assert_eq!(rig.objective("lie_low"), ObjectiveStatus::Completed);
    assert_eq!(rig.objective("call_for_help"), ObjectiveStatus::Failed);
    // The alarm is perceived by whoever is at the hospital: Walter and Jesse
    // now, not Hank, who left.
    let heard: Vec<&str> = second
        .memories
        .iter()
        .map(|m| m.character_id.as_str())
        .collect();
    assert_eq!(heard, [JESSE, WALTER]);
    let kinds: Vec<_> = second.events.iter().map(|e| e.event_type).collect();
    assert_eq!(
        kinds,
        [
            WorldEventType::WorldFlagChanged,
            WorldEventType::ObjectiveUpdated,
            WorldEventType::ObjectiveUpdated,
            WorldEventType::WorldEventTriggered,
            WorldEventType::DialogueStarted,
            WorldEventType::InformationRevealed,
        ]
    );
    // Sequence numbers never repeat or skip.
    let session = rig.session();
    let sequences: Vec<u64> = session.recent_events.iter().map(|e| e.sequence).collect();
    assert_eq!(sequences, (1..=session.event_count).collect::<Vec<_>>());
}
