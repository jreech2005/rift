//! The initial plan builder, run against a real-shape WorldBible V1 document.

use rift_backend::narrative::{
    CanonRelation, CharacterStatus, CheckpointStatus, Condition, IdKind, Importance,
    LostDependency, MissionStatus, NarrativeEngine, NarrativeError, NarrativeState,
    OPENING_CHECKPOINT_ID, OPENING_MISSION_ID, OPENING_REACH_OBJECTIVE_ID,
    OPENING_RESOLVE_OBJECTIVE_ID, ObjectiveStatus, ReplanReason, WorldBibleSeed, build_initial,
    opening_choice_flag,
};

use crate::fixtures::{apply, died, mission_status, objective_status, player_moved, set_flag};

/// A full WorldBible V1, as the universe compiler writes it to
/// `cache/universes/` (generated offline from the canon test fixtures).
const WORLD_BIBLE: &str = include_str!("fixtures/world_bible_breaking_bad.json");

fn bible() -> WorldBibleSeed {
    serde_json::from_str(WORLD_BIBLE).expect("a WorldBible V1 document is a valid seed")
}

fn opening(seed: &WorldBibleSeed) -> NarrativeState {
    let (plan, world) = build_initial(seed).unwrap();
    NarrativeEngine::start(plan, world).unwrap().state
}

#[test]
fn builds_one_opening_mission_from_a_world_bible() {
    let (plan, _) = build_initial(&bible()).unwrap();

    assert_eq!(plan.plan_id, "breaking_bad_tv_1396.opening");
    assert_eq!(plan.universe_id, "breaking_bad_tv_1396");
    assert_eq!(plan.missions.len(), 1);
    assert_eq!(plan.checkpoints.len(), 1);

    let mission = &plan.missions[0];
    assert_eq!(mission.mission_id, OPENING_MISSION_ID);
    assert_eq!(mission.title, "Cash That Does Not Add Up");
    // The opening conflict is invented for the game, and it is the story.
    assert_eq!(mission.canon_relation, CanonRelation::Generated);
    assert_eq!(mission.importance, Importance::Critical);
    assert_eq!(
        mission.related_characters,
        ["walter_white", "hank_schrader", "marisol_vega"]
    );
    assert_eq!(mission.related_locations, ["a1a_car_wash"]);
    assert_eq!(
        mission.metadata["decision_option_1"],
        "Confront Walter privately"
    );
    assert_eq!(mission.metadata.len(), 5);

    // It needs its location and every character involved in it.
    assert_eq!(mission.prerequisites.len(), 4);
    assert!(mission.prerequisites.iter().any(|p| p.condition
        == Condition::CharacterAlive {
            character_id: "hank_schrader".into()
        }));

    // The player starts on the scene, so there is nothing to walk to.
    assert_eq!(mission.objectives.len(), 1);
    assert_eq!(
        mission.objectives[0].description,
        "Decide what to do with the discrepancy before Hank leaves."
    );
}

#[test]
fn builds_the_starting_world_with_truths_separate_from_the_plan() {
    let (_, world) = build_initial(&bible()).unwrap();

    assert_eq!(world.characters.len(), 4);
    assert!(
        world
            .characters
            .values()
            .all(|c| c.status == CharacterStatus::Available)
    );
    assert_eq!(
        world.characters["walter_white"].location.as_deref(),
        Some("a1a_car_wash")
    );
    // Not part of the opening conflict: his whereabouts are unknown.
    assert_eq!(world.characters["jesse_pinkman"].location, None);
    assert_eq!(world.locations.len(), 3);
    assert_eq!(world.player_location.as_deref(), Some("a1a_car_wash"));

    // World rules and standing conflicts are truths, not events to play out.
    let truths: Vec<&str> = world.truths.keys().map(String::as_str).collect();
    assert_eq!(
        truths,
        ["walter_double_life", "world_rule_1", "world_rule_2"]
    );
    assert!(world.truths.values().all(|t| t.holds));
    assert_eq!(
        world.truths["world_rule_1"].canon_relation,
        CanonRelation::Canon
    );
    assert_eq!(
        world.truths["walter_double_life"].canon_relation,
        CanonRelation::Inferred
    );
}

#[test]
fn building_is_deterministic() {
    assert_eq!(build_initial(&bible()), build_initial(&bible()));
}

#[test]
fn opening_mission_starts_active_and_any_decision_resolves_it() {
    let state = opening(&bible());
    assert_eq!(
        mission_status(&state, OPENING_MISSION_ID),
        MissionStatus::Active
    );
    assert_eq!(
        objective_status(&state, OPENING_RESOLVE_OBJECTIVE_ID),
        ObjectiveStatus::Active
    );

    for choice in 1..=3 {
        let t = apply(&state, set_flag(&opening_choice_flag(choice)));
        assert_eq!(
            mission_status(&t.state, OPENING_MISSION_ID),
            MissionStatus::Completed
        );
        assert_eq!(
            t.state
                .plan
                .checkpoint(OPENING_CHECKPOINT_ID)
                .unwrap()
                .status,
            CheckpointStatus::Reached
        );
        assert!(t.replan.is_none());
    }
    // There is no fourth option.
    let t = apply(&state, set_flag(&opening_choice_flag(4)));
    assert_eq!(
        mission_status(&t.state, OPENING_MISSION_ID),
        MissionStatus::Active
    );
}

#[test]
fn losing_an_opening_character_invalidates_the_opening() {
    let t = apply(&opening(&bible()), died("hank_schrader"));
    assert_eq!(
        mission_status(&t.state, OPENING_MISSION_ID),
        MissionStatus::Invalidated
    );
    let replan = t.replan.unwrap();
    assert_eq!(replan.reason, ReplanReason::MissionInvalidated);
    assert_eq!(
        replan.lost_prerequisites[0],
        LostDependency::CharacterDead {
            character_id: "hank_schrader".into()
        }
    );
    // The standing conflict is a truth and is still there to build on.
    assert!(
        replan
            .remaining_context
            .holding_truths
            .contains(&"walter_double_life".to_owned())
    );

    // Someone outside the opening conflict does not affect it.
    let t = apply(&opening(&bible()), died("jesse_pinkman"));
    assert_eq!(
        mission_status(&t.state, OPENING_MISSION_ID),
        MissionStatus::Active
    );
}

#[test]
fn player_must_reach_the_scene_when_starting_elsewhere() {
    let mut seed = bible();
    seed.starting_location.location_id = "wynne_high_school".into();
    let state = opening(&seed);

    let ids: Vec<&str> = state.plan.missions[0]
        .objectives
        .iter()
        .map(|o| o.objective_id.as_str())
        .collect();
    assert_eq!(
        ids,
        [OPENING_REACH_OBJECTIVE_ID, OPENING_RESOLVE_OBJECTIVE_ID]
    );
    assert_eq!(
        state.active_objectives()[0].1.description,
        "Go to A1A Car Wash"
    );
    assert_eq!(
        objective_status(&state, OPENING_RESOLVE_OBJECTIVE_ID),
        ObjectiveStatus::Pending
    );

    let state = apply(&state, player_moved("a1a_car_wash")).state;
    assert_eq!(
        objective_status(&state, OPENING_REACH_OBJECTIVE_ID),
        ObjectiveStatus::Completed
    );
    assert_eq!(
        objective_status(&state, OPENING_RESOLVE_OBJECTIVE_ID),
        ObjectiveStatus::Active
    );
}

#[test]
fn inconsistent_world_bible_is_rejected() {
    let mut seed = bible();
    seed.opening_conflict
        .involved_character_ids
        .push("heisenberg".into());
    assert_eq!(
        build_initial(&seed),
        Err(NarrativeError::UnknownId {
            kind: IdKind::Character,
            id: "heisenberg".into()
        })
    );

    let mut seed = bible();
    seed.opening_conflict.location_id = "los_pollos".into();
    assert!(matches!(
        build_initial(&seed),
        Err(NarrativeError::UnknownId {
            kind: IdKind::Location,
            ..
        })
    ));

    // Two truths cannot share an id.
    let mut seed = bible();
    seed.important_conflicts[0].id = "world_rule_1".into();
    assert_eq!(
        build_initial(&seed),
        Err(NarrativeError::DuplicateId {
            kind: IdKind::Truth,
            id: "world_rule_1".into()
        })
    );
}
