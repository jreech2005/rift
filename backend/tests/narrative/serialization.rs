//! Serialization: everything round-trips through JSON, and the JSON shape is
//! the documented one.

use rift_backend::narrative::{
    Condition, MissionStatus, NarrativeEngine, NarrativeEvent, NarrativePlan, NarrativeState,
    NarrativeTransition, Necessity, ObjectiveStatus, ReplanRequest, WorldFacts,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::fixtures::{LAW, LEDGER, apply, died, evidence, experiment, set_flag, start};

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) {
    let text = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, value);
    // Serializing is itself deterministic.
    assert_eq!(serde_json::to_string(&back).unwrap(), text);
}

#[test]
fn plan_world_and_state_round_trip() {
    for (plan, world) in [experiment(), evidence()] {
        round_trip(&plan);
        round_trip(&world);
        round_trip(&start((plan, world)));
    }
}

#[test]
fn state_round_trips_mid_story_and_continues_identically() {
    let state = apply(&start(experiment()), set_flag("met_maya")).state;
    round_trip(&state);

    // A state restored from JSON behaves exactly like the original.
    let restored: NarrativeState =
        serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
    assert_eq!(apply(&restored, died("maya")), apply(&state, died("maya")));
}

#[test]
fn transition_and_replan_request_round_trip() {
    let maya = apply(&start(experiment()), died("maya"));
    let exposed = apply(
        &start(evidence()),
        NarrativeEvent::FactRevealed {
            fact_id: LEDGER.into(),
            to: LAW.into(),
        },
    );
    for transition in [maya, exposed] {
        round_trip::<NarrativeTransition>(&transition);
        round_trip::<ReplanRequest>(transition.replan.as_ref().unwrap());
    }
}

#[test]
fn every_event_round_trips() {
    let events = [
        set_flag("f"),
        died("maya"),
        NarrativeEvent::CharacterAvailabilityChanged {
            character_id: "maya".into(),
            available: false,
        },
        NarrativeEvent::CharacterMoved {
            character_id: "maya".into(),
            location_id: "town".into(),
        },
        NarrativeEvent::PlayerMoved {
            location_id: "town".into(),
        },
        NarrativeEvent::LocationLost {
            location_id: "town".into(),
        },
        NarrativeEvent::ObjectDestroyed {
            object_id: "reactor_console".into(),
        },
        NarrativeEvent::FactRevealed {
            fact_id: LEDGER.into(),
            to: LAW.into(),
        },
        NarrativeEvent::RelationshipChanged {
            from: "maya".into(),
            to: "player".into(),
            trust: 10,
            fear: 0,
            affinity: -5,
        },
        NarrativeEvent::TruthEnded {
            truth_id: "experiment_running".into(),
        },
        NarrativeEvent::TruthEstablished {
            truth_id: "breach".into(),
            statement: "The lab is breached.".into(),
        },
        NarrativeEvent::ObjectiveCompleted {
            objective_id: "meet_maya".into(),
        },
        NarrativeEvent::ObjectiveFailed {
            objective_id: "meet_maya".into(),
        },
        NarrativeEvent::CheckpointReached {
            checkpoint_id: "voss_escapes".into(),
        },
        NarrativeEvent::CheckpointSkipped {
            checkpoint_id: "voss_escapes".into(),
        },
    ];
    for event in &events {
        round_trip(event);
    }
    assert_eq!(
        serde_json::to_value(&events[1]).unwrap(),
        json!({ "type": "character_died", "character_id": "maya" })
    );
}

#[test]
fn replan_request_has_the_documented_json_shape() {
    let t = apply(
        &apply(&start(experiment()), set_flag("met_maya")).state,
        died("maya"),
    );
    let value = serde_json::to_value(t.replan.unwrap()).unwrap();

    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["revision"], 2);
    assert_eq!(value["reason"], "mission_invalidated");
    assert_eq!(
        value["world_changes"]["trigger"],
        json!({ "type": "character_died", "character_id": "maya" })
    );
    assert_eq!(
        value["invalidated_missions"][0],
        json!({
            "mission_id": "stop_experiment",
            "from": "active",
            "to": "invalidated",
            "cause": { "type": "objective_invalidated", "objective_id": "maya_bypasses_door" },
        })
    );
    assert_eq!(
        value["invalidated_objectives"][0],
        json!({
            "mission_id": "stop_experiment",
            "objective_id": "maya_bypasses_door",
            "from": "active",
            "to": "invalidated",
            "cause": {
                "type": "prerequisite_broken",
                "lost": [{ "type": "character_dead", "character_id": "maya" }],
            },
        })
    );
    assert_eq!(
        value["lost_prerequisites"][0],
        json!({ "type": "character_dead", "character_id": "maya" })
    );
    assert_eq!(
        value["beats"][0],
        json!({
            "checkpoint_id": "maya_sacrifice",
            "policy": "replace",
            "gravity": 6,
            "ready": false,
            "lost": [{ "type": "character_dead", "character_id": "maya" }],
        })
    );
    assert_eq!(
        value["remaining_context"]["dead_characters"],
        json!(["maya"])
    );
    assert_eq!(
        value["remaining_context"]["holding_truths"],
        json!(["experiment_running"])
    );
}

fn minimal_plan() -> Value {
    json!({
        "schema_version": 1,
        "plan_id": "demo.opening",
        "universe_id": "demo",
        "missions": [{
            "mission_id": "m1",
            "title": "Stop the experiment",
            "canon_relation": "generated",
            "importance": "major",
            "objectives": [{
                "objective_id": "o1",
                "description": "Find Maya",
                "prerequisites": [
                    { "condition": { "type": "character_alive", "character_id": "maya" } },
                    {
                        "condition": { "type": "player_at", "location_id": "lab" },
                        "necessity": "flexible",
                    },
                ],
                "completes_when": { "type": "flag_is", "flag": "met_maya", "value": true },
            }],
        }],
    })
}

#[test]
fn hand_written_plan_json_deserializes_with_defaults_and_runs() {
    let plan: NarrativePlan = serde_json::from_value(minimal_plan()).unwrap();
    let mission = &plan.missions[0];
    let objective = &mission.objectives[0];
    assert_eq!(mission.status, MissionStatus::Inactive);
    assert_eq!(objective.status, ObjectiveStatus::Pending);
    assert!(!objective.optional);
    assert_eq!(objective.prerequisites[0].necessity, Necessity::Essential);
    assert_eq!(objective.prerequisites[1].necessity, Necessity::Flexible);
    assert_eq!(
        objective.completes_when,
        Some(Condition::FlagIs {
            flag: "met_maya".into(),
            value: true
        })
    );
    assert!(plan.checkpoints.is_empty());

    let world: WorldFacts = serde_json::from_value(json!({
        "characters": { "maya": {} },
        "locations": ["lab"],
        "player_location": "lab",
    }))
    .unwrap();
    let state = NarrativeEngine::start(plan, world).unwrap().state;
    assert_eq!(state.active_objectives()[0].1.objective_id, "o1");
}

#[test]
fn unknown_fields_and_unknown_kinds_are_rejected() {
    let mut plan = minimal_plan();
    plan["missions"][0]["surprise"] = json!(true);
    assert!(serde_json::from_value::<NarrativePlan>(plan).is_err());

    let mut plan = minimal_plan();
    plan["missions"][0]["objectives"][0]["status"] = json!("almost_done");
    assert!(serde_json::from_value::<NarrativePlan>(plan).is_err());

    // Conditions are a closed set of data shapes: there is no way to smuggle
    // in code or an unknown kind of check.
    for condition in [
        json!({ "type": "run_script", "source": "maya.alive = true" }),
        json!({ "type": "character_alive", "character_id": "maya", "or_else": "revive" }),
        json!({ "type": "character_alive" }),
        json!("character_alive"),
    ] {
        assert!(
            serde_json::from_value::<Condition>(condition.clone()).is_err(),
            "{condition}"
        );
    }

    assert!(
        serde_json::from_value::<NarrativeEvent>(json!({
            "type": "character_revived",
            "character_id": "maya",
        }))
        .is_err()
    );
}
