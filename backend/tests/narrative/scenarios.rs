//! The two demo-critical divergences, end to end.

use rift_backend::narrative::{
    CanonPolicy, Cause, CharacterStatus, Condition, Effect, IdKind, LostDependency, Mission,
    MissionStatus, NarrativeEngine, NarrativeError, NarrativeEvent, NarrativeState, Objective,
    ObjectiveChange, ObjectiveStatus, RelationshipAxis, ReplanReason, ReplanRequest,
};

use crate::fixtures::{
    LAW, LEDGER, alive, apply, died, essential, evidence, experiment, mission_status,
    objective_status, policy, set_flag, start, truth,
};

fn maya_dead() -> LostDependency {
    LostDependency::CharacterDead {
        character_id: "maya".into(),
    }
}

fn change<'a>(replan: &'a ReplanRequest, objective_id: &str) -> &'a ObjectiveChange {
    replan
        .invalidated_objectives
        .iter()
        .find(|c| c.objective_id == objective_id)
        .expect("objective is in the replan request")
}

// ---------------------------------------------------------------------------
// The player causes companion Maya to die
// ---------------------------------------------------------------------------

/// The mission is under way: Maya has been met, and the door is next.
fn with_maya() -> NarrativeState {
    let state = start(experiment());
    assert_eq!(
        mission_status(&state, "stop_experiment"),
        MissionStatus::Active
    );
    let state = apply(&state, set_flag("met_maya")).state;
    assert_eq!(
        objective_status(&state, "maya_bypasses_door"),
        ObjectiveStatus::Active
    );
    state
}

#[test]
fn maya_dies_objectives_that_need_her_become_invalid() {
    let t = apply(&with_maya(), died("maya"));

    for id in ["maya_bypasses_door", "learn_maya_secret"] {
        assert_eq!(objective_status(&t.state, id), ObjectiveStatus::Invalidated);
    }
    // What was already done stays done.
    assert_eq!(
        objective_status(&t.state, "meet_maya"),
        ObjectiveStatus::Completed
    );
    // The mission cannot be finished without her.
    assert_eq!(
        mission_status(&t.state, "stop_experiment"),
        MissionStatus::Invalidated
    );
    // Invalidated, not completed and not failed: no outcome effects fire.
    assert!(t.effects.is_empty());
    assert!(!t.state.world.flag("experiment_stopped"));
}

#[test]
fn maya_dies_replan_request_names_what_was_lost() {
    let t = apply(&with_maya(), died("maya"));
    let replan = t.replan.expect("invalidation must request a replan");

    assert_eq!(replan.reason, ReplanReason::MissionInvalidated);
    assert_eq!(replan.revision, t.state.revision);
    assert_eq!(replan.world_changes.trigger, Some(died("maya")));

    let mission = &replan.invalidated_missions[0];
    assert_eq!(mission.mission_id, "stop_experiment");
    assert_eq!(mission.to, MissionStatus::Invalidated);
    assert_eq!(
        mission.cause,
        Some(Cause::ObjectiveInvalidated {
            objective_id: "maya_bypasses_door".into()
        })
    );

    // Each objective says why it ended: she is the lost dependency...
    for id in ["maya_bypasses_door", "learn_maya_secret"] {
        assert_eq!(
            change(&replan, id).cause,
            Some(Cause::PrerequisiteBroken {
                lost: vec![maya_dead()]
            })
        );
    }
    // ...and the last step is lost only because the step before it is.
    assert_eq!(
        change(&replan, "shut_down_reactor").cause,
        Some(Cause::PrerequisiteBroken {
            lost: vec![LostDependency::ObjectiveUnreachable {
                objective_id: "maya_bypasses_door".into(),
                status: ObjectiveStatus::Invalidated,
            }]
        })
    );
    assert_eq!(replan.lost_prerequisites[0], maya_dead());
}

#[test]
fn maya_dies_and_remains_dead() {
    let dead = apply(&with_maya(), died("maya")).state;
    let status = |state: &NarrativeState| state.world.character_status("maya");
    assert_eq!(status(&dead), Some(CharacterStatus::Dead));

    // Nothing brings her back, moves her, or tells her anything.
    for event in [
        NarrativeEvent::CharacterAvailabilityChanged {
            character_id: "maya".into(),
            available: true,
        },
        NarrativeEvent::CharacterMoved {
            character_id: "maya".into(),
            location_id: "town".into(),
        },
        NarrativeEvent::FactRevealed {
            fact_id: "override_code".into(),
            to: "maya".into(),
        },
    ] {
        assert!(matches!(
            NarrativeEngine::apply_event(&dead, &event),
            Err(NarrativeError::InvalidTransition(_))
        ));
    }

    // The world moves on and she is still dead. Reporting it twice is harmless.
    let mut state = dead;
    for event in [
        set_flag("lab_door_open"),
        set_flag("reactor_offline"),
        died("maya"),
        NarrativeEvent::PlayerMoved {
            location_id: "town".into(),
        },
    ] {
        state = apply(&state, event).state;
        assert_eq!(status(&state), Some(CharacterStatus::Dead));
    }
    // Flags that would have completed her objectives change nothing now.
    assert_eq!(
        mission_status(&state, "stop_experiment"),
        MissionStatus::Invalidated
    );
    assert!(!state.world.flag("experiment_stopped"));
}

#[test]
fn maya_dies_and_nothing_is_manufactured_to_replace_her() {
    let before = with_maya();
    let t = apply(&before, died("maya"));

    // Same cast, same missions, same objectives: no "Maya 2", no stand-in
    // objective, no substitute beat.
    let cast = |state: &NarrativeState| -> Vec<String> {
        state.world.characters.keys().cloned().collect()
    };
    assert_eq!(cast(&t.state), cast(&before));
    assert_eq!(cast(&t.state), ["dr_voss", "maya"]);
    assert_eq!(t.state.plan.missions.len(), 1);
    assert_eq!(t.state.plan.missions[0].objectives.len(), 4);
    assert_eq!(t.state.plan.checkpoints.len(), 3);
    assert!(t.state.active_objectives().is_empty());

    // The Director is told who is left and who is not.
    let context = t.replan.unwrap().remaining_context;
    assert_eq!(context.available_characters, ["dr_voss"]);
    assert_eq!(context.dead_characters, ["maya"]);
    assert!(context.active_missions.is_empty());
}

#[test]
fn maya_dies_but_the_experiment_is_still_running() {
    let t = apply(&with_maya(), died("maya"));

    // The world truth outlives the mission built on it.
    assert_eq!(t.state.world.truth_holds("experiment_running"), Some(true));
    let replan = t.replan.unwrap();
    assert_eq!(
        replan.remaining_context.holding_truths,
        ["experiment_running"]
    );

    // Her canon beat cannot be preserved; it matters, so it must be replaced.
    assert_eq!(policy(&t.state, "maya_sacrifice"), CanonPolicy::Replace);
    // Beats that never needed her are untouched.
    assert_eq!(policy(&t.state, "voss_escapes"), CanonPolicy::Preserve);
    assert_eq!(
        policy(&t.state, "containment_breach"),
        CanonPolicy::Preserve
    );

    let beat = |id: &str| replan.beats.iter().find(|b| b.checkpoint_id == id).unwrap();
    assert_eq!(beat("maya_sacrifice").lost, vec![maya_dead()]);
    assert!(beat("containment_breach").ready);
}

#[test]
fn after_maya_a_director_may_adopt_a_different_objective_but_not_her() {
    let dead = apply(&with_maya(), died("maya")).state;

    // A proposal that still depends on Maya is rejected outright...
    let needs_maya = Mission {
        prerequisites: vec![essential(alive("maya"))],
        ..Mission::new(
            "maya_returns",
            "Maya returns",
            vec![Objective::new("regroup", "Regroup with Maya")],
        )
    };
    assert_eq!(
        NarrativeEngine::adopt_mission(&dead, needs_maya),
        Err(NarrativeError::Impossible {
            kind: IdKind::Mission,
            id: "maya_returns".into(),
            lost: vec![maya_dead()],
        })
    );
    // ...including when the dependency hides in an objective.
    let hides_maya = Mission::new(
        "second_try",
        "Second try",
        vec![Objective {
            prerequisites: vec![essential(alive("maya"))],
            ..Objective::new("ask_maya", "Ask Maya for the code")
        }],
    );
    assert!(matches!(
        NarrativeEngine::adopt_mission(&dead, hides_maya),
        Err(NarrativeError::Impossible {
            kind: IdKind::Objective,
            ..
        })
    ));

    // A different story, built on what is still true, is accepted.
    let survive = Mission {
        prerequisites: vec![essential(truth("experiment_running"))],
        ..Mission::new(
            "survive_the_breach",
            "Survive the catastrophe",
            vec![Objective {
                completes_when: Some(Condition::PlayerAt {
                    location_id: "town".into(),
                }),
                ..Objective::new("reach_town", "Get clear of the lab")
            }],
        )
    };
    let t = NarrativeEngine::adopt_mission(&dead, survive).unwrap();
    assert_eq!(
        mission_status(&t.state, "survive_the_breach"),
        MissionStatus::Active
    );
    assert_eq!(
        objective_status(&t.state, "reach_town"),
        ObjectiveStatus::Active
    );
    assert_eq!(t.state.revision, dead.revision + 1);
    assert!(t.replan.is_none());
    // The old mission stays as it ended.
    assert_eq!(
        mission_status(&t.state, "stop_experiment"),
        MissionStatus::Invalidated
    );

    let done = apply(
        &t.state,
        NarrativeEvent::PlayerMoved {
            location_id: "town".into(),
        },
    )
    .state;
    assert_eq!(
        mission_status(&done, "survive_the_breach"),
        MissionStatus::Completed
    );
}

#[test]
fn with_maya_alive_the_mission_can_be_completed() {
    let mut state = with_maya();
    for flag in ["lab_door_open", "reactor_offline"] {
        state = apply(&state, set_flag(flag)).state;
    }
    assert_eq!(
        mission_status(&state, "stop_experiment"),
        MissionStatus::Completed
    );
    assert!(state.world.flag("experiment_stopped"));
    // Stopping the reactor is what ends the truth, and with it the breach.
    assert_eq!(state.world.truth_holds("experiment_running"), Some(false));
    assert_eq!(policy(&state, "containment_breach"), CanonPolicy::Delete);
}

// ---------------------------------------------------------------------------
// The player exposes the evidence instead of hiding it
// ---------------------------------------------------------------------------

fn expose() -> NarrativeEvent {
    NarrativeEvent::FactRevealed {
        fact_id: LEDGER.into(),
        to: LAW.into(),
    }
}

#[test]
fn exposing_the_evidence_fails_the_objective_to_hide_it() {
    let state = start(evidence());
    assert_eq!(
        objective_status(&state, "hide_ledger"),
        ObjectiveStatus::Active
    );

    let t = apply(&state, expose());
    assert_eq!(
        objective_status(&t.state, "hide_ledger"),
        ObjectiveStatus::Failed
    );
    assert_eq!(
        t.objective_changes[0].cause,
        Some(Cause::FailConditionMet {
            condition: Condition::FactKnown {
                fact_id: LEDGER.into(),
                by: LAW.into(),
            }
        })
    );
    // What depended on it is invalidated, and the mission fails.
    assert_eq!(
        objective_status(&t.state, "earn_walter_trust"),
        ObjectiveStatus::Invalidated
    );
    assert_eq!(
        mission_status(&t.state, "protect_the_books"),
        MissionStatus::Failed
    );
}

#[test]
fn exposing_the_evidence_updates_world_flags_and_truths() {
    let t = apply(&start(evidence()), expose());

    assert!(t.state.world.knows(LEDGER, LAW));
    assert!(t.state.world.flag("police_alerted"));
    assert_eq!(
        t.state.world.truth_holds("dea_watching_car_wash"),
        Some(true)
    );
    // Truths that were there before are still there.
    assert_eq!(t.state.world.truth_holds("walter_double_life"), Some(true));
    assert_eq!(t.state.world.relationship("walter", "player").trust, -40);

    assert_eq!(
        t.effects,
        vec![
            Effect::SetFlag {
                flag: "police_alerted".into(),
                value: true,
            },
            Effect::EstablishTruth {
                truth_id: "dea_watching_car_wash".into(),
                statement: "The DEA is watching the car wash.".into(),
            },
            Effect::AdjustRelationship {
                from: "walter".into(),
                to: "player".into(),
                axis: RelationshipAxis::Trust,
                delta: -40,
            },
        ]
    );
}

#[test]
fn exposing_the_evidence_requests_a_replan() {
    let t = apply(&start(evidence()), expose());
    let replan = t.replan.expect("failure must request a replan");

    assert_eq!(replan.reason, ReplanReason::MissionFailed);
    assert_eq!(replan.world_changes.trigger, Some(expose()));
    assert_eq!(replan.world_changes.effects, t.effects);
    assert_eq!(
        replan.invalidated_missions[0].cause,
        Some(Cause::ObjectiveFailed {
            objective_id: "hide_ledger".into()
        })
    );
    assert_eq!(change(&replan, "hide_ledger").to, ObjectiveStatus::Failed);
    assert_eq!(
        change(&replan, "earn_walter_trust").to,
        ObjectiveStatus::Invalidated
    );

    // The secrecy is the dependency that cannot be had back.
    assert!(
        replan
            .lost_prerequisites
            .contains(&LostDependency::FactExposed {
                fact_id: LEDGER.into(),
                known_by: LAW.into(),
            })
    );
    assert_eq!(
        replan.remaining_context.flags.get("police_alerted"),
        Some(&true)
    );
    assert_eq!(
        replan.remaining_context.holding_truths,
        ["dea_watching_car_wash", "walter_double_life"]
    );
    assert_eq!(
        replan.remaining_context.available_characters,
        ["hank", "walter"]
    );
}

#[test]
fn exposing_the_evidence_reevaluates_future_beats() {
    let before = start(evidence());
    // Before the divergence the quiet path is what is ready.
    let ready = |state: &NarrativeState| -> Vec<String> {
        state
            .ready_checkpoints()
            .into_iter()
            .map(|b| b.checkpoint_id)
            .collect()
    };
    assert_eq!(ready(&before), ["walter_confides", "quiet_sale"]);

    let t = apply(&before, expose());
    let state = &t.state;

    // Beats that needed the secret cannot be preserved.
    assert_eq!(policy(state, "walter_confides"), CanonPolicy::Replace);
    assert_eq!(policy(state, "quiet_sale"), CanonPolicy::Delete);
    // The beat behind a replaced beat goes with it.
    assert_eq!(policy(state, "family_dinner"), CanonPolicy::Delete);
    // Beats that needed the exposure are now the valid future.
    assert_eq!(policy(state, "hank_investigates"), CanonPolicy::Preserve);
    assert_eq!(policy(state, "walter_flees"), CanonPolicy::Preserve);
    assert_eq!(ready(state), ["hank_investigates"]);

    // Every beat is in the request, in plan order, with its new policy.
    let policies: Vec<(&str, CanonPolicy)> = t
        .replan
        .as_ref()
        .unwrap()
        .beats
        .iter()
        .map(|b| (b.checkpoint_id.as_str(), b.policy))
        .collect();
    assert_eq!(
        policies,
        [
            ("walter_confides", CanonPolicy::Replace),
            ("quiet_sale", CanonPolicy::Delete),
            ("hank_investigates", CanonPolicy::Preserve),
            ("walter_flees", CanonPolicy::Preserve),
            ("family_dinner", CanonPolicy::Delete),
        ]
    );

    // The story continues down the new path.
    let state = apply(
        state,
        NarrativeEvent::CheckpointReached {
            checkpoint_id: "hank_investigates".into(),
        },
    )
    .state;
    assert_eq!(ready(&state), ["walter_flees"]);
}

#[test]
fn hiding_the_evidence_keeps_the_canon_path() {
    let t = apply(&start(evidence()), set_flag("ledger_hidden"));
    assert_eq!(
        objective_status(&t.state, "hide_ledger"),
        ObjectiveStatus::Completed
    );
    assert_eq!(
        objective_status(&t.state, "earn_walter_trust"),
        ObjectiveStatus::Active
    );
    assert!(t.replan.is_none());
    assert!(t.state.divergences().is_empty());
    assert!(!t.state.world.flag("police_alerted"));
}
