//! Structural validation of a plan against the world it will run in.
//!
//! Nothing runs until this passes: ids are well formed and unique, every
//! reference resolves, and no mission, objective or beat waits on itself.

use std::collections::{BTreeMap, BTreeSet};

use super::condition::{Condition, Prerequisite};
use super::error::{IdKind, NarrativeError};
use super::model::{Effect, NARRATIVE_SCHEMA_VERSION, NarrativePlan};
use super::world::WorldFacts;
use crate::action::MAX_IDENTIFIER_LEN;

/// Maximum length of plan ids, flag keys and fact ids, which are often
/// `prefix:entity_id` (e.g. the session flag `interacted:<target>`).
pub const MAX_KEY_LEN: usize = 128;

fn check(kind: IdKind, id: &str, max_len: usize) -> Result<(), NarrativeError> {
    let reason = if id.is_empty() {
        "must not be empty"
    } else if id.len() > max_len {
        "too long"
    } else if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
    {
        "may only contain [A-Za-z0-9_.:-]"
    } else {
        return Ok(());
    };
    Err(NarrativeError::InvalidIdentifier {
        kind,
        id: id.to_owned(),
        reason,
    })
}

/// An entity id: protocol V1 identifier, 1..=64 chars.
pub(crate) fn check_id(kind: IdKind, id: &str) -> Result<(), NarrativeError> {
    check(kind, id, MAX_IDENTIFIER_LEN)
}

/// A flag key, fact id or plan id: same charset, 1..=128 chars.
pub(crate) fn check_key(kind: IdKind, id: &str) -> Result<(), NarrativeError> {
    check(kind, id, MAX_KEY_LEN)
}

fn unknown(kind: IdKind, id: &str) -> NarrativeError {
    NarrativeError::UnknownId {
        kind,
        id: id.to_owned(),
    }
}

/// Validate `plan` and `world` together.
pub fn validate(plan: &NarrativePlan, world: &WorldFacts) -> Result<(), NarrativeError> {
    if plan.schema_version != NARRATIVE_SCHEMA_VERSION {
        return Err(NarrativeError::UnsupportedSchemaVersion(
            plan.schema_version,
        ));
    }
    check_key(IdKind::Plan, &plan.plan_id)?;
    check_key(IdKind::Universe, &plan.universe_id)?;

    validate_world(world)?;

    let mut missions = BTreeSet::new();
    let mut objectives = BTreeSet::new();
    let mut checkpoints = BTreeSet::new();
    for mission in &plan.missions {
        check_id(IdKind::Mission, &mission.mission_id)?;
        if !missions.insert(mission.mission_id.as_str()) {
            return Err(NarrativeError::DuplicateId {
                kind: IdKind::Mission,
                id: mission.mission_id.clone(),
            });
        }
        if mission.objectives.iter().all(|o| o.optional) {
            return Err(NarrativeError::InvalidPlan(format!(
                "mission {:?} needs at least one required objective",
                mission.mission_id
            )));
        }
        for objective in &mission.objectives {
            check_id(IdKind::Objective, &objective.objective_id)?;
            if !objectives.insert(objective.objective_id.as_str()) {
                return Err(NarrativeError::DuplicateId {
                    kind: IdKind::Objective,
                    id: objective.objective_id.clone(),
                });
            }
        }
    }
    for checkpoint in &plan.checkpoints {
        check_id(IdKind::Checkpoint, &checkpoint.checkpoint_id)?;
        if !checkpoints.insert(checkpoint.checkpoint_id.as_str()) {
            return Err(NarrativeError::DuplicateId {
                kind: IdKind::Checkpoint,
                id: checkpoint.checkpoint_id.clone(),
            });
        }
    }

    // Truths a condition may refer to: those in the world, and those the plan
    // itself establishes.
    let mut truths: BTreeSet<&str> = world.truths.keys().map(String::as_str).collect();
    for effect in plan_effects(plan) {
        if let Effect::EstablishTruth { truth_id, .. } = effect {
            truths.insert(truth_id.as_str());
        }
    }

    let refs = Refs {
        world,
        missions,
        objectives,
        checkpoints,
        truths,
    };
    for mission in &plan.missions {
        for character_id in &mission.related_characters {
            refs.character(character_id)?;
        }
        for location_id in &mission.related_locations {
            refs.location(location_id)?;
        }
        refs.prerequisites(&mission.prerequisites)?;
        refs.effects(&mission.success_effects)?;
        refs.effects(&mission.failure_effects)?;
        for objective in &mission.objectives {
            refs.prerequisites(&objective.prerequisites)?;
            for condition in objective.completes_when.iter().chain(&objective.fails_when) {
                refs.condition(condition)?;
            }
            refs.effects(&objective.success_effects)?;
            refs.effects(&objective.failure_effects)?;
        }
    }
    for checkpoint in &plan.checkpoints {
        refs.prerequisites(&checkpoint.prerequisites)?;
        if let Some(condition) = &checkpoint.reached_when {
            refs.condition(condition)?;
        }
    }

    match find_cycle(&dependency_graph(plan)) {
        Some(path) => Err(NarrativeError::DependencyCycle { path }),
        None => Ok(()),
    }
}

fn validate_world(world: &WorldFacts) -> Result<(), NarrativeError> {
    for location_id in &world.locations {
        check_id(IdKind::Location, location_id)?;
    }
    for location_id in &world.lost_locations {
        if !world.locations.contains(location_id) {
            return Err(unknown(IdKind::Location, location_id));
        }
    }
    for object_id in &world.objects {
        check_id(IdKind::Object, object_id)?;
    }
    for object_id in &world.destroyed_objects {
        if !world.objects.contains(object_id) {
            return Err(unknown(IdKind::Object, object_id));
        }
    }
    for (character_id, character) in &world.characters {
        check_id(IdKind::Character, character_id)?;
        if WorldFacts::is_player(character_id) {
            return Err(NarrativeError::InvalidIdentifier {
                kind: IdKind::Character,
                id: character_id.clone(),
                reason: "is reserved for the player",
            });
        }
        if let Some(location_id) = &character.location
            && !world.locations.contains(location_id)
        {
            return Err(unknown(IdKind::Location, location_id));
        }
    }
    if let Some(location_id) = &world.player_location
        && !world.locations.contains(location_id)
    {
        return Err(unknown(IdKind::Location, location_id));
    }
    for flag in world.flags.keys() {
        check_key(IdKind::Flag, flag)?;
    }
    for (fact_id, actors) in &world.known_facts {
        check_key(IdKind::Fact, fact_id)?;
        for actor in actors {
            check_id(IdKind::Actor, actor)?;
        }
    }
    for (from, others) in &world.relationships {
        check_id(IdKind::Actor, from)?;
        for to in others.keys() {
            check_id(IdKind::Actor, to)?;
        }
    }
    for truth_id in world.truths.keys() {
        check_id(IdKind::Truth, truth_id)?;
    }
    Ok(())
}

fn plan_effects(plan: &NarrativePlan) -> impl Iterator<Item = &Effect> {
    plan.missions.iter().flat_map(|mission| {
        mission
            .success_effects
            .iter()
            .chain(&mission.failure_effects)
            .chain(
                mission
                    .objectives
                    .iter()
                    .flat_map(|o| o.success_effects.iter().chain(&o.failure_effects)),
            )
    })
}

/// Everything a condition or effect may refer to.
struct Refs<'a> {
    world: &'a WorldFacts,
    missions: BTreeSet<&'a str>,
    objectives: BTreeSet<&'a str>,
    checkpoints: BTreeSet<&'a str>,
    truths: BTreeSet<&'a str>,
}

impl Refs<'_> {
    fn character(&self, id: &str) -> Result<(), NarrativeError> {
        check_id(IdKind::Character, id)?;
        if self.world.characters.contains_key(id) {
            Ok(())
        } else {
            Err(unknown(IdKind::Character, id))
        }
    }

    fn location(&self, id: &str) -> Result<(), NarrativeError> {
        check_id(IdKind::Location, id)?;
        if self.world.locations.contains(id) {
            Ok(())
        } else {
            Err(unknown(IdKind::Location, id))
        }
    }

    fn known(&self, kind: IdKind, set: &BTreeSet<&str>, id: &str) -> Result<(), NarrativeError> {
        check_id(kind, id)?;
        if set.contains(id) {
            Ok(())
        } else {
            Err(unknown(kind, id))
        }
    }

    fn prerequisites(&self, prerequisites: &[Prerequisite]) -> Result<(), NarrativeError> {
        prerequisites
            .iter()
            .try_for_each(|p| self.condition(&p.condition))
    }

    fn condition(&self, condition: &Condition) -> Result<(), NarrativeError> {
        match condition {
            Condition::FlagIs { flag, .. } => check_key(IdKind::Flag, flag),
            Condition::CharacterAlive { character_id }
            | Condition::CharacterDead { character_id }
            | Condition::CharacterAvailable { character_id } => self.character(character_id),
            Condition::CharacterAt {
                character_id,
                location_id,
            } => {
                self.character(character_id)?;
                self.location(location_id)
            }
            Condition::PlayerAt { location_id } | Condition::LocationAvailable { location_id } => {
                self.location(location_id)
            }
            Condition::ObjectIntact { object_id } => {
                check_id(IdKind::Object, object_id)?;
                if self.world.objects.contains(object_id) {
                    Ok(())
                } else {
                    Err(unknown(IdKind::Object, object_id))
                }
            }
            Condition::ObjectiveIs { objective_id, .. } => {
                self.known(IdKind::Objective, &self.objectives, objective_id)
            }
            Condition::MissionIs { mission_id, .. } => {
                self.known(IdKind::Mission, &self.missions, mission_id)
            }
            Condition::CheckpointReached { checkpoint_id } => {
                self.known(IdKind::Checkpoint, &self.checkpoints, checkpoint_id)
            }
            Condition::FactKnown { fact_id, by: actor }
            | Condition::FactSecret {
                fact_id,
                from: actor,
            } => {
                check_key(IdKind::Fact, fact_id)?;
                check_id(IdKind::Actor, actor)
            }
            Condition::RelationshipAtLeast { from, to, .. }
            | Condition::RelationshipAtMost { from, to, .. } => {
                check_id(IdKind::Actor, from)?;
                check_id(IdKind::Actor, to)
            }
            Condition::TruthHolds { truth_id } => self.known(IdKind::Truth, &self.truths, truth_id),
            Condition::All { conditions } | Condition::Any { conditions } => {
                if conditions.is_empty() {
                    return Err(NarrativeError::InvalidPlan(
                        "all/any needs at least one condition".to_owned(),
                    ));
                }
                conditions.iter().try_for_each(|c| self.condition(c))
            }
        }
    }

    fn effects(&self, effects: &[Effect]) -> Result<(), NarrativeError> {
        effects.iter().try_for_each(|effect| match effect {
            Effect::SetFlag { flag, .. } => check_key(IdKind::Flag, flag),
            Effect::RevealFact { fact_id, to } => {
                check_key(IdKind::Fact, fact_id)?;
                check_id(IdKind::Actor, to)
            }
            Effect::AdjustRelationship { from, to, .. } => {
                check_id(IdKind::Actor, from)?;
                check_id(IdKind::Actor, to)
            }
            Effect::EstablishTruth { truth_id, .. } => check_id(IdKind::Truth, truth_id),
            Effect::EndTruth { truth_id } => self.known(IdKind::Truth, &self.truths, truth_id),
        })
    }
}

// ---------------------------------------------------------------------------
// Dependency cycles
// ---------------------------------------------------------------------------

type Graph = BTreeMap<String, Vec<String>>;

/// Who waits on whom: a node points at every mission, objective or beat its
/// prerequisites (or completion / reach condition) refer to.
fn dependency_graph(plan: &NarrativePlan) -> Graph {
    let mut graph = Graph::new();
    for mission in &plan.missions {
        let edges = graph
            .entry(format!("mission:{}", mission.mission_id))
            .or_default();
        for prerequisite in &mission.prerequisites {
            references(&prerequisite.condition, edges);
        }
        for objective in &mission.objectives {
            let edges = graph
                .entry(format!("objective:{}", objective.objective_id))
                .or_default();
            for prerequisite in &objective.prerequisites {
                references(&prerequisite.condition, edges);
            }
            if let Some(condition) = &objective.completes_when {
                references(condition, edges);
            }
        }
    }
    for checkpoint in &plan.checkpoints {
        let edges = graph
            .entry(format!("checkpoint:{}", checkpoint.checkpoint_id))
            .or_default();
        for prerequisite in &checkpoint.prerequisites {
            references(&prerequisite.condition, edges);
        }
        if let Some(condition) = &checkpoint.reached_when {
            references(condition, edges);
        }
    }
    graph
}

fn references(condition: &Condition, out: &mut Vec<String>) {
    match condition {
        Condition::ObjectiveIs { objective_id, .. } => {
            out.push(format!("objective:{objective_id}"));
        }
        Condition::MissionIs { mission_id, .. } => out.push(format!("mission:{mission_id}")),
        Condition::CheckpointReached { checkpoint_id } => {
            out.push(format!("checkpoint:{checkpoint_id}"));
        }
        Condition::All { conditions } | Condition::Any { conditions } => {
            conditions.iter().for_each(|c| references(c, out));
        }
        _ => {}
    }
}

/// Depth-first search; returns the first cycle found, closed on itself.
fn find_cycle(graph: &Graph) -> Option<Vec<String>> {
    fn visit<'a>(
        node: &'a str,
        graph: &'a Graph,
        done: &mut BTreeSet<&'a str>,
        path: &mut Vec<&'a str>,
    ) -> Option<Vec<String>> {
        if let Some(start) = path.iter().position(|n| *n == node) {
            let mut cycle: Vec<String> = path[start..].iter().map(|n| (*n).to_owned()).collect();
            cycle.push(node.to_owned());
            return Some(cycle);
        }
        if !done.insert(node) {
            return None;
        }
        path.push(node);
        for next in graph.get(node).into_iter().flatten() {
            if let Some(cycle) = visit(next, graph, done, path) {
                return Some(cycle);
            }
        }
        path.pop();
        None
    }

    let mut done = BTreeSet::new();
    graph
        .keys()
        .find_map(|node| visit(node, graph, &mut done, &mut Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_follow_protocol_charset() {
        assert!(check_id(IdKind::Character, "walter_white").is_ok());
        assert!(check_id(IdKind::Flag, "interacted:test_door").is_ok());
        assert!(check_id(IdKind::Character, "").is_err());
        assert!(check_id(IdKind::Character, "maya 2").is_err());
        assert!(check_id(IdKind::Character, &"x".repeat(65)).is_err());
        assert!(check_key(IdKind::Flag, &"x".repeat(65)).is_ok());
        assert!(check_key(IdKind::Flag, &"x".repeat(129)).is_err());
    }

    #[test]
    fn finds_a_cycle_and_closes_it() {
        let mut graph = Graph::new();
        graph.insert("a".into(), vec!["b".into()]);
        graph.insert("b".into(), vec!["c".into()]);
        graph.insert("c".into(), vec!["a".into()]);
        graph.insert("d".into(), vec!["a".into()]);
        assert_eq!(
            find_cycle(&graph),
            Some(vec!["a".into(), "b".into(), "c".into(), "a".into()])
        );
    }

    #[test]
    fn a_shared_dependency_is_not_a_cycle() {
        let mut graph = Graph::new();
        graph.insert("a".into(), vec!["b".into(), "c".into()]);
        graph.insert("b".into(), vec!["d".into()]);
        graph.insert("c".into(), vec!["d".into()]);
        graph.insert("d".into(), vec![]);
        assert_eq!(find_cycle(&graph), None);
    }
}
