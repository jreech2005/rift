//! Prerequisites: declarative conditions over the plan and the world.
//!
//! A [`Condition`] is data, never code. Evaluating one gives a three-valued
//! [`Verdict`], and the distinction between `Unmet` and `Broken` is what the
//! whole layer runs on: `Unmet` can still come true, `Broken` never will.
//! There is no negation, so every `Broken` verdict is permanent.

use serde::{Deserialize, Serialize};

use super::model::{
    ActorId, CanonPolicy, CharacterId, CheckpointId, CheckpointStatus, FactId, FlagId, LocationId,
    MissionId, MissionStatus, NarrativePlan, ObjectId, ObjectiveId, ObjectiveStatus, TruthId,
};
use super::world::{CharacterStatus, RelationshipAxis, WorldFacts};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Condition {
    FlagIs {
        flag: FlagId,
        value: bool,
    },
    CharacterAlive {
        character_id: CharacterId,
    },
    CharacterDead {
        character_id: CharacterId,
    },
    /// Alive and not out of action.
    CharacterAvailable {
        character_id: CharacterId,
    },
    CharacterAt {
        character_id: CharacterId,
        location_id: LocationId,
    },
    PlayerAt {
        location_id: LocationId,
    },
    LocationAvailable {
        location_id: LocationId,
    },
    ObjectIntact {
        object_id: ObjectId,
    },
    ObjectiveIs {
        objective_id: ObjectiveId,
        status: ObjectiveStatus,
    },
    MissionIs {
        mission_id: MissionId,
        status: MissionStatus,
    },
    CheckpointReached {
        checkpoint_id: CheckpointId,
    },
    FactKnown {
        fact_id: FactId,
        by: ActorId,
    },
    /// `from` has not learned the fact. Exposure cannot be undone.
    FactSecret {
        fact_id: FactId,
        from: ActorId,
    },
    RelationshipAtLeast {
        from: ActorId,
        to: ActorId,
        axis: RelationshipAxis,
        value: i32,
    },
    RelationshipAtMost {
        from: ActorId,
        to: ActorId,
        axis: RelationshipAxis,
        value: i32,
    },
    TruthHolds {
        truth_id: TruthId,
    },
    All {
        conditions: Vec<Condition>,
    },
    Any {
        conditions: Vec<Condition>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Necessity {
    /// Without it the mission, objective or beat cannot happen at all.
    #[default]
    Essential,
    /// Preferred circumstance. If it is permanently lost it is waived and the
    /// beat goes ahead in adapted form.
    Flexible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prerequisite {
    pub condition: Condition,
    #[serde(default)]
    pub necessity: Necessity,
}

impl Prerequisite {
    pub fn essential(condition: Condition) -> Self {
        Self {
            condition,
            necessity: Necessity::Essential,
        }
    }

    pub fn flexible(condition: Condition) -> Self {
        Self {
            condition,
            necessity: Necessity::Flexible,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// True now.
    Holds,
    /// False now, but it can still become true.
    Unmet,
    /// It can never become true.
    Broken,
}

/// Something the story depended on that is permanently gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LostDependency {
    CharacterDead {
        character_id: CharacterId,
    },
    ObjectDestroyed {
        object_id: ObjectId,
    },
    LocationLost {
        location_id: LocationId,
    },
    FactExposed {
        fact_id: FactId,
        known_by: ActorId,
    },
    /// The objective ended in `status`, not the one that was needed.
    ObjectiveUnreachable {
        objective_id: ObjectiveId,
        status: ObjectiveStatus,
    },
    MissionUnreachable {
        mission_id: MissionId,
        status: MissionStatus,
    },
    /// The beat was skipped or can no longer happen as written.
    CheckpointUnreachable {
        checkpoint_id: CheckpointId,
    },
    TruthEnded {
        truth_id: TruthId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub verdict: Verdict,
    /// Why the condition is `Broken`; empty otherwise.
    pub lost: Vec<LostDependency>,
}

impl Evaluation {
    fn met(holds: bool) -> Self {
        Self {
            verdict: if holds {
                Verdict::Holds
            } else {
                Verdict::Unmet
            },
            lost: Vec::new(),
        }
    }

    fn broken(lost: LostDependency) -> Self {
        Self {
            verdict: Verdict::Broken,
            lost: vec![lost],
        }
    }
}

fn dead(character_id: &str) -> Evaluation {
    Evaluation::broken(LostDependency::CharacterDead {
        character_id: character_id.to_owned(),
    })
}

/// Evaluate one condition. Ids are expected to resolve (plans are validated
/// before they run); an id that does not is simply `Unmet`.
pub fn evaluate(condition: &Condition, plan: &NarrativePlan, world: &WorldFacts) -> Evaluation {
    match condition {
        Condition::FlagIs { flag, value } => Evaluation::met(world.flag(flag) == *value),

        Condition::CharacterAlive { character_id } => match world.character_status(character_id) {
            Some(CharacterStatus::Dead) => dead(character_id),
            status => Evaluation::met(status.is_some()),
        },
        Condition::CharacterDead { character_id } => Evaluation::met(world.is_dead(character_id)),
        Condition::CharacterAvailable { character_id } => {
            match world.character_status(character_id) {
                Some(CharacterStatus::Dead) => dead(character_id),
                status => Evaluation::met(status == Some(CharacterStatus::Available)),
            }
        }
        Condition::CharacterAt {
            character_id,
            location_id,
        } => {
            if world.is_dead(character_id) {
                dead(character_id)
            } else if world.location_lost(location_id) {
                lost_location(location_id)
            } else {
                Evaluation::met(
                    world
                        .characters
                        .get(character_id)
                        .is_some_and(|c| c.location.as_deref() == Some(location_id)),
                )
            }
        }

        Condition::PlayerAt { location_id } => {
            if world.location_lost(location_id) {
                lost_location(location_id)
            } else {
                Evaluation::met(world.player_location.as_deref() == Some(location_id))
            }
        }
        Condition::LocationAvailable { location_id } => {
            if world.location_lost(location_id) {
                lost_location(location_id)
            } else {
                Evaluation::met(world.locations.contains(location_id))
            }
        }
        Condition::ObjectIntact { object_id } => {
            if world.object_destroyed(object_id) {
                Evaluation::broken(LostDependency::ObjectDestroyed {
                    object_id: object_id.clone(),
                })
            } else {
                Evaluation::met(world.objects.contains(object_id))
            }
        }

        Condition::ObjectiveIs {
            objective_id,
            status,
        } => match plan.objective(objective_id) {
            Some((_, objective)) if !objective.status.can_become(*status) => {
                Evaluation::broken(LostDependency::ObjectiveUnreachable {
                    objective_id: objective_id.clone(),
                    status: objective.status,
                })
            }
            found => Evaluation::met(found.is_some_and(|(_, o)| o.status == *status)),
        },
        Condition::MissionIs { mission_id, status } => match plan.mission(mission_id) {
            Some(mission) if !mission.status.can_become(*status) => {
                Evaluation::broken(LostDependency::MissionUnreachable {
                    mission_id: mission_id.clone(),
                    status: mission.status,
                })
            }
            found => Evaluation::met(found.is_some_and(|m| m.status == *status)),
        },
        Condition::CheckpointReached { checkpoint_id } => match plan.checkpoint(checkpoint_id) {
            Some(checkpoint)
                if checkpoint.status == CheckpointStatus::Skipped
                    || (checkpoint.status == CheckpointStatus::Pending
                        && matches!(
                            checkpoint.policy,
                            CanonPolicy::Replace | CanonPolicy::Delete
                        )) =>
            {
                Evaluation::broken(LostDependency::CheckpointUnreachable {
                    checkpoint_id: checkpoint_id.clone(),
                })
            }
            found => Evaluation::met(found.is_some_and(|c| c.status == CheckpointStatus::Reached)),
        },

        Condition::FactKnown { fact_id, by } => {
            if world.knows(fact_id, by) {
                Evaluation::met(true)
            } else if world.is_dead(by) {
                dead(by)
            } else {
                Evaluation::met(false)
            }
        }
        Condition::FactSecret { fact_id, from } => {
            if world.knows(fact_id, from) {
                Evaluation::broken(LostDependency::FactExposed {
                    fact_id: fact_id.clone(),
                    known_by: from.clone(),
                })
            } else {
                Evaluation::met(true)
            }
        }

        Condition::RelationshipAtLeast {
            from,
            to,
            axis,
            value,
        } => relationship(world, from, to, |facts| facts.get(*axis) >= *value),
        Condition::RelationshipAtMost {
            from,
            to,
            axis,
            value,
        } => relationship(world, from, to, |facts| facts.get(*axis) <= *value),

        Condition::TruthHolds { truth_id } => match world.truth_holds(truth_id) {
            Some(false) => Evaluation::broken(LostDependency::TruthEnded {
                truth_id: truth_id.clone(),
            }),
            holds => Evaluation::met(holds == Some(true)),
        },

        Condition::All { conditions } => {
            let parts: Vec<Evaluation> = conditions
                .iter()
                .map(|c| evaluate(c, plan, world))
                .collect();
            if parts.iter().any(|e| e.verdict == Verdict::Broken) {
                Evaluation {
                    verdict: Verdict::Broken,
                    lost: merge(parts.into_iter().filter(|e| e.verdict == Verdict::Broken)),
                }
            } else {
                Evaluation::met(parts.iter().all(|e| e.verdict == Verdict::Holds))
            }
        }
        Condition::Any { conditions } => {
            let parts: Vec<Evaluation> = conditions
                .iter()
                .map(|c| evaluate(c, plan, world))
                .collect();
            if !parts.is_empty() && parts.iter().all(|e| e.verdict == Verdict::Broken) {
                Evaluation {
                    verdict: Verdict::Broken,
                    lost: merge(parts.into_iter()),
                }
            } else {
                Evaluation::met(parts.iter().any(|e| e.verdict == Verdict::Holds))
            }
        }
    }
}

fn lost_location(location_id: &str) -> Evaluation {
    Evaluation::broken(LostDependency::LocationLost {
        location_id: location_id.to_owned(),
    })
}

fn relationship(
    world: &WorldFacts,
    from: &str,
    to: &str,
    test: impl Fn(super::world::RelationshipFacts) -> bool,
) -> Evaluation {
    // A threshold with someone who has died can never be worked toward.
    for party in [from, to] {
        if world.is_dead(party) {
            return dead(party);
        }
    }
    Evaluation::met(test(world.relationship(from, to)))
}

fn merge(parts: impl Iterator<Item = Evaluation>) -> Vec<LostDependency> {
    let mut lost = Vec::new();
    for part in parts {
        push_unique(&mut lost, part.lost);
    }
    lost
}

pub(crate) fn push_unique(into: &mut Vec<LostDependency>, more: Vec<LostDependency>) {
    for item in more {
        if !into.contains(&item) {
            into.push(item);
        }
    }
}

/// The combined standing of a prerequisite list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrerequisiteStatus {
    /// Every prerequisite holds, or is flexible and waived.
    pub satisfied: bool,
    /// An essential prerequisite is broken.
    pub impossible: bool,
    /// A flexible prerequisite is broken and has been waived.
    pub adapted: bool,
    pub lost_essential: Vec<LostDependency>,
    pub lost_flexible: Vec<LostDependency>,
}

pub fn assess(
    prerequisites: &[Prerequisite],
    plan: &NarrativePlan,
    world: &WorldFacts,
) -> PrerequisiteStatus {
    let mut status = PrerequisiteStatus {
        satisfied: true,
        ..PrerequisiteStatus::default()
    };
    for prerequisite in prerequisites {
        let evaluation = evaluate(&prerequisite.condition, plan, world);
        match (evaluation.verdict, prerequisite.necessity) {
            (Verdict::Holds, _) => {}
            (Verdict::Unmet, _) => status.satisfied = false,
            (Verdict::Broken, Necessity::Essential) => {
                status.satisfied = false;
                status.impossible = true;
                push_unique(&mut status.lost_essential, evaluation.lost);
            }
            (Verdict::Broken, Necessity::Flexible) => {
                status.adapted = true;
                push_unique(&mut status.lost_flexible, evaluation.lost);
            }
        }
    }
    status
}
