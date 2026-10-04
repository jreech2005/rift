//! Canon adaptation: what becomes of a planned beat once the world has moved.
//!
//! A beat is preserved because its prerequisites are still valid, never
//! because the source material says it happened. Canon gravity only decides
//! how hard the story holds on to a beat's *place* once the beat itself is
//! impossible, and which valid beat comes first.

use serde::{Deserialize, Serialize};

use super::condition::{LostDependency, Verdict, assess, evaluate, push_unique};
use super::model::{
    CanonPolicy, CanonRelation, CheckpointId, Importance, NarrativeCheckpoint, NarrativePlan,
};
use super::world::WorldFacts;

/// An impossible beat with at least this much gravity is replaced, not deleted.
pub const REPLACE_MIN_GRAVITY: u8 = 3;

/// How strongly the story is pulled toward a beat, 0..=6: importance
/// (minor 0, major 2, critical 4) plus closeness to canon (generated 0,
/// inferred 1, canon 2).
pub fn gravity(importance: Importance, canon_relation: CanonRelation) -> u8 {
    let weight = match importance {
        Importance::Minor => 0,
        Importance::Major => 2,
        Importance::Critical => 4,
    };
    let canon = match canon_relation {
        CanonRelation::Generated => 0,
        CanonRelation::Inferred => 1,
        CanonRelation::Canon => 2,
    };
    weight + canon
}

/// The classification of one beat against the current world.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeatAssessment {
    pub checkpoint_id: CheckpointId,
    pub policy: CanonPolicy,
    pub gravity: u8,
    /// Every prerequisite holds right now (flexible losses waived).
    pub ready: bool,
    /// What the beat has permanently lost; empty when preserved.
    pub lost: Vec<LostDependency>,
}

pub fn assess_checkpoint(
    checkpoint: &NarrativeCheckpoint,
    plan: &NarrativePlan,
    world: &WorldFacts,
) -> BeatAssessment {
    let prerequisites = assess(&checkpoint.prerequisites, plan, world);
    let mut lost = prerequisites.lost_essential;
    let mut impossible = prerequisites.impossible;

    // A beat that is reached through a condition is impossible once that
    // condition is.
    if let Some(condition) = &checkpoint.reached_when {
        let reach = evaluate(condition, plan, world);
        if reach.verdict == Verdict::Broken {
            impossible = true;
            push_unique(&mut lost, reach.lost);
        }
    }

    let gravity = gravity(checkpoint.importance, checkpoint.canon_relation);
    let policy = if impossible {
        // If the truth underneath the beat has ended, the beat is moot and
        // nothing needs to stand in for it, whatever its gravity.
        let moot = lost
            .iter()
            .any(|l| matches!(l, LostDependency::TruthEnded { .. }));
        if !moot && gravity >= REPLACE_MIN_GRAVITY {
            CanonPolicy::Replace
        } else {
            CanonPolicy::Delete
        }
    } else if prerequisites.adapted {
        CanonPolicy::Adapt
    } else {
        CanonPolicy::Preserve
    };
    push_unique(&mut lost, prerequisites.lost_flexible);

    BeatAssessment {
        checkpoint_id: checkpoint.checkpoint_id.clone(),
        policy,
        gravity,
        ready: !impossible && prerequisites.satisfied,
        lost,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gravity_orders_importance_then_canon() {
        use CanonRelation::{Canon, Generated, Inferred};
        use Importance::{Critical, Major, Minor};

        assert_eq!(gravity(Minor, Generated), 0);
        assert_eq!(gravity(Minor, Canon), 2);
        assert_eq!(gravity(Major, Generated), 2);
        assert_eq!(gravity(Major, Inferred), 3);
        assert_eq!(gravity(Critical, Generated), 4);
        assert_eq!(gravity(Critical, Canon), 6);
        assert!(gravity(Major, Canon) > gravity(Major, Inferred));
        assert!(gravity(Critical, Generated) > gravity(Major, Inferred));
    }
}
