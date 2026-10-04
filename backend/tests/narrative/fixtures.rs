//! Story fixtures and small helpers shared by the narrative tests.
//!
//! The stories here (Maya, the ledger, the creature) are test data only;
//! nothing in `src/narrative/` knows about them.

use rift_backend::narrative::{
    CanonPolicy, CanonRelation, Condition, Effect, Importance, Mission, MissionStatus,
    NarrativeCheckpoint, NarrativeEngine, NarrativeEvent, NarrativePlan, NarrativeState,
    NarrativeTransition, Objective, ObjectiveStatus, Prerequisite, RelationshipAxis, WorldFacts,
};

// -- Conditions ---------------------------------------------------------------

pub fn flag(name: &str) -> Condition {
    Condition::FlagIs {
        flag: name.into(),
        value: true,
    }
}

pub fn alive(character_id: &str) -> Condition {
    Condition::CharacterAlive {
        character_id: character_id.into(),
    }
}

pub fn truth(truth_id: &str) -> Condition {
    Condition::TruthHolds {
        truth_id: truth_id.into(),
    }
}

pub fn objective_done(objective_id: &str) -> Condition {
    Condition::ObjectiveIs {
        objective_id: objective_id.into(),
        status: ObjectiveStatus::Completed,
    }
}

pub fn reached(checkpoint_id: &str) -> Condition {
    Condition::CheckpointReached {
        checkpoint_id: checkpoint_id.into(),
    }
}

pub fn essential(condition: Condition) -> Prerequisite {
    Prerequisite::essential(condition)
}

pub fn flexible(condition: Condition) -> Prerequisite {
    Prerequisite::flexible(condition)
}

// -- Events -------------------------------------------------------------------

pub fn set_flag(name: &str) -> NarrativeEvent {
    NarrativeEvent::FlagSet {
        flag: name.into(),
        value: true,
    }
}

pub fn died(character_id: &str) -> NarrativeEvent {
    NarrativeEvent::CharacterDied {
        character_id: character_id.into(),
    }
}

pub fn player_moved(location_id: &str) -> NarrativeEvent {
    NarrativeEvent::PlayerMoved {
        location_id: location_id.into(),
    }
}

// -- Driving the engine -------------------------------------------------------

pub fn start((plan, world): (NarrativePlan, WorldFacts)) -> NarrativeState {
    NarrativeEngine::start(plan, world)
        .expect("fixture is valid")
        .state
}

pub fn apply(state: &NarrativeState, event: NarrativeEvent) -> NarrativeTransition {
    NarrativeEngine::apply_event(state, &event).expect("event is valid")
}

pub fn mission_status(state: &NarrativeState, mission_id: &str) -> MissionStatus {
    state.plan.mission(mission_id).expect("mission").status
}

pub fn objective_status(state: &NarrativeState, objective_id: &str) -> ObjectiveStatus {
    state
        .plan
        .objective(objective_id)
        .expect("objective")
        .1
        .status
}

pub fn policy(state: &NarrativeState, checkpoint_id: &str) -> CanonPolicy {
    state
        .plan
        .checkpoint(checkpoint_id)
        .expect("checkpoint")
        .policy
}

// -- Building plans -----------------------------------------------------------

pub fn plan_with(missions: Vec<Mission>, checkpoints: Vec<NarrativeCheckpoint>) -> NarrativePlan {
    NarrativePlan {
        missions,
        checkpoints,
        ..NarrativePlan::new("test.plan", "test_universe")
    }
}

pub fn beat(
    checkpoint_id: &str,
    canon_relation: CanonRelation,
    importance: Importance,
    prerequisites: Vec<Prerequisite>,
) -> NarrativeCheckpoint {
    NarrativeCheckpoint {
        prerequisites,
        ..NarrativeCheckpoint::new(checkpoint_id, checkpoint_id, canon_relation, importance)
    }
}

// -- Stories ------------------------------------------------------------------

/// Work with companion Maya to stop an experiment.
pub fn experiment() -> (NarrativePlan, WorldFacts) {
    let mut world = WorldFacts::default();
    for location in ["lab_entrance", "containment_lab", "town"] {
        world.add_location(location);
    }
    world.add_character("maya", Some("lab_entrance"));
    world.add_character("dr_voss", Some("containment_lab"));
    world.add_object("reactor_console");
    world.add_truth(
        "experiment_running",
        "The containment experiment is running and unstable.",
        CanonRelation::Canon,
    );
    world.player_location = Some("lab_entrance".into());

    let meet = Objective {
        prerequisites: vec![essential(alive("maya"))],
        completes_when: Some(flag("met_maya")),
        ..Objective::new("meet_maya", "Find Maya at the lab entrance")
    };
    let bypass = Objective {
        prerequisites: vec![
            essential(alive("maya")),
            essential(objective_done("meet_maya")),
        ],
        completes_when: Some(flag("lab_door_open")),
        ..Objective::new("maya_bypasses_door", "Have Maya bypass the lab door")
    };
    let shut_down = Objective {
        prerequisites: vec![
            essential(objective_done("maya_bypasses_door")),
            essential(Condition::ObjectIntact {
                object_id: "reactor_console".into(),
            }),
        ],
        completes_when: Some(flag("reactor_offline")),
        success_effects: vec![Effect::EndTruth {
            truth_id: "experiment_running".into(),
        }],
        ..Objective::new("shut_down_reactor", "Shut the reactor down")
    };
    let secret = Objective {
        optional: true,
        prerequisites: vec![essential(alive("maya"))],
        ..Objective::new("learn_maya_secret", "Learn why Maya came back")
    };

    let mission = Mission {
        description: "Work with Maya to stop the experiment before it breaches.".into(),
        prerequisites: vec![essential(truth("experiment_running"))],
        success_effects: vec![Effect::SetFlag {
            flag: "experiment_stopped".into(),
            value: true,
        }],
        related_characters: vec!["maya".into(), "dr_voss".into()],
        related_locations: vec!["containment_lab".into()],
        canon_relation: CanonRelation::Canon,
        importance: Importance::Critical,
        ..Mission::new(
            "stop_experiment",
            "Stop the experiment",
            vec![meet, bypass, shut_down, secret],
        )
    };

    let checkpoints = vec![
        beat(
            "maya_sacrifice",
            CanonRelation::Canon,
            Importance::Critical,
            vec![
                essential(alive("maya")),
                essential(truth("experiment_running")),
            ],
        ),
        beat(
            "voss_escapes",
            CanonRelation::Canon,
            Importance::Minor,
            vec![essential(alive("dr_voss"))],
        ),
        beat(
            "containment_breach",
            CanonRelation::Inferred,
            Importance::Major,
            vec![essential(truth("experiment_running"))],
        ),
    ];

    (plan_with(vec![mission], checkpoints), world)
}

pub const LEDGER: &str = "ledger_discrepancy";
pub const LAW: &str = "law_enforcement";

/// Hide evidence from law enforcement.
pub fn evidence() -> (NarrativePlan, WorldFacts) {
    let mut world = WorldFacts::default();
    for location in ["car_wash", "dea_office"] {
        world.add_location(location);
    }
    world.add_character("walter", Some("car_wash"));
    world.add_character("hank", Some("car_wash"));
    world.add_truth(
        "walter_double_life",
        "Walter hides a criminal enterprise from his family.",
        CanonRelation::Inferred,
    );
    world.player_location = Some("car_wash".into());

    let exposed = Condition::FactKnown {
        fact_id: LEDGER.into(),
        by: LAW.into(),
    };
    let secret = Condition::FactSecret {
        fact_id: LEDGER.into(),
        from: LAW.into(),
    };

    let hide = Objective {
        completes_when: Some(flag("ledger_hidden")),
        fails_when: Some(exposed.clone()),
        failure_effects: vec![
            Effect::SetFlag {
                flag: "police_alerted".into(),
                value: true,
            },
            Effect::EstablishTruth {
                truth_id: "dea_watching_car_wash".into(),
                statement: "The DEA is watching the car wash.".into(),
            },
        ],
        ..Objective::new(
            "hide_ledger",
            "Hide the ledger discrepancy from law enforcement",
        )
    };
    let trust = Objective {
        prerequisites: vec![essential(objective_done("hide_ledger"))],
        completes_when: Some(Condition::RelationshipAtLeast {
            from: "walter".into(),
            to: "player".into(),
            axis: RelationshipAxis::Trust,
            value: 50,
        }),
        ..Objective::new("earn_walter_trust", "Earn Walter's trust")
    };
    let mission = Mission {
        failure_effects: vec![Effect::AdjustRelationship {
            from: "walter".into(),
            to: "player".into(),
            axis: RelationshipAxis::Trust,
            delta: -40,
        }],
        related_characters: vec!["walter".into(), "hank".into()],
        related_locations: vec!["car_wash".into()],
        ..Mission::new(
            "protect_the_books",
            "Keep the books quiet",
            vec![hide, trust],
        )
    };

    let checkpoints = vec![
        beat(
            "walter_confides",
            CanonRelation::Canon,
            Importance::Major,
            vec![essential(secret.clone()), essential(alive("walter"))],
        ),
        beat(
            "quiet_sale",
            CanonRelation::Generated,
            Importance::Minor,
            vec![essential(secret)],
        ),
        beat(
            "hank_investigates",
            CanonRelation::Canon,
            Importance::Critical,
            vec![essential(exposed), essential(alive("hank"))],
        ),
        beat(
            "walter_flees",
            CanonRelation::Inferred,
            Importance::Major,
            vec![essential(reached("hank_investigates"))],
        ),
        beat(
            "family_dinner",
            CanonRelation::Canon,
            Importance::Minor,
            vec![essential(reached("walter_confides"))],
        ),
    ];

    (plan_with(vec![mission], checkpoints), world)
}

/// A creature that exists whether or not anyone ever discovers it.
pub fn creature() -> (NarrativePlan, WorldFacts) {
    let mut world = WorldFacts::default();
    for location in ["school", "woods", "town_square"] {
        world.add_location(location);
    }
    world.add_character("mike", Some("school"));
    world.add_character("will", Some("woods"));
    world.add_truth(
        "creature_exists",
        "The creature exists.",
        CanonRelation::Canon,
    );
    world.player_location = Some("woods".into());

    let checkpoints = vec![
        beat(
            "discover_creature_at_school",
            CanonRelation::Canon,
            Importance::Major,
            vec![
                essential(truth("creature_exists")),
                essential(Condition::LocationAvailable {
                    location_id: "school".into(),
                }),
            ],
        ),
        beat(
            "creature_attacks_town",
            CanonRelation::Canon,
            Importance::Critical,
            vec![essential(truth("creature_exists"))],
        ),
        beat(
            "heroes_celebrated",
            CanonRelation::Canon,
            Importance::Minor,
            vec![essential(reached("discover_creature_at_school"))],
        ),
    ];

    (plan_with(Vec::new(), checkpoints), world)
}
