//! Mission and objective lifecycle.

use rift_backend::narrative::{
    ACTUAL_TIMELINE_CAP, Cause, Condition, Effect, LostDependency, Mission, MissionChange,
    MissionStatus, NarrativeEngine, NarrativeError, NarrativeEvent, NarrativePlan, Objective,
    ObjectiveStatus, ReplanReason, TimelineEntry, WorldFacts,
};

use crate::fixtures::{
    apply, died, essential, flag, mission_status, objective_done, objective_status, plan_with,
    player_moved, set_flag, start,
};

/// A briefing unlocks a three-objective job; one objective is optional.
fn vault() -> (NarrativePlan, WorldFacts) {
    let mut world = WorldFacts::default();
    world.add_location("lobby");
    world.add_location("vault");
    world.add_character("guard", Some("lobby"));
    world.add_object("key");
    world.player_location = Some("lobby".into());

    let get_key = Objective {
        completes_when: Some(flag("has_key")),
        ..Objective::new("get_key", "Get the key")
    };
    let open_door = Objective {
        prerequisites: vec![
            essential(objective_done("get_key")),
            essential(Condition::PlayerAt {
                location_id: "vault".into(),
            }),
        ],
        completes_when: Some(flag("door_open")),
        fails_when: Some(flag("alarm_raised")),
        success_effects: vec![Effect::SetFlag {
            flag: "door_was_opened".into(),
            value: true,
        }],
        ..Objective::new("open_door", "Open the vault door")
    };
    let bribe = Objective {
        optional: true,
        prerequisites: vec![essential(Condition::CharacterAvailable {
            character_id: "guard".into(),
        })],
        ..Objective::new("bribe_guard", "Bribe the guard")
    };
    let mission = Mission {
        prerequisites: vec![
            essential(flag("briefed")),
            essential(Condition::ObjectIntact {
                object_id: "key".into(),
            }),
        ],
        success_effects: vec![Effect::SetFlag {
            flag: "vault_open".into(),
            value: true,
        }],
        failure_effects: vec![Effect::SetFlag {
            flag: "locked_down".into(),
            value: true,
        }],
        ..Mission::new(
            "open_vault",
            "Open the vault",
            vec![get_key, open_door, bribe],
        )
    };
    (plan_with(vec![mission], Vec::new()), world)
}

fn briefed() -> rift_backend::narrative::NarrativeState {
    apply(&start(vault()), set_flag("briefed")).state
}

#[test]
fn mission_stays_inactive_until_its_prerequisites_hold() {
    let (plan, world) = vault();
    let started = NarrativeEngine::start(plan, world).unwrap();
    assert!(started.mission_changes.is_empty());
    assert!(started.replan.is_none());
    assert_eq!(
        mission_status(&started.state, "open_vault"),
        MissionStatus::Inactive
    );
    assert_eq!(
        objective_status(&started.state, "get_key"),
        ObjectiveStatus::Pending
    );
    assert!(started.state.active_objectives().is_empty());
}

#[test]
fn mission_activates_when_its_prerequisites_hold() {
    let t = apply(&start(vault()), set_flag("briefed"));
    assert_eq!(
        t.mission_changes,
        vec![MissionChange {
            mission_id: "open_vault".into(),
            from: MissionStatus::Inactive,
            to: MissionStatus::Active,
            cause: None,
        }]
    );
    assert_eq!(
        objective_status(&t.state, "get_key"),
        ObjectiveStatus::Active
    );
    assert_eq!(
        objective_status(&t.state, "open_door"),
        ObjectiveStatus::Pending
    );
    assert_eq!(
        objective_status(&t.state, "bribe_guard"),
        ObjectiveStatus::Active
    );
    assert!(t.replan.is_none());

    // "What is currently supposed to happen?"
    let now: Vec<&str> = t
        .state
        .active_objectives()
        .iter()
        .map(|(_, o)| o.objective_id.as_str())
        .collect();
    assert_eq!(now, ["get_key", "bribe_guard"]);
}

#[test]
fn mission_without_prerequisites_activates_at_start() {
    let (mut plan, world) = vault();
    plan.missions[0].prerequisites.clear();
    let started = NarrativeEngine::start(plan, world).unwrap();
    assert_eq!(
        mission_status(&started.state, "open_vault"),
        MissionStatus::Active
    );
    assert_eq!(started.mission_changes.len(), 1);
    assert_eq!(started.state.revision, 0);
}

#[test]
fn objective_completes_when_its_condition_holds() {
    let state = apply(&briefed(), set_flag("has_key")).state;
    assert_eq!(
        objective_status(&state, "get_key"),
        ObjectiveStatus::Completed
    );
    // The next objective still waits for the player to be at the vault.
    assert_eq!(
        objective_status(&state, "open_door"),
        ObjectiveStatus::Pending
    );

    let state = apply(&state, player_moved("vault")).state;
    assert_eq!(
        objective_status(&state, "open_door"),
        ObjectiveStatus::Active
    );
}

#[test]
fn completion_condition_does_not_complete_a_pending_objective() {
    // The door flag is set before the objective is active: nothing completes.
    let state = apply(&briefed(), set_flag("door_open")).state;
    assert_eq!(
        objective_status(&state, "open_door"),
        ObjectiveStatus::Pending
    );
    assert_eq!(mission_status(&state, "open_vault"), MissionStatus::Active);
}

#[test]
fn objective_completion_can_be_reported() {
    let state = briefed();
    let report = |id: &str| NarrativeEvent::ObjectiveCompleted {
        objective_id: id.into(),
    };

    let done = apply(&state, report("bribe_guard")).state;
    assert_eq!(
        objective_status(&done, "bribe_guard"),
        ObjectiveStatus::Completed
    );

    // Not active yet, already finished, or in a mission that has not started.
    for (state, id) in [
        (&state, "open_door"),
        (&done, "bribe_guard"),
        (&start(vault()), "get_key"),
    ] {
        assert!(matches!(
            NarrativeEngine::apply_event(state, &report(id)),
            Err(NarrativeError::InvalidTransition(_))
        ));
    }
}

#[test]
fn objective_fails_when_its_fail_condition_holds() {
    let t = apply(&briefed(), set_flag("alarm_raised"));

    // It fails even though it was still pending.
    let failed = t
        .objective_changes
        .iter()
        .find(|c| c.objective_id == "open_door")
        .unwrap();
    assert_eq!(failed.from, ObjectiveStatus::Pending);
    assert_eq!(failed.to, ObjectiveStatus::Failed);
    assert_eq!(
        failed.cause,
        Some(Cause::FailConditionMet {
            condition: flag("alarm_raised")
        })
    );
}

#[test]
fn mission_fails_when_a_required_objective_fails() {
    let t = apply(&briefed(), set_flag("alarm_raised"));

    assert_eq!(
        t.mission_changes,
        vec![MissionChange {
            mission_id: "open_vault".into(),
            from: MissionStatus::Active,
            to: MissionStatus::Failed,
            cause: Some(Cause::ObjectiveFailed {
                objective_id: "open_door".into()
            }),
        }]
    );
    // Open objectives are voided, and the failure effects fire.
    for id in ["get_key", "bribe_guard"] {
        assert_eq!(objective_status(&t.state, id), ObjectiveStatus::Invalidated);
    }
    assert!(t.state.world.flag("locked_down"));
    assert!(!t.state.world.flag("vault_open"));
    assert_eq!(
        t.effects,
        vec![Effect::SetFlag {
            flag: "locked_down".into(),
            value: true
        }]
    );
    assert_eq!(t.replan.unwrap().reason, ReplanReason::MissionFailed);
}

#[test]
fn objective_failure_can_be_reported() {
    let t = apply(
        &briefed(),
        NarrativeEvent::ObjectiveFailed {
            objective_id: "get_key".into(),
        },
    );
    assert_eq!(t.objective_changes[0].cause, Some(Cause::Reported));
    assert_eq!(
        objective_status(&t.state, "get_key"),
        ObjectiveStatus::Failed
    );
    assert_eq!(
        mission_status(&t.state, "open_vault"),
        MissionStatus::Failed
    );
}

#[test]
fn mission_completes_when_every_required_objective_completes() {
    let mut state = briefed();
    for event in [set_flag("has_key"), player_moved("vault")] {
        state = apply(&state, event).state;
    }
    let t = apply(&state, set_flag("door_open"));

    assert_eq!(
        mission_status(&t.state, "open_vault"),
        MissionStatus::Completed
    );
    assert_eq!(
        t.effects,
        vec![
            Effect::SetFlag {
                flag: "door_was_opened".into(),
                value: true
            },
            Effect::SetFlag {
                flag: "vault_open".into(),
                value: true
            },
        ]
    );
    assert!(t.state.world.flag("vault_open"));
    assert!(!t.state.world.flag("locked_down"));

    // The optional objective is voided, which is not a reason to replan.
    assert_eq!(
        objective_status(&t.state, "bribe_guard"),
        ObjectiveStatus::Invalidated
    );
    assert!(t.replan.is_none());
}

#[test]
fn optional_objective_never_decides_the_mission() {
    let t = apply(&briefed(), died("guard"));
    assert_eq!(
        objective_status(&t.state, "bribe_guard"),
        ObjectiveStatus::Invalidated
    );
    assert_eq!(
        mission_status(&t.state, "open_vault"),
        MissionStatus::Active
    );

    // The Director is still told, at lower severity.
    let replan = t.replan.unwrap();
    assert_eq!(replan.reason, ReplanReason::ObjectiveInvalidated);
    assert!(replan.invalidated_missions.is_empty());
    assert_eq!(
        replan.lost_prerequisites,
        vec![LostDependency::CharacterDead {
            character_id: "guard".into()
        }]
    );
}

#[test]
fn mission_is_invalidated_when_an_essential_prerequisite_breaks() {
    let t = apply(
        &briefed(),
        NarrativeEvent::ObjectDestroyed {
            object_id: "key".into(),
        },
    );
    let lost = vec![LostDependency::ObjectDestroyed {
        object_id: "key".into(),
    }];
    assert_eq!(
        t.mission_changes,
        vec![MissionChange {
            mission_id: "open_vault".into(),
            from: MissionStatus::Active,
            to: MissionStatus::Invalidated,
            cause: Some(Cause::PrerequisiteBroken { lost: lost.clone() }),
        }]
    );
    for id in ["get_key", "open_door", "bribe_guard"] {
        assert_eq!(objective_status(&t.state, id), ObjectiveStatus::Invalidated);
    }
    // Invalidation is not failure: no failure effects fire.
    assert!(t.effects.is_empty());
    assert!(!t.state.world.flag("locked_down"));

    let replan = t.replan.unwrap();
    assert_eq!(replan.reason, ReplanReason::MissionInvalidated);
    assert_eq!(replan.lost_prerequisites, lost);
}

#[test]
fn inactive_mission_is_invalidated_when_it_can_never_start() {
    let t = apply(
        &start(vault()),
        NarrativeEvent::ObjectDestroyed {
            object_id: "key".into(),
        },
    );
    assert_eq!(t.mission_changes[0].from, MissionStatus::Inactive);
    assert_eq!(
        mission_status(&t.state, "open_vault"),
        MissionStatus::Invalidated
    );

    // A terminal mission stays terminal.
    let later = apply(&t.state, set_flag("briefed")).state;
    assert_eq!(
        mission_status(&later, "open_vault"),
        MissionStatus::Invalidated
    );
}

#[test]
fn objective_is_invalidated_when_it_can_never_complete() {
    let mut world = WorldFacts::default();
    world.add_location("lobby");
    world.add_location("vault");
    world.add_character("guard", Some("lobby"));
    let escort = Objective {
        completes_when: Some(Condition::CharacterAt {
            character_id: "guard".into(),
            location_id: "vault".into(),
        }),
        ..Objective::new("escort_guard", "Bring the guard to the vault")
    };
    let plan = plan_with(
        vec![Mission::new("escort", "Escort", vec![escort])],
        Vec::new(),
    );

    let t = apply(&start((plan, world)), died("guard"));
    assert_eq!(
        t.objective_changes[0].cause,
        Some(Cause::CompletionImpossible {
            lost: vec![LostDependency::CharacterDead {
                character_id: "guard".into()
            }]
        })
    );
    assert_eq!(
        mission_status(&t.state, "escort"),
        MissionStatus::Invalidated
    );
}

#[test]
fn effects_cascade_into_other_missions_in_the_same_step() {
    let (mut plan, world) = vault();
    let leave = Objective {
        completes_when: Some(Condition::PlayerAt {
            location_id: "lobby".into(),
        }),
        ..Objective::new("leave", "Walk out")
    };
    plan.missions.push(Mission {
        prerequisites: vec![essential(flag("vault_open"))],
        ..Mission::new("escape", "Get out", vec![leave])
    });

    let mut state = start((plan, world));
    for event in [
        set_flag("briefed"),
        set_flag("has_key"),
        player_moved("vault"),
    ] {
        state = apply(&state, event).state;
    }
    assert_eq!(mission_status(&state, "escape"), MissionStatus::Inactive);

    // Completing the first mission sets the flag the second one waits for.
    let t = apply(&state, set_flag("door_open"));
    assert_eq!(mission_status(&t.state, "escape"), MissionStatus::Active);
    assert_eq!(objective_status(&t.state, "leave"), ObjectiveStatus::Active);
    assert_eq!(t.mission_changes.len(), 2);
}

#[test]
fn apply_event_is_deterministic_and_leaves_its_input_alone() {
    let state = briefed();
    let before = state.clone();

    let first = apply(&state, set_flag("alarm_raised"));
    let second = apply(&state, set_flag("alarm_raised"));
    assert_eq!(first, second);
    assert_eq!(state, before);
    assert_ne!(first.state, before);
}

#[test]
fn rejected_event_changes_nothing() {
    let state = briefed();
    let before = state.clone();
    let result = NarrativeEngine::apply_event(
        &state,
        &NarrativeEvent::CharacterMoved {
            character_id: "nobody".into(),
            location_id: "vault".into(),
        },
    );
    assert!(result.is_err());
    assert_eq!(state, before);
}

#[test]
fn revision_and_actual_timeline_record_what_happened() {
    let started = start(vault());
    assert_eq!(started.revision, 0);
    assert!(started.actual_timeline.is_empty());

    let state = apply(&started, set_flag("briefed")).state;
    assert_eq!(state.revision, 1);
    assert_eq!(
        state.actual_timeline,
        [TimelineEntry {
            revision: 1,
            event: set_flag("briefed")
        }]
    );

    // The log is bounded; the oldest entries fall off.
    let mut state = state;
    for n in 0..ACTUAL_TIMELINE_CAP + 5 {
        state = apply(&state, set_flag(&format!("noise_{n}"))).state;
    }
    assert_eq!(state.actual_timeline.len(), ACTUAL_TIMELINE_CAP);
    assert_eq!(state.revision, ACTUAL_TIMELINE_CAP as u64 + 6);
    assert_eq!(
        state.actual_timeline.back().unwrap().revision,
        state.revision
    );
    assert_eq!(state.actual_timeline.front().unwrap().revision, 7);
}

#[test]
fn a_reported_invalidation_ends_an_active_mission_without_effects() {
    let state = apply(&start(vault()), set_flag("briefed")).state;
    assert_eq!(mission_status(&state, "open_vault"), MissionStatus::Active);

    let event = NarrativeEvent::MissionInvalidated {
        mission_id: "open_vault".into(),
    };
    let t = apply(&state, event.clone());
    assert_eq!(
        mission_status(&t.state, "open_vault"),
        MissionStatus::Invalidated
    );
    assert_eq!(t.mission_changes[0].cause, Some(Cause::Reported));
    // Its open objectives go with it, and neither outcome's effects fire.
    assert_eq!(
        objective_status(&t.state, "get_key"),
        ObjectiveStatus::Invalidated
    );
    assert!(t.effects.is_empty());
    assert_eq!(
        t.replan
            .expect("an invalidated mission asks for a replan")
            .reason,
        ReplanReason::MissionInvalidated
    );

    // Terminal: it cannot be invalidated twice, and unknown missions are refused.
    assert!(matches!(
        NarrativeEngine::apply_event(&t.state, &event),
        Err(NarrativeError::InvalidTransition(_))
    ));
    let unknown = NarrativeEvent::MissionInvalidated {
        mission_id: "no_such_mission".into(),
    };
    assert!(matches!(
        NarrativeEngine::apply_event(&state, &unknown),
        Err(NarrativeError::UnknownId { .. })
    ));
    // An inactive mission has nothing to invalidate yet.
    assert!(NarrativeEngine::apply_event(&start(vault()), &event).is_err());
}
