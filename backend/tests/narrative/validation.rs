//! Structural validation: bad ids, dangling references and dependency cycles
//! are rejected before anything runs.

use rift_backend::narrative::{
    CanonRelation, Condition, Effect, IdKind, Importance, Mission, MissionStatus,
    NarrativeCheckpoint, NarrativeEngine, NarrativeError, NarrativeEvent, NarrativePlan, Objective,
    ObjectiveStatus, WorldFacts, validate,
};

use crate::fixtures::{
    alive, beat, essential, experiment, flag, objective_done, plan_with, reached, start, truth,
};

fn world() -> WorldFacts {
    let mut world = WorldFacts::default();
    world.add_location("hall");
    world.add_character("ann", Some("hall"));
    world.add_object("gem");
    world.add_truth("t1", "Something is true.", CanonRelation::Canon);
    world
}

fn objective(id: &str) -> Objective {
    Objective::new(id, id)
}

fn mission(id: &str, objectives: Vec<Objective>) -> Mission {
    Mission::new(id, id, objectives)
}

/// A one-objective plan whose objective requires `condition`.
fn requiring(condition: Condition) -> NarrativePlan {
    let needy = Objective {
        prerequisites: vec![essential(condition)],
        ..objective("o1")
    };
    plan_with(vec![mission("m1", vec![needy])], Vec::new())
}

fn check(plan: &NarrativePlan) -> Result<(), NarrativeError> {
    validate(plan, &world())
}

fn is_unknown(result: Result<(), NarrativeError>, expected: IdKind, expected_id: &str) -> bool {
    matches!(result, Err(NarrativeError::UnknownId { kind, id }) if kind == expected && id == expected_id)
}

fn is_invalid(result: Result<(), NarrativeError>, expected: IdKind) -> bool {
    matches!(result, Err(NarrativeError::InvalidIdentifier { kind, .. }) if kind == expected)
}

fn is_duplicate(result: Result<(), NarrativeError>, expected: IdKind) -> bool {
    matches!(result, Err(NarrativeError::DuplicateId { kind, .. }) if kind == expected)
}

#[test]
fn accepts_a_well_formed_plan() {
    let (plan, world) = experiment();
    assert_eq!(validate(&plan, &world), Ok(()));
}

#[test]
fn rejects_unknown_schema_version() {
    let mut plan = requiring(alive("ann"));
    plan.schema_version = 2;
    assert_eq!(
        check(&plan),
        Err(NarrativeError::UnsupportedSchemaVersion(2))
    );
    assert!(NarrativeEngine::start(plan, world()).is_err());
}

#[test]
fn rejects_malformed_ids() {
    assert!(is_invalid(
        check(&plan_with(
            vec![mission("bad id", vec![objective("o1")])],
            Vec::new()
        )),
        IdKind::Mission
    ));
    assert!(is_invalid(
        check(&plan_with(
            vec![mission("m1", vec![objective("")])],
            Vec::new()
        )),
        IdKind::Objective
    ));
    assert!(is_invalid(
        check(&plan_with(
            Vec::new(),
            vec![beat(
                "drop;table",
                CanonRelation::Canon,
                Importance::Minor,
                Vec::new()
            )]
        )),
        IdKind::Checkpoint
    ));
    assert!(is_invalid(
        check(&requiring(alive("maya 2"))),
        IdKind::Character
    ));
    // Flag keys may be longer than entity ids, but not unbounded.
    assert_eq!(check(&requiring(flag(&"f".repeat(100)))), Ok(()));
    assert!(is_invalid(
        check(&requiring(flag(&"f".repeat(129)))),
        IdKind::Flag
    ));
    assert!(is_invalid(
        check(&requiring(alive(&"c".repeat(65)))),
        IdKind::Character
    ));

    let mut plan = requiring(alive("ann"));
    plan.plan_id = "not a plan id".into();
    assert!(is_invalid(check(&plan), IdKind::Plan));
}

#[test]
fn rejects_a_world_that_does_not_hold_together() {
    let plan = requiring(alive("ann"));

    let mut bad = world();
    bad.add_character("bad id", None);
    assert!(is_invalid(validate(&plan, &bad), IdKind::Character));

    // "player" is the player, never a character in the cast.
    let mut bad = world();
    bad.add_character("player", None);
    assert!(is_invalid(validate(&plan, &bad), IdKind::Character));

    let mut bad = world();
    bad.add_character("bob", Some("nowhere"));
    assert!(is_unknown(
        validate(&plan, &bad),
        IdKind::Location,
        "nowhere"
    ));

    let mut bad = world();
    bad.player_location = Some("nowhere".into());
    assert!(is_unknown(
        validate(&plan, &bad),
        IdKind::Location,
        "nowhere"
    ));

    let mut bad = world();
    bad.lost_locations.insert("atlantis".into());
    assert!(is_unknown(
        validate(&plan, &bad),
        IdKind::Location,
        "atlantis"
    ));

    let mut bad = world();
    bad.destroyed_objects.insert("grail".into());
    assert!(is_unknown(validate(&plan, &bad), IdKind::Object, "grail"));
}

#[test]
fn rejects_duplicate_ids() {
    assert!(is_duplicate(
        check(&plan_with(
            vec![
                mission("m1", vec![objective("o1")]),
                mission("m1", vec![objective("o2")])
            ],
            Vec::new()
        )),
        IdKind::Mission
    ));
    // Objective ids are unique across the whole plan, not per mission.
    assert!(is_duplicate(
        check(&plan_with(
            vec![
                mission("m1", vec![objective("o1")]),
                mission("m2", vec![objective("o1")])
            ],
            Vec::new()
        )),
        IdKind::Objective
    ));
    let twin = || beat("c1", CanonRelation::Canon, Importance::Minor, Vec::new());
    assert!(is_duplicate(
        check(&plan_with(Vec::new(), vec![twin(), twin()])),
        IdKind::Checkpoint
    ));
}

#[test]
fn rejects_references_to_things_that_do_not_exist() {
    let cases = [
        (alive("maya_2"), IdKind::Character, "maya_2"),
        (
            Condition::CharacterAt {
                character_id: "ann".into(),
                location_id: "moon".into(),
            },
            IdKind::Location,
            "moon",
        ),
        (
            Condition::PlayerAt {
                location_id: "moon".into(),
            },
            IdKind::Location,
            "moon",
        ),
        (
            Condition::ObjectIntact {
                object_id: "grail".into(),
            },
            IdKind::Object,
            "grail",
        ),
        (objective_done("o9"), IdKind::Objective, "o9"),
        (
            Condition::MissionIs {
                mission_id: "m9".into(),
                status: MissionStatus::Completed,
            },
            IdKind::Mission,
            "m9",
        ),
        (reached("c9"), IdKind::Checkpoint, "c9"),
        (truth("t9"), IdKind::Truth, "t9"),
        // Nested conditions are checked too.
        (
            Condition::Any {
                conditions: vec![
                    alive("ann"),
                    Condition::All {
                        conditions: vec![alive("ghost")],
                    },
                ],
            },
            IdKind::Character,
            "ghost",
        ),
    ];
    for (condition, kind, id) in cases {
        assert!(
            is_unknown(check(&requiring(condition.clone())), kind, id),
            "{condition:?}"
        );
    }

    let mut plan = requiring(alive("ann"));
    plan.missions[0].related_characters = vec!["stranger".into()];
    assert!(is_unknown(check(&plan), IdKind::Character, "stranger"));

    let mut plan = requiring(alive("ann"));
    plan.missions[0].related_locations = vec!["moon".into()];
    assert!(is_unknown(check(&plan), IdKind::Location, "moon"));

    // Completion, failure and reach conditions are checked like prerequisites.
    let mut plan = requiring(alive("ann"));
    plan.missions[0].objectives[0].completes_when = Some(alive("ghost"));
    assert!(is_unknown(check(&plan), IdKind::Character, "ghost"));

    let mut plan = requiring(alive("ann"));
    plan.missions[0].objectives[0].fails_when = Some(alive("ghost"));
    assert!(is_unknown(check(&plan), IdKind::Character, "ghost"));

    let mut plan = requiring(alive("ann"));
    plan.checkpoints.push(NarrativeCheckpoint {
        reached_when: Some(alive("ghost")),
        ..NarrativeCheckpoint::new("c1", "c1", CanonRelation::Canon, Importance::Minor)
    });
    assert!(is_unknown(check(&plan), IdKind::Character, "ghost"));
}

#[test]
fn effects_may_only_touch_truths_that_exist_or_that_the_plan_establishes() {
    let mut plan = requiring(alive("ann"));
    plan.missions[0].success_effects = vec![Effect::EndTruth {
        truth_id: "t9".into(),
    }];
    assert!(is_unknown(check(&plan), IdKind::Truth, "t9"));

    // A truth the plan establishes may be ended, and required, by the plan.
    plan.missions[0].objectives[0].success_effects = vec![Effect::EstablishTruth {
        truth_id: "t9".into(),
        statement: "Now it is true.".into(),
    }];
    plan.checkpoints.push(beat(
        "c1",
        CanonRelation::Generated,
        Importance::Minor,
        vec![essential(truth("t9"))],
    ));
    assert_eq!(check(&plan), Ok(()));

    plan.missions[0].failure_effects = vec![Effect::SetFlag {
        flag: "not a flag".into(),
        value: true,
    }];
    assert!(is_invalid(check(&plan), IdKind::Flag));
}

#[test]
fn rejects_shapes_that_can_never_work() {
    let empty_any = requiring(Condition::Any {
        conditions: Vec::new(),
    });
    assert!(matches!(
        check(&empty_any),
        Err(NarrativeError::InvalidPlan(_))
    ));

    // A mission needs something that can decide its outcome.
    let only_optional = plan_with(
        vec![mission(
            "m1",
            vec![Objective {
                optional: true,
                ..objective("o1")
            }],
        )],
        Vec::new(),
    );
    assert!(matches!(
        check(&only_optional),
        Err(NarrativeError::InvalidPlan(_))
    ));
    assert!(matches!(
        check(&plan_with(vec![mission("m1", Vec::new())], Vec::new())),
        Err(NarrativeError::InvalidPlan(_))
    ));
}

#[test]
fn rejects_dependency_cycles() {
    let needs = |id: &str, on: &str| Objective {
        prerequisites: vec![essential(objective_done(on))],
        ..objective(id)
    };
    let cycle = |plan: &NarrativePlan| match check(plan) {
        Err(NarrativeError::DependencyCycle { path }) => path,
        other => panic!("expected a cycle, got {other:?}"),
    };

    // An objective that waits on itself.
    let plan = plan_with(vec![mission("m1", vec![needs("o1", "o1")])], Vec::new());
    assert_eq!(cycle(&plan), ["objective:o1", "objective:o1"]);

    // Two objectives that wait on each other.
    let plan = plan_with(
        vec![mission("m1", vec![needs("o1", "o2"), needs("o2", "o1")])],
        Vec::new(),
    );
    assert_eq!(
        cycle(&plan),
        ["objective:o1", "objective:o2", "objective:o1"]
    );

    // A longer loop through a mission, a beat and an objective:
    // m1 waits for beat c1, c1 for objective o2, o2 for mission m1.
    let m1 = Mission {
        prerequisites: vec![essential(reached("c1"))],
        ..mission("m1", vec![objective("o1")])
    };
    let o2 = Objective {
        prerequisites: vec![essential(Condition::MissionIs {
            mission_id: "m1".into(),
            status: MissionStatus::Completed,
        })],
        ..objective("o2")
    };
    let c1 = beat(
        "c1",
        CanonRelation::Canon,
        Importance::Major,
        vec![essential(objective_done("o2"))],
    );
    let plan = plan_with(vec![m1, mission("m2", vec![o2])], vec![c1]);
    let path = cycle(&plan);
    assert_eq!(path.first(), path.last());
    assert_eq!(path.len(), 4);
    for node in ["mission:m1", "checkpoint:c1", "objective:o2"] {
        assert!(path.iter().any(|n| n == node), "{node} in {path:?}");
    }

    // A cycle hidden in a completion condition or behind all/any counts.
    let hidden = Objective {
        completes_when: Some(Condition::Any {
            conditions: vec![flag("x"), objective_done("o2")],
        }),
        ..objective("o1")
    };
    let plan = plan_with(
        vec![mission("m1", vec![hidden, needs("o2", "o1")])],
        Vec::new(),
    );
    assert_eq!(cycle(&plan).len(), 3);
}

#[test]
fn accepts_chains_and_shared_dependencies() {
    let needs = |id: &str, on: &[&str]| Objective {
        prerequisites: on.iter().map(|o| essential(objective_done(o))).collect(),
        ..objective(id)
    };
    // o1 <- o2, o1 <- o3, {o2, o3} <- o4: a diamond, not a cycle.
    let plan = plan_with(
        vec![mission(
            "m1",
            vec![
                objective("o1"),
                needs("o2", &["o1"]),
                needs("o3", &["o1"]),
                needs("o4", &["o2", "o3"]),
            ],
        )],
        Vec::new(),
    );
    assert_eq!(check(&plan), Ok(()));
}

#[test]
fn events_with_bad_ids_are_rejected() {
    let state = start(experiment());
    let unknown = |event: NarrativeEvent, expected: IdKind| {
        matches!(
            NarrativeEngine::apply_event(&state, &event),
            Err(NarrativeError::UnknownId { kind, .. }) if kind == expected
        )
    };

    assert!(unknown(
        NarrativeEvent::CharacterDied {
            character_id: "maya_2".into()
        },
        IdKind::Character
    ));
    assert!(unknown(
        NarrativeEvent::CharacterMoved {
            character_id: "maya".into(),
            location_id: "moon".into()
        },
        IdKind::Location
    ));
    assert!(unknown(
        NarrativeEvent::PlayerMoved {
            location_id: "moon".into()
        },
        IdKind::Location
    ));
    assert!(unknown(
        NarrativeEvent::LocationLost {
            location_id: "moon".into()
        },
        IdKind::Location
    ));
    assert!(unknown(
        NarrativeEvent::ObjectDestroyed {
            object_id: "grail".into()
        },
        IdKind::Object
    ));
    assert!(unknown(
        NarrativeEvent::TruthEnded {
            truth_id: "t9".into()
        },
        IdKind::Truth
    ));
    assert!(unknown(
        NarrativeEvent::ObjectiveCompleted {
            objective_id: "o9".into()
        },
        IdKind::Objective
    ));
    assert!(unknown(
        NarrativeEvent::ObjectiveFailed {
            objective_id: "o9".into()
        },
        IdKind::Objective
    ));
    assert!(unknown(
        NarrativeEvent::CheckpointReached {
            checkpoint_id: "c9".into()
        },
        IdKind::Checkpoint
    ));
    assert!(unknown(
        NarrativeEvent::CheckpointSkipped {
            checkpoint_id: "c9".into()
        },
        IdKind::Checkpoint
    ));

    // Free-form keys are still checked for shape.
    for event in [
        NarrativeEvent::FlagSet {
            flag: "not a flag".into(),
            value: true,
        },
        NarrativeEvent::FactRevealed {
            fact_id: "secret".into(),
            to: "some one".into(),
        },
        NarrativeEvent::TruthEstablished {
            truth_id: "".into(),
            statement: "Nothing.".into(),
        },
    ] {
        assert!(matches!(
            NarrativeEngine::apply_event(&state, &event),
            Err(NarrativeError::InvalidIdentifier { .. })
        ));
    }

    // A truth cannot be established twice.
    assert!(matches!(
        NarrativeEngine::apply_event(
            &state,
            &NarrativeEvent::TruthEstablished {
                truth_id: "experiment_running".into(),
                statement: "Again.".into(),
            }
        ),
        Err(NarrativeError::DuplicateId {
            kind: IdKind::Truth,
            ..
        })
    ));
}

#[test]
fn adopted_missions_are_validated_like_the_rest_of_the_plan() {
    let state = start(experiment());
    let adopt = |mission: Mission| NarrativeEngine::adopt_mission(&state, mission);
    let fresh = |id: &str, objective_id: &str| mission(id, vec![objective(objective_id)]);

    assert!(matches!(
        adopt(fresh("stop_experiment", "new_objective")),
        Err(NarrativeError::DuplicateId {
            kind: IdKind::Mission,
            ..
        })
    ));
    assert!(matches!(
        adopt(fresh("new_mission", "meet_maya")),
        Err(NarrativeError::DuplicateId {
            kind: IdKind::Objective,
            ..
        })
    ));
    assert!(matches!(
        adopt(Mission {
            related_characters: vec!["maya_2".into()],
            ..fresh("new_mission", "new_objective")
        }),
        Err(NarrativeError::UnknownId {
            kind: IdKind::Character,
            ..
        })
    ));

    // A proposal arrives fresh; it cannot smuggle in progress.
    assert!(matches!(
        adopt(Mission {
            status: MissionStatus::Completed,
            ..fresh("new_mission", "new_objective")
        }),
        Err(NarrativeError::InvalidPlan(_))
    ));
    let mut done = fresh("new_mission", "new_objective");
    done.objectives[0].status = ObjectiveStatus::Completed;
    assert!(matches!(adopt(done), Err(NarrativeError::InvalidPlan(_))));

    // And a rejected proposal leaves the plan as it was.
    assert_eq!(state.plan.missions.len(), 1);
    assert!(adopt(fresh("new_mission", "new_objective")).is_ok());
}
