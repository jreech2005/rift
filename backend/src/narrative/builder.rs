//! Derive a minimal opening plan from a WorldBible.
//!
//! One opening conflict becomes one mission. This is deliberately not a
//! campaign generator.

use serde::Deserialize;

use super::condition::{Condition, Prerequisite};
use super::error::{IdKind, NarrativeError};
use super::model::{
    CanonRelation, FlagId, Importance, Mission, MissionStatus, NarrativeCheckpoint, NarrativePlan,
    Objective, ObjectiveStatus,
};
use super::validate::validate;
use super::world::WorldFacts;

pub const OPENING_MISSION_ID: &str = "opening_conflict";
pub const OPENING_REACH_OBJECTIVE_ID: &str = "opening_conflict.reach_scene";
pub const OPENING_RESOLVE_OBJECTIVE_ID: &str = "opening_conflict.resolve";
pub const OPENING_CHECKPOINT_ID: &str = "opening_conflict_resolved";

/// The flag gameplay sets when the player commits to the `n`th decision
/// option of the opening conflict (1-based).
pub fn opening_choice_flag(n: usize) -> FlagId {
    format!("opening_conflict.choice_{n}")
}

/// The part of WorldBible V1 the narrative layer reads. Unknown fields are
/// ignored, so a cached `cache/universes/<id>.json` deserializes as it is.
#[derive(Debug, Clone, Deserialize)]
pub struct WorldBibleSeed {
    pub universe: SeedUniverse,
    #[serde(default)]
    pub world_rules: Vec<SeedClaim>,
    pub locations: Vec<SeedLocation>,
    pub characters: Vec<SeedCharacter>,
    #[serde(default)]
    pub important_conflicts: Vec<SeedConflict>,
    pub starting_location: SeedStartingLocation,
    pub opening_conflict: SeedOpeningConflict,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SeedUniverse {
    pub universe_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SeedClaim {
    pub text: String,
    pub classification: CanonRelation,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SeedLocation {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SeedCharacter {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SeedConflict {
    pub id: String,
    pub name: String,
    pub description: String,
    pub classification: CanonRelation,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SeedStartingLocation {
    pub location_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SeedOpeningConflict {
    pub title: String,
    pub summary: String,
    pub location_id: String,
    pub involved_character_ids: Vec<String>,
    pub immediate_goal: String,
    #[serde(default)]
    pub decision_options: Vec<String>,
    pub stakes: String,
}

/// Build the opening plan and the world it starts in. The result is validated;
/// hand it to [`NarrativeEngine::start`].
///
/// * World rules and important conflicts become world truths: they hold
///   whatever the player does with the opening.
/// * The opening conflict becomes one mission. It needs its location and all
///   of its characters; losing any of them invalidates it.
///
/// [`NarrativeEngine::start`]: super::NarrativeEngine::start
pub fn build_initial(
    bible: &WorldBibleSeed,
) -> Result<(NarrativePlan, WorldFacts), NarrativeError> {
    let opening = &bible.opening_conflict;
    let start = &bible.starting_location.location_id;

    let mut world = WorldFacts::default();
    for location in &bible.locations {
        world.add_location(location.id.clone());
    }
    for character in &bible.characters {
        let on_scene = opening.involved_character_ids.contains(&character.id);
        world.add_character(
            character.id.clone(),
            on_scene.then_some(opening.location_id.as_str()),
        );
    }
    world.player_location = Some(start.clone());
    for (index, rule) in bible.world_rules.iter().enumerate() {
        add_truth(
            &mut world,
            format!("world_rule_{}", index + 1),
            rule.text.clone(),
            rule.classification,
        )?;
    }
    for conflict in &bible.important_conflicts {
        add_truth(
            &mut world,
            conflict.id.clone(),
            format!("{}: {}", conflict.name, conflict.description),
            conflict.classification,
        )?;
    }

    let mut objectives = Vec::new();
    let mut resolve = Objective::new(OPENING_RESOLVE_OBJECTIVE_ID, opening.immediate_goal.clone());
    if *start != opening.location_id {
        let scene = bible
            .locations
            .iter()
            .find(|l| l.id == opening.location_id)
            .map_or(opening.location_id.as_str(), |l| l.name.as_str());
        objectives.push(Objective {
            completes_when: Some(Condition::PlayerAt {
                location_id: opening.location_id.clone(),
            }),
            ..Objective::new(OPENING_REACH_OBJECTIVE_ID, format!("Go to {scene}"))
        });
        resolve
            .prerequisites
            .push(Prerequisite::essential(Condition::ObjectiveIs {
                objective_id: OPENING_REACH_OBJECTIVE_ID.to_owned(),
                status: ObjectiveStatus::Completed,
            }));
    }
    // Committing to any decision option resolves the conflict. With no options
    // listed, only an explicit `objective_completed` does.
    if !opening.decision_options.is_empty() {
        resolve.completes_when = Some(Condition::Any {
            conditions: (1..=opening.decision_options.len())
                .map(|n| Condition::FlagIs {
                    flag: opening_choice_flag(n),
                    value: true,
                })
                .collect(),
        });
    }
    objectives.push(resolve);

    let mut mission = Mission::new(OPENING_MISSION_ID, opening.title.clone(), objectives);
    mission.description = opening.summary.clone();
    mission.importance = Importance::Critical;
    mission.related_characters = opening.involved_character_ids.clone();
    mission.related_locations = vec![opening.location_id.clone()];
    mission
        .prerequisites
        .push(Prerequisite::essential(Condition::LocationAvailable {
            location_id: opening.location_id.clone(),
        }));
    for character_id in &opening.involved_character_ids {
        mission
            .prerequisites
            .push(Prerequisite::essential(Condition::CharacterAlive {
                character_id: character_id.clone(),
            }));
    }
    mission.metadata.insert(
        "source".to_owned(),
        "world_bible.opening_conflict".to_owned(),
    );
    mission
        .metadata
        .insert("stakes".to_owned(), opening.stakes.clone());
    for (index, option) in opening.decision_options.iter().enumerate() {
        mission
            .metadata
            .insert(format!("decision_option_{}", index + 1), option.clone());
    }

    let mut plan = NarrativePlan::new(
        format!("{}.opening", bible.universe.universe_id),
        bible.universe.universe_id.clone(),
    );
    plan.missions.push(mission);
    plan.checkpoints.push(NarrativeCheckpoint {
        description: opening.stakes.clone(),
        reached_when: Some(Condition::MissionIs {
            mission_id: OPENING_MISSION_ID.to_owned(),
            status: MissionStatus::Completed,
        }),
        ..NarrativeCheckpoint::new(
            OPENING_CHECKPOINT_ID,
            format!("{} resolved", opening.title),
            CanonRelation::Generated,
            Importance::Critical,
        )
    });

    validate(&plan, &world)?;
    Ok((plan, world))
}

fn add_truth(
    world: &mut WorldFacts,
    truth_id: String,
    statement: String,
    canon_relation: CanonRelation,
) -> Result<(), NarrativeError> {
    if world.truths.contains_key(&truth_id) {
        return Err(NarrativeError::DuplicateId {
            kind: IdKind::Truth,
            id: truth_id,
        });
    }
    world.add_truth(truth_id, statement, canon_relation);
    Ok(())
}
