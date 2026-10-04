//! Prerequisite evaluation: Holds / Unmet / Broken for every kind of condition.

use rift_backend::narrative::{
    CanonRelation, Condition, Importance, LostDependency, Mission, MissionStatus, NarrativeEvent,
    NarrativeState, Objective, ObjectiveStatus, Prerequisite, RELATIONSHIP_MAX, RelationshipAxis,
    Verdict, WorldFacts, assess, evaluate,
};

use crate::fixtures::{
    alive, apply, beat, died, essential, flag, flexible, mission_status, objective_done,
    objective_status, plan_with, player_moved, reached, set_flag, start, truth,
};

/// Two characters, two locations, one object, one truth, one two-step mission
/// and one beat.
fn sandbox() -> NarrativeState {
    let mut world = WorldFacts::default();
    world.add_location("a");
    world.add_location("b");
    world.add_character("ann", Some("a"));
    world.add_character("bob", Some("b"));
    world.add_object("gem");
    world.add_truth("t1", "Something is true.", CanonRelation::Canon);
    world.player_location = Some("a".into());

    let o1 = Objective {
        completes_when: Some(flag("f1")),
        ..Objective::new("o1", "first")
    };
    let o2 = Objective {
        prerequisites: vec![essential(objective_done("o1"))],
        completes_when: Some(flag("f2")),
        ..Objective::new("o2", "second")
    };
    let plan = plan_with(
        vec![Mission::new("m1", "Mission", vec![o1, o2])],
        vec![beat(
            "c1",
            CanonRelation::Canon,
            Importance::Minor,
            Vec::new(),
        )],
    );
    start((plan, world))
}

fn verdict(state: &NarrativeState, condition: &Condition) -> Verdict {
    evaluate(condition, &state.plan, &state.world).verdict
}

fn lost(state: &NarrativeState, condition: &Condition) -> Vec<LostDependency> {
    evaluate(condition, &state.plan, &state.world).lost
}

fn dead(character_id: &str) -> LostDependency {
    LostDependency::CharacterDead {
        character_id: character_id.into(),
    }
}

#[test]
fn flags_are_reversible_and_never_broken() {
    let state = sandbox();
    let on = flag("door_open");
    let off = Condition::FlagIs {
        flag: "door_open".into(),
        value: false,
    };
    // An unset flag reads as false.
    assert_eq!(verdict(&state, &on), Verdict::Unmet);
    assert_eq!(verdict(&state, &off), Verdict::Holds);

    let state = apply(&state, set_flag("door_open")).state;
    assert_eq!(verdict(&state, &on), Verdict::Holds);
    assert_eq!(verdict(&state, &off), Verdict::Unmet);

    let state = apply(
        &state,
        NarrativeEvent::FlagSet {
            flag: "door_open".into(),
            value: false,
        },
    )
    .state;
    assert_eq!(verdict(&state, &on), Verdict::Unmet);
}

#[test]
fn character_life_and_availability() {
    let state = sandbox();
    let is_alive = alive("ann");
    let is_dead = Condition::CharacterDead {
        character_id: "ann".into(),
    };
    let is_available = Condition::CharacterAvailable {
        character_id: "ann".into(),
    };
    assert_eq!(verdict(&state, &is_alive), Verdict::Holds);
    assert_eq!(verdict(&state, &is_available), Verdict::Holds);
    assert_eq!(verdict(&state, &is_dead), Verdict::Unmet);

    // Out of action is temporary.
    let away = apply(
        &state,
        NarrativeEvent::CharacterAvailabilityChanged {
            character_id: "ann".into(),
            available: false,
        },
    )
    .state;
    assert_eq!(verdict(&away, &is_alive), Verdict::Holds);
    assert_eq!(verdict(&away, &is_available), Verdict::Unmet);
    let back = apply(
        &away,
        NarrativeEvent::CharacterAvailabilityChanged {
            character_id: "ann".into(),
            available: true,
        },
    )
    .state;
    assert_eq!(verdict(&back, &is_available), Verdict::Holds);

    // Death is not.
    let gone = apply(&state, died("ann")).state;
    assert_eq!(verdict(&gone, &is_alive), Verdict::Broken);
    assert_eq!(lost(&gone, &is_alive), vec![dead("ann")]);
    assert_eq!(verdict(&gone, &is_available), Verdict::Broken);
    assert_eq!(verdict(&gone, &is_dead), Verdict::Holds);
}

#[test]
fn character_location() {
    let state = sandbox();
    let at_a = Condition::CharacterAt {
        character_id: "ann".into(),
        location_id: "a".into(),
    };
    let at_b = Condition::CharacterAt {
        character_id: "ann".into(),
        location_id: "b".into(),
    };
    assert_eq!(verdict(&state, &at_a), Verdict::Holds);
    assert_eq!(verdict(&state, &at_b), Verdict::Unmet);

    let moved = apply(
        &state,
        NarrativeEvent::CharacterMoved {
            character_id: "ann".into(),
            location_id: "b".into(),
        },
    )
    .state;
    assert_eq!(verdict(&moved, &at_b), Verdict::Holds);
    assert_eq!(verdict(&moved, &at_a), Verdict::Unmet);

    assert_eq!(
        lost(&apply(&state, died("ann")).state, &at_b),
        vec![dead("ann")]
    );

    let sealed = apply(
        &state,
        NarrativeEvent::LocationLost {
            location_id: "b".into(),
        },
    )
    .state;
    assert_eq!(
        lost(&sealed, &at_b),
        vec![LostDependency::LocationLost {
            location_id: "b".into()
        }]
    );
}

#[test]
fn player_location_and_lost_locations() {
    let state = sandbox();
    let at_b = Condition::PlayerAt {
        location_id: "b".into(),
    };
    let b_available = Condition::LocationAvailable {
        location_id: "b".into(),
    };
    assert_eq!(
        verdict(
            &state,
            &Condition::PlayerAt {
                location_id: "a".into()
            }
        ),
        Verdict::Holds
    );
    assert_eq!(verdict(&state, &at_b), Verdict::Unmet);
    assert_eq!(verdict(&state, &b_available), Verdict::Holds);
    assert_eq!(
        verdict(&apply(&state, player_moved("b")).state, &at_b),
        Verdict::Holds
    );

    let sealed = apply(
        &state,
        NarrativeEvent::LocationLost {
            location_id: "b".into(),
        },
    )
    .state;
    let gone = vec![LostDependency::LocationLost {
        location_id: "b".into(),
    }];
    assert_eq!(verdict(&sealed, &at_b), Verdict::Broken);
    assert_eq!(lost(&sealed, &at_b), gone);
    assert_eq!(lost(&sealed, &b_available), gone);
}

#[test]
fn destroyed_objects_stay_destroyed() {
    let state = sandbox();
    let intact = Condition::ObjectIntact {
        object_id: "gem".into(),
    };
    assert_eq!(verdict(&state, &intact), Verdict::Holds);

    let state = apply(
        &state,
        NarrativeEvent::ObjectDestroyed {
            object_id: "gem".into(),
        },
    )
    .state;
    assert_eq!(verdict(&state, &intact), Verdict::Broken);
    assert_eq!(
        lost(&state, &intact),
        vec![LostDependency::ObjectDestroyed {
            object_id: "gem".into()
        }]
    );
}

#[test]
fn previous_objective_state() {
    let state = sandbox();
    let is = |status| Condition::ObjectiveIs {
        objective_id: "o1".into(),
        status,
    };
    assert_eq!(objective_status(&state, "o1"), ObjectiveStatus::Active);
    assert_eq!(
        verdict(&state, &is(ObjectiveStatus::Active)),
        Verdict::Holds
    );
    assert_eq!(
        verdict(&state, &is(ObjectiveStatus::Completed)),
        Verdict::Unmet
    );
    // Statuses never move backward.
    assert_eq!(
        verdict(&state, &is(ObjectiveStatus::Pending)),
        Verdict::Broken
    );

    let done = apply(&state, set_flag("f1")).state;
    assert_eq!(
        verdict(&done, &is(ObjectiveStatus::Completed)),
        Verdict::Holds
    );
    assert_eq!(
        verdict(&done, &is(ObjectiveStatus::Failed)),
        Verdict::Broken
    );

    // A permanently failed objective breaks whatever needed it completed.
    let failed = apply(
        &state,
        NarrativeEvent::ObjectiveFailed {
            objective_id: "o1".into(),
        },
    )
    .state;
    assert_eq!(
        lost(&failed, &objective_done("o1")),
        vec![LostDependency::ObjectiveUnreachable {
            objective_id: "o1".into(),
            status: ObjectiveStatus::Failed,
        }]
    );
}

#[test]
fn mission_state() {
    let state = sandbox();
    let is = |status| Condition::MissionIs {
        mission_id: "m1".into(),
        status,
    };
    assert_eq!(verdict(&state, &is(MissionStatus::Active)), Verdict::Holds);
    assert_eq!(
        verdict(&state, &is(MissionStatus::Completed)),
        Verdict::Unmet
    );
    assert_eq!(
        verdict(&state, &is(MissionStatus::Inactive)),
        Verdict::Broken
    );

    let failed = apply(
        &state,
        NarrativeEvent::ObjectiveFailed {
            objective_id: "o1".into(),
        },
    )
    .state;
    assert_eq!(mission_status(&failed, "m1"), MissionStatus::Failed);
    assert_eq!(verdict(&failed, &is(MissionStatus::Failed)), Verdict::Holds);
    assert_eq!(
        lost(&failed, &is(MissionStatus::Completed)),
        vec![LostDependency::MissionUnreachable {
            mission_id: "m1".into(),
            status: MissionStatus::Failed,
        }]
    );
}

#[test]
fn known_facts_and_secrets() {
    let state = sandbox();
    let known = Condition::FactKnown {
        fact_id: "plan".into(),
        by: "bob".into(),
    };
    let secret = Condition::FactSecret {
        fact_id: "plan".into(),
        from: "bob".into(),
    };
    assert_eq!(verdict(&state, &known), Verdict::Unmet);
    assert_eq!(verdict(&state, &secret), Verdict::Holds);

    // Exposure cannot be undone.
    let told = apply(
        &state,
        NarrativeEvent::FactRevealed {
            fact_id: "plan".into(),
            to: "bob".into(),
        },
    )
    .state;
    assert_eq!(verdict(&told, &known), Verdict::Holds);
    assert_eq!(verdict(&told, &secret), Verdict::Broken);
    assert_eq!(
        lost(&told, &secret),
        vec![LostDependency::FactExposed {
            fact_id: "plan".into(),
            known_by: "bob".into()
        }]
    );

    // The dead learn nothing, but what they knew stays known.
    let silenced = apply(&state, died("bob")).state;
    assert_eq!(lost(&silenced, &known), vec![dead("bob")]);
    assert_eq!(verdict(&silenced, &secret), Verdict::Holds);
    assert_eq!(
        verdict(&apply(&told, died("bob")).state, &known),
        Verdict::Holds
    );
}

#[test]
fn relationship_thresholds() {
    let state = sandbox();
    let trusts = Condition::RelationshipAtLeast {
        from: "ann".into(),
        to: "player".into(),
        axis: RelationshipAxis::Trust,
        value: 10,
    };
    let unafraid = Condition::RelationshipAtMost {
        from: "ann".into(),
        to: "player".into(),
        axis: RelationshipAxis::Fear,
        value: 20,
    };
    assert_eq!(verdict(&state, &trusts), Verdict::Unmet);
    assert_eq!(verdict(&state, &unafraid), Verdict::Holds);

    let changed = apply(
        &state,
        NarrativeEvent::RelationshipChanged {
            from: "ann".into(),
            to: "player".into(),
            trust: 500,
            fear: 60,
            affinity: 0,
        },
    )
    .state;
    assert_eq!(verdict(&changed, &trusts), Verdict::Holds);
    assert_eq!(verdict(&changed, &unafraid), Verdict::Unmet);
    // Reported values are clamped to the valid range.
    assert_eq!(
        changed.world.relationship("ann", "player").trust,
        RELATIONSHIP_MAX
    );

    // There is no working toward a threshold with someone who has died.
    let gone = apply(&changed, died("ann")).state;
    assert_eq!(lost(&gone, &trusts), vec![dead("ann")]);
}

#[test]
fn world_truths() {
    let state = sandbox();
    assert_eq!(verdict(&state, &truth("t1")), Verdict::Holds);

    let ended = apply(
        &state,
        NarrativeEvent::TruthEnded {
            truth_id: "t1".into(),
        },
    )
    .state;
    assert_eq!(
        lost(&ended, &truth("t1")),
        vec![LostDependency::TruthEnded {
            truth_id: "t1".into()
        }]
    );

    let established = apply(
        &state,
        NarrativeEvent::TruthEstablished {
            truth_id: "t2".into(),
            statement: "Something new is true.".into(),
        },
    )
    .state;
    assert_eq!(verdict(&established, &truth("t2")), Verdict::Holds);
}

#[test]
fn checkpoint_reached() {
    let state = sandbox();
    assert_eq!(verdict(&state, &reached("c1")), Verdict::Unmet);

    let happened = apply(
        &state,
        NarrativeEvent::CheckpointReached {
            checkpoint_id: "c1".into(),
        },
    )
    .state;
    assert_eq!(verdict(&happened, &reached("c1")), Verdict::Holds);

    let skipped = apply(
        &state,
        NarrativeEvent::CheckpointSkipped {
            checkpoint_id: "c1".into(),
        },
    )
    .state;
    assert_eq!(
        lost(&skipped, &reached("c1")),
        vec![LostDependency::CheckpointUnreachable {
            checkpoint_id: "c1".into()
        }]
    );
}

#[test]
fn all_and_any_combine_three_valued() {
    let state = apply(&apply(&sandbox(), died("ann")).state, set_flag("yes")).state;
    let holds = flag("yes");
    let unmet = flag("no");
    let broken = alive("ann");
    let also_broken = Condition::CharacterAvailable {
        character_id: "ann".into(),
    };
    let all = |conditions: Vec<Condition>| Condition::All { conditions };
    let any = |conditions: Vec<Condition>| Condition::Any { conditions };

    assert_eq!(
        verdict(&state, &all(vec![holds.clone(), holds.clone()])),
        Verdict::Holds
    );
    assert_eq!(
        verdict(&state, &all(vec![holds.clone(), unmet.clone()])),
        Verdict::Unmet
    );
    // One broken part breaks the whole.
    let one_broken = all(vec![holds.clone(), unmet.clone(), broken.clone()]);
    assert_eq!(verdict(&state, &one_broken), Verdict::Broken);
    assert_eq!(lost(&state, &one_broken), vec![dead("ann")]);

    assert_eq!(
        verdict(&state, &any(vec![broken.clone(), holds])),
        Verdict::Holds
    );
    // An alternative that is merely unmet keeps the whole alive.
    assert_eq!(
        verdict(&state, &any(vec![broken.clone(), unmet])),
        Verdict::Unmet
    );
    let none_left = any(vec![broken, also_broken]);
    assert_eq!(verdict(&state, &none_left), Verdict::Broken);
    assert_eq!(lost(&state, &none_left), vec![dead("ann")]);
}

#[test]
fn prerequisite_lists_distinguish_unmet_impossible_and_waived() {
    let state = apply(&sandbox(), died("ann")).state;
    let check = |prerequisites: &[Prerequisite]| assess(prerequisites, &state.plan, &state.world);

    let met = check(&[essential(alive("bob"))]);
    assert!(met.satisfied && !met.impossible && !met.adapted);

    let waiting = check(&[essential(alive("bob")), essential(flag("later"))]);
    assert!(!waiting.satisfied && !waiting.impossible);

    let impossible = check(&[essential(alive("ann"))]);
    assert!(!impossible.satisfied && impossible.impossible);
    assert_eq!(impossible.lost_essential, vec![dead("ann")]);

    // A flexible prerequisite that is permanently lost is waived.
    let waived = check(&[essential(alive("bob")), flexible(alive("ann"))]);
    assert!(waived.satisfied && waived.adapted && !waived.impossible);
    assert_eq!(waived.lost_flexible, vec![dead("ann")]);
    assert!(waived.lost_essential.is_empty());

    // A flexible prerequisite that is merely unmet still has to be waited for.
    let patient = check(&[flexible(flag("later"))]);
    assert!(!patient.satisfied && !patient.adapted);
}

#[test]
fn objective_proceeds_without_a_lost_flexible_prerequisite() {
    let mut world = WorldFacts::default();
    world.add_character("guide", None);
    let cross = Objective {
        prerequisites: vec![flexible(alive("guide")), essential(flag("map_found"))],
        ..Objective::new("cross", "Cross the marsh")
    };
    let plan = plan_with(
        vec![Mission::new("marsh", "Marsh", vec![cross])],
        Vec::new(),
    );

    let state = apply(&start((plan, world)), died("guide")).state;
    assert_eq!(objective_status(&state, "cross"), ObjectiveStatus::Pending);
    assert_eq!(mission_status(&state, "marsh"), MissionStatus::Active);

    let state = apply(&state, set_flag("map_found")).state;
    assert_eq!(objective_status(&state, "cross"), ObjectiveStatus::Active);
}
