//! The key demo scenario, end to end and offline: the player is given an
//! objective, deliberately does the opposite, and the Director's decision
//! invalidates the mission, records the divergence, changes how characters
//! feel, stages a world event and hands the player a replacement objective.
//!
//! The WorldBible is the real one compiled in Phase 1. Nothing in the
//! Director is specific to it; the unit tests run the same logic on an
//! unrelated universe.

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::{DECISION, hank_context};
use rift_backend::director::{
    DirectorAction, DirectorDecision, DirectorEngine, DirectorError, Disposition, IssueCode,
    MissionStatus, ObjectiveStatus, ReasonCode, ScriptedProvider, Trigger, parse_proposal,
    validate_actions,
};
use serde_json::{Value, json};

const DIVERGENCE_ACTIONS: [&str; 5] = [
    "invalidate_mission",
    "set_world_flag",
    "set_objective",
    "set_npc_disposition",
    "trigger_world_event",
];

fn types(actions: &[DirectorAction]) -> BTreeSet<&'static str> {
    actions.iter().map(DirectorAction::type_name).collect()
}

fn tampered(edit: impl FnOnce(&mut Value)) -> String {
    let mut value: Value = serde_json::from_str(DECISION).unwrap();
    edit(&mut value);
    value.to_string()
}

#[test]
fn scenario_context_is_valid_and_bounded() {
    let ctx = hank_context();
    assert_eq!(ctx.validate(), Ok(()));
    assert_eq!(ctx.universe_id, "breaking_bad_tv_1396");
    assert_eq!(
        ctx.trigger,
        Trigger::PlayerDisclosure {
            npc_id: "hank_schrader".into(),
            objective_id: Some("hide_burner_phone".into()),
        }
    );
    let bytes = serde_json::to_vec(&ctx).unwrap().len();
    assert!(bytes < 8 * 1024, "context is {bytes} B");
    assert!(
        bytes < common::WORLD_BIBLE.len(),
        "smaller than the raw WorldBible"
    );
}

#[test]
fn representative_divergence_decision_validates() {
    let ctx = hank_context();
    let proposal = parse_proposal(&ctx, DECISION).unwrap_or_else(|issues| panic!("{issues:#?}"));

    assert_eq!(proposal.reason_code, ReasonCode::PlayerDivergence);
    let present = types(&proposal.actions);
    for expected in DIVERGENCE_ACTIONS {
        assert!(present.contains(expected), "missing {expected}");
    }
    assert!(
        proposal
            .actions
            .contains(&DirectorAction::InvalidateMission {
                action_id: "a2".into(),
                mission_id: "protect_walters_cover".into(),
                reason:
                    "Hank now knows Walter has a hidden phone, so the cover cannot be protected."
                        .into(),
            })
    );
    assert!(proposal.actions.iter().any(|a| matches!(
        a,
        DirectorAction::SetNpcDisposition { npc_id, toward, disposition: Disposition::Hostile, .. }
            if npc_id == "walter_white" && toward == "player"
    )));
    // The replacement objective is new and not tied to the dead mission.
    assert!(proposal.actions.iter().any(|a| matches!(
        a,
        DirectorAction::SetObjective { objective_id, mission_id: None, .. }
            if objective_id == "choose_what_to_tell_hank"
    )));
}

#[tokio::test]
async fn engine_turns_the_fixture_into_a_director_decision() {
    let ctx = hank_context();
    let provider = Arc::new(ScriptedProvider::texts([DECISION]));
    let decision = DirectorEngine::new(provider.clone())
        .decide(&ctx)
        .await
        .unwrap();

    assert_eq!(decision.session_id, ctx.session_id);
    assert_eq!(decision.trigger, ctx.trigger);
    assert_eq!(decision.metadata.attempts, 1);
    assert_eq!(decision.metadata.based_on_event_count, 6);
    assert_eq!(decision.actions.len(), 7);
    assert_eq!(provider.call_count(), 1);

    // The decision is plain JSON a later phase can store, log or forward.
    let wire = serde_json::to_value(&decision).unwrap();
    assert_eq!(wire["schema_version"], 1);
    assert_eq!(wire["reason_code"], "player_divergence");
    assert_eq!(wire["actions"][1]["type"], "invalidate_mission");
    assert_eq!(
        wire["actions"][2],
        json!({
            "type": "set_world_flag", "action_id": "a3", "flag": "hank_knows_about_phone"
        })
    );
    let back: DirectorDecision = serde_json::from_value(wire).unwrap();
    assert_eq!(back, decision);
}

#[tokio::test]
async fn deterministic_fallback_represents_the_same_divergence() {
    let ctx = hank_context();
    let decision = DirectorEngine::deterministic().decide(&ctx).await.unwrap();

    assert_eq!(decision.metadata.provider, "fallback");
    assert_eq!(decision.reason_code, ReasonCode::PlayerDivergence);
    let present = types(&decision.actions);
    for expected in DIVERGENCE_ACTIONS {
        assert!(present.contains(expected), "missing {expected}");
    }
    assert!(decision.actions.contains(&DirectorAction::SetWorldFlag {
        action_id: "a3".into(),
        flag: "disclosed_to:hank_schrader".into(),
    }));
    assert!(decision.actions.iter().any(|a| matches!(
        a,
        DirectorAction::InvalidateMission { mission_id, .. } if mission_id == "protect_walters_cover"
    )));

    let again = DirectorEngine::deterministic().decide(&ctx).await.unwrap();
    assert_eq!(
        again.actions, decision.actions,
        "same context, same actions"
    );
}

#[tokio::test]
async fn tampered_decisions_never_become_a_decision() {
    let ctx = hank_context();
    let cases: Vec<(&str, String, IssueCode)> = vec![
        (
            "a character the WorldBible does not contain",
            tampered(|v| v["actions"][3]["npc_id"] = json!("gus_fring")),
            IssueCode::UnknownReference,
        ),
        (
            "a location the WorldBible does not contain",
            tampered(|v| v["actions"][5]["location_id"] = json!("los_pollos_hermanos")),
            IssueCode::UnknownReference,
        ),
        (
            "an action outside the allowlist",
            tampered(|v| {
                v["actions"][0] =
                    json!({"type": "execute_script", "action_id": "a1", "script": "x"})
            }),
            IssueCode::UnknownActionType,
        ),
        (
            "an extra untyped field",
            tampered(|v| v["actions"][2]["console_command"] = json!("god")),
            IssueCode::Malformed,
        ),
        (
            "a flag owned by the gameplay rules",
            tampered(|v| v["actions"][2]["flag"] = json!("interacted:burner_phone")),
            IssueCode::InvalidValue,
        ),
        (
            "a replacement objective under the mission it just invalidated",
            tampered(|v| v["actions"][6]["mission_id"] = json!("protect_walters_cover")),
            IssueCode::InvalidCombination,
        ),
        (
            "more actions than allowed",
            tampered(|v| {
                let extra: Vec<Value> = (0..2)
                    .map(|i| json!({"type": "set_world_flag", "action_id": format!("x{i}"), "flag": format!("f{i}")}))
                    .collect();
                v["actions"].as_array_mut().unwrap().extend(extra);
            }),
            IssueCode::TooMany,
        ),
    ];

    for (what, raw, code) in cases {
        let issues = parse_proposal(&ctx, &raw).expect_err(what);
        assert!(issues.iter().any(|i| i.code == code), "{what}: {issues:#?}");

        // Through the engine: rejected on both attempts, nothing accepted.
        let provider = Arc::new(ScriptedProvider::texts([raw.clone(), raw]));
        let error = DirectorEngine::new(provider.clone())
            .decide(&ctx)
            .await
            .expect_err(what);
        assert!(
            matches!(error, DirectorError::InvalidDecision { attempts: 2, .. }),
            "{what}"
        );
        assert_eq!(provider.call_count(), 2, "{what}");
    }
}

#[tokio::test]
async fn a_decision_is_revalidated_against_newer_state_before_it_is_applied() {
    let ctx = hank_context();
    let provider = Arc::new(ScriptedProvider::texts([DECISION]));
    let decision = DirectorEngine::new(provider).decide(&ctx).await.unwrap();
    assert_eq!(validate_actions(&ctx, &decision.actions), Ok(()));

    // The world moved on while the model was thinking: the mission was
    // already invalidated and the objective failed by something else.
    let mut newer = ctx.clone();
    newer.event_count += 3;
    let narrative = newer.narrative.as_mut().unwrap();
    narrative.missions[0].status = MissionStatus::Invalidated;
    narrative.objectives[0].status = ObjectiveStatus::Failed;

    assert!(
        decision.metadata.based_on_event_count < newer.event_count,
        "stale"
    );
    let issues = validate_actions(&newer, &decision.actions).unwrap_err();
    let paths: Vec<&str> = issues.iter().map(|i| i.path.as_str()).collect();
    assert_eq!(paths, ["actions[0].objective_id", "actions[1].mission_id"]);
    assert!(
        issues
            .iter()
            .all(|i| i.code == IssueCode::InvalidCombination)
    );
}
