//! Canon adaptation: PRESERVE / ADAPT / REPLACE / DELETE, canon gravity, and
//! world truths that outlive the events planned around them.

use rift_backend::narrative::{
    CanonPolicy, CanonRelation, CheckpointStatus, Condition, IdKind, Importance, LostDependency,
    Mission, MissionStatus, NarrativeCheckpoint, NarrativeEngine, NarrativeError, NarrativeEvent,
    NarrativeState, Objective, REPLACE_MIN_GRAVITY, ReplanReason, WorldFacts, gravity,
};

use crate::fixtures::{
    alive, apply, beat, creature, died, essential, flag, flexible, plan_with, player_moved, policy,
    set_flag, start, truth,
};

fn stage() -> WorldFacts {
    let mut world = WorldFacts::default();
    world.add_location("bridge");
    world.add_location("tower");
    world.add_character("hero", Some("bridge"));
    world.add_character("rival", Some("tower"));
    world.add_truth("storm", "A storm is coming.", CanonRelation::Canon);
    world.player_location = Some("bridge".into());
    world
}

fn with_beats(checkpoints: Vec<NarrativeCheckpoint>) -> NarrativeState {
    start((plan_with(Vec::new(), checkpoints), stage()))
}

fn reach(checkpoint_id: &str) -> NarrativeEvent {
    NarrativeEvent::CheckpointReached {
        checkpoint_id: checkpoint_id.into(),
    }
}

fn status(state: &NarrativeState, checkpoint_id: &str) -> CheckpointStatus {
    state.plan.checkpoint(checkpoint_id).unwrap().status
}

fn rival_dead() -> Vec<LostDependency> {
    vec![LostDependency::CharacterDead {
        character_id: "rival".into(),
    }]
}

const ALL_IMPORTANCE: [Importance; 3] =
    [Importance::Minor, Importance::Major, Importance::Critical];
const ALL_RELATIONS: [CanonRelation; 3] = [
    CanonRelation::Generated,
    CanonRelation::Inferred,
    CanonRelation::Canon,
];

// -- PRESERVE -----------------------------------------------------------------

#[test]
fn beat_is_preserved_while_nothing_it_needs_is_lost() {
    let state = with_beats(vec![beat(
        "duel",
        CanonRelation::Canon,
        Importance::Major,
        vec![
            essential(alive("rival")),
            essential(Condition::PlayerAt {
                location_id: "tower".into(),
            }),
        ],
    )]);
    // The player is elsewhere: not ready, but nothing is lost, so it stays.
    assert_eq!(policy(&state, "duel"), CanonPolicy::Preserve);
    assert!(state.ready_checkpoints().is_empty());

    let t = apply(&state, player_moved("tower"));
    assert_eq!(policy(&t.state, "duel"), CanonPolicy::Preserve);
    assert_eq!(t.state.ready_checkpoints()[0].checkpoint_id, "duel");
    assert!(t.replan.is_none());
    assert!(t.state.divergences().is_empty());
}

// -- ADAPT --------------------------------------------------------------------

#[test]
fn losing_only_a_flexible_prerequisite_adapts_the_beat() {
    let state = with_beats(vec![beat(
        "duel",
        CanonRelation::Canon,
        Importance::Major,
        vec![
            essential(alive("rival")),
            flexible(Condition::LocationAvailable {
                location_id: "tower".into(),
            }),
        ],
    )]);
    let t = apply(
        &state,
        NarrativeEvent::LocationLost {
            location_id: "tower".into(),
        },
    );
    let tower_lost = vec![LostDependency::LocationLost {
        location_id: "tower".into(),
    }];

    assert_eq!(policy(&t.state, "duel"), CanonPolicy::Adapt);
    assert_eq!(status(&t.state, "duel"), CheckpointStatus::Pending);
    assert_eq!(t.checkpoint_changes[0].from_policy, CanonPolicy::Preserve);
    assert_eq!(t.checkpoint_changes[0].lost, tower_lost);

    let replan = t.replan.unwrap();
    assert_eq!(replan.reason, ReplanReason::CanonDivergence);
    assert_eq!(replan.lost_prerequisites, tower_lost);
    assert!(replan.beats[0].ready);

    // It can still happen, somewhere else, and is recorded as a divergence.
    let happened = apply(&t.state, reach("duel")).state;
    assert_eq!(status(&happened, "duel"), CheckpointStatus::Reached);
    assert_eq!(policy(&happened, "duel"), CanonPolicy::Adapt);
    assert_eq!(happened.divergences()[0].checkpoint_id, "duel");
}

// -- REPLACE ------------------------------------------------------------------

#[test]
fn impossible_beat_that_matters_is_replaced() {
    let state = with_beats(vec![beat(
        "duel",
        CanonRelation::Canon,
        Importance::Major,
        vec![essential(alive("rival"))],
    )]);
    let t = apply(&state, died("rival"));

    // The slot stays open for the Director; nothing is put in it.
    assert_eq!(policy(&t.state, "duel"), CanonPolicy::Replace);
    assert_eq!(status(&t.state, "duel"), CheckpointStatus::Pending);
    assert_eq!(t.state.plan.checkpoints.len(), 1);

    let replan = t.replan.unwrap();
    assert_eq!(replan.reason, ReplanReason::CanonDivergence);
    assert_eq!(replan.beats[0].policy, CanonPolicy::Replace);
    assert_eq!(replan.beats[0].lost, rival_dead());
    assert!(!replan.beats[0].ready);
}

// -- DELETE -------------------------------------------------------------------

#[test]
fn impossible_beat_that_does_not_matter_is_deleted() {
    let state = with_beats(vec![beat(
        "banter",
        CanonRelation::Generated,
        Importance::Major,
        vec![essential(alive("rival"))],
    )]);
    let t = apply(&state, died("rival"));

    assert_eq!(policy(&t.state, "banter"), CanonPolicy::Delete);
    assert_eq!(status(&t.state, "banter"), CheckpointStatus::Skipped);
    // The dropped beat is still reported once, with what it lost.
    let replan = t.replan.unwrap();
    assert_eq!(replan.beats[0].policy, CanonPolicy::Delete);
    assert_eq!(replan.beats[0].lost, rival_dead());

    // After that it is gone from the beats still ahead.
    assert!(t.state.assess_checkpoints().is_empty());
    assert!(matches!(
        NarrativeEngine::apply_event(&t.state, &reach("banter")),
        Err(NarrativeError::InvalidTransition(_))
    ));
}

// -- Canon gravity ------------------------------------------------------------

#[test]
fn gravity_decides_between_replace_and_delete() {
    let mut checkpoints = Vec::new();
    for importance in ALL_IMPORTANCE {
        for relation in ALL_RELATIONS {
            checkpoints.push(beat(
                &format!("beat_{}", checkpoints.len()),
                relation,
                importance,
                vec![essential(alive("rival"))],
            ));
        }
    }
    let state = apply(&with_beats(checkpoints), died("rival")).state;

    let (mut replaced, mut deleted) = (0, 0);
    for checkpoint in &state.plan.checkpoints {
        let pull = gravity(checkpoint.importance, checkpoint.canon_relation);
        if pull >= REPLACE_MIN_GRAVITY {
            assert_eq!(checkpoint.policy, CanonPolicy::Replace, "gravity {pull}");
            assert_eq!(checkpoint.status, CheckpointStatus::Pending);
            replaced += 1;
        } else {
            assert_eq!(checkpoint.policy, CanonPolicy::Delete, "gravity {pull}");
            assert_eq!(checkpoint.status, CheckpointStatus::Skipped);
            deleted += 1;
        }
    }
    // Critical anything, and major inferred/canon, are worth replacing.
    assert_eq!((replaced, deleted), (5, 4));
}

#[test]
fn gravity_never_preserves_a_beat_the_world_contradicts() {
    // The strongest pull there is: a critical canon event.
    let state = with_beats(vec![beat(
        "duel",
        CanonRelation::Canon,
        Importance::Critical,
        vec![essential(alive("rival"))],
    )]);
    let state = apply(&state, died("rival")).state;
    assert_ne!(policy(&state, "duel"), CanonPolicy::Preserve);
    assert_ne!(policy(&state, "duel"), CanonPolicy::Adapt);

    // Nor can it be declared to have happened anyway.
    assert_eq!(
        NarrativeEngine::apply_event(&state, &reach("duel")),
        Err(NarrativeError::Impossible {
            kind: IdKind::Checkpoint,
            id: "duel".into(),
            lost: rival_dead(),
        })
    );
    assert_eq!(status(&state, "duel"), CheckpointStatus::Pending);
}

#[test]
fn no_gravity_preserves_any_contradicted_beat() {
    for importance in ALL_IMPORTANCE {
        for relation in ALL_RELATIONS {
            let state = with_beats(vec![beat(
                "duel",
                relation,
                importance,
                vec![essential(alive("rival"))],
            )]);
            let state = apply(&state, died("rival")).state;
            assert!(
                matches!(
                    policy(&state, "duel"),
                    CanonPolicy::Replace | CanonPolicy::Delete
                ),
                "{importance:?} {relation:?}"
            );
        }
    }
}

#[test]
fn canon_alone_is_no_reason_to_preserve_and_generated_no_reason_to_drop() {
    // Same prerequisites, same world: a valid generated beat is preserved
    // exactly like a valid canon one.
    let state = with_beats(vec![
        beat(
            "canon_beat",
            CanonRelation::Canon,
            Importance::Critical,
            vec![essential(alive("hero"))],
        ),
        beat(
            "invented_beat",
            CanonRelation::Generated,
            Importance::Minor,
            vec![essential(alive("hero"))],
        ),
    ]);
    let state = apply(&state, died("rival")).state;
    assert_eq!(policy(&state, "canon_beat"), CanonPolicy::Preserve);
    assert_eq!(policy(&state, "invented_beat"), CanonPolicy::Preserve);
}

#[test]
fn ready_beats_are_ordered_by_gravity_then_plan_order() {
    let state = with_beats(vec![
        beat(
            "festival",
            CanonRelation::Generated,
            Importance::Minor,
            vec![],
        ),
        beat("duel", CanonRelation::Canon, Importance::Critical, vec![]),
        beat("rumor", CanonRelation::Inferred, Importance::Major, vec![]),
        beat("omen", CanonRelation::Canon, Importance::Minor, vec![]),
        beat("pact", CanonRelation::Generated, Importance::Major, vec![]),
        beat(
            "ambush",
            CanonRelation::Canon,
            Importance::Critical,
            vec![essential(flag("night_fell"))],
        ),
    ]);
    let order = |state: &NarrativeState| -> Vec<String> {
        state
            .ready_checkpoints()
            .into_iter()
            .map(|b| b.checkpoint_id)
            .collect()
    };
    // The high-importance canon event comes first; the ambush is not ready.
    assert_eq!(order(&state), ["duel", "rumor", "omen", "pact", "festival"]);

    // Once its prerequisite naturally holds it joins the front, after the
    // equally heavy beat that precedes it in the plan.
    let state = apply(&state, set_flag("night_fell")).state;
    assert_eq!(
        order(&state),
        ["duel", "ambush", "rumor", "omen", "pact", "festival"]
    );
}

#[test]
fn unmet_prerequisites_block_reaching_a_beat() {
    let state = with_beats(vec![beat(
        "ambush",
        CanonRelation::Canon,
        Importance::Critical,
        vec![essential(flag("night_fell"))],
    )]);
    assert_eq!(
        NarrativeEngine::apply_event(&state, &reach("ambush")),
        Err(NarrativeError::PrerequisitesNotMet {
            kind: IdKind::Checkpoint,
            id: "ambush".into(),
        })
    );
    let state = apply(&state, set_flag("night_fell")).state;
    assert_eq!(
        status(&apply(&state, reach("ambush")).state, "ambush"),
        CheckpointStatus::Reached
    );
}

// -- World truths vs canon events ---------------------------------------------

#[test]
fn world_truth_survives_a_skipped_canon_event() {
    let state = start(creature());
    let t = apply(
        &state,
        NarrativeEvent::CheckpointSkipped {
            checkpoint_id: "discover_creature_at_school".into(),
        },
    );

    // The protagonists never found it. The creature exists all the same.
    assert_eq!(
        status(&t.state, "discover_creature_at_school"),
        CheckpointStatus::Skipped
    );
    assert_eq!(t.state.world.truth_holds("creature_exists"), Some(true));

    // A beat that needs the truth is untouched and still ready...
    assert_eq!(
        policy(&t.state, "creature_attacks_town"),
        CanonPolicy::Preserve
    );
    assert_eq!(
        t.state.ready_checkpoints()[0].checkpoint_id,
        "creature_attacks_town"
    );
    // ...while a beat that needed the *event* is gone.
    assert_eq!(policy(&t.state, "heroes_celebrated"), CanonPolicy::Delete);
    assert_eq!(
        t.checkpoint_changes.last().unwrap().lost,
        vec![LostDependency::CheckpointUnreachable {
            checkpoint_id: "discover_creature_at_school".into()
        }]
    );

    let replan = t.replan.unwrap();
    assert_eq!(replan.reason, ReplanReason::CanonDivergence);
    assert_eq!(replan.remaining_context.holding_truths, ["creature_exists"]);
    assert_eq!(t.state.divergences().len(), 2);
}

#[test]
fn losing_where_a_canon_event_happens_does_not_erase_the_truth() {
    let state = apply(
        &start(creature()),
        NarrativeEvent::LocationLost {
            location_id: "school".into(),
        },
    )
    .state;
    assert_eq!(
        policy(&state, "discover_creature_at_school"),
        CanonPolicy::Replace
    );
    assert_eq!(state.world.truth_holds("creature_exists"), Some(true));
    assert_eq!(
        policy(&state, "creature_attacks_town"),
        CanonPolicy::Preserve
    );
    // Dependent beats are re-evaluated: the celebration needed the discovery.
    assert_eq!(policy(&state, "heroes_celebrated"), CanonPolicy::Delete);
}

#[test]
fn only_an_explicit_event_ends_a_truth_and_then_its_beats_are_moot() {
    let t = apply(
        &start(creature()),
        NarrativeEvent::TruthEnded {
            truth_id: "creature_exists".into(),
        },
    );
    assert_eq!(t.state.world.truth_holds("creature_exists"), Some(false));

    // Even a critical canon event is deleted rather than replaced: there is
    // nothing left for a replacement to be about.
    for id in [
        "discover_creature_at_school",
        "creature_attacks_town",
        "heroes_celebrated",
    ] {
        assert_eq!(policy(&t.state, id), CanonPolicy::Delete, "{id}");
        assert_eq!(status(&t.state, id), CheckpointStatus::Skipped, "{id}");
    }
    let replan = t.replan.unwrap();
    assert_eq!(replan.remaining_context.ended_truths, ["creature_exists"]);
    assert!(replan.remaining_context.holding_truths.is_empty());

    // An ended truth cannot quietly come back.
    assert!(
        NarrativeEngine::apply_event(
            &t.state,
            &NarrativeEvent::TruthEstablished {
                truth_id: "creature_exists".into(),
                statement: "It is back.".into(),
            },
        )
        .is_err()
    );
}

// -- Beats tied to missions ---------------------------------------------------

fn finale() -> NarrativeState {
    let objective = Objective {
        completes_when: Some(flag("done")),
        fails_when: Some(flag("botched")),
        ..Objective::new("the_job", "Do the job")
    };
    let finale = NarrativeCheckpoint {
        prerequisites: vec![essential(truth("storm"))],
        reached_when: Some(Condition::MissionIs {
            mission_id: "job".into(),
            status: MissionStatus::Completed,
        }),
        ..NarrativeCheckpoint::new(
            "finale",
            "Finale",
            CanonRelation::Generated,
            Importance::Critical,
        )
    };
    let plan = plan_with(
        vec![Mission::new("job", "The job", vec![objective])],
        vec![finale],
    );
    start((plan, stage()))
}

#[test]
fn beat_is_reached_when_its_mission_completes() {
    let t = apply(&finale(), set_flag("done"));
    assert_eq!(status(&t.state, "finale"), CheckpointStatus::Reached);
    assert_eq!(policy(&t.state, "finale"), CanonPolicy::Preserve);
    assert!(t.replan.is_none());
}

#[test]
fn failed_mission_makes_its_beat_impossible() {
    let t = apply(&finale(), set_flag("botched"));
    assert_eq!(status(&t.state, "finale"), CheckpointStatus::Pending);
    assert_eq!(policy(&t.state, "finale"), CanonPolicy::Replace);

    let replan = t.replan.unwrap();
    assert_eq!(replan.reason, ReplanReason::MissionFailed);
    assert_eq!(
        replan.beats[0].lost,
        vec![LostDependency::MissionUnreachable {
            mission_id: "job".into(),
            status: MissionStatus::Failed,
        }]
    );
}
