//! Bounded relationship state: how one NPC feels about another entity.
//!
//! Three dimensions only, all integers, all clamped. Updates are deterministic
//! saturating additions — no randomness, no decay, no LLM.

use serde::{Deserialize, Serialize};

/// Lower bound of `trust` and `affinity`.
pub const RELATIONSHIP_MIN: i32 = -100;
/// Upper bound of every dimension.
pub const RELATIONSHIP_MAX: i32 = 100;
/// `affinity` at or above this toward someone makes them an ally.
pub const ALLY_AFFINITY: i32 = 30;

/// How one NPC feels about one other entity.
///
/// * `trust`    — -100 (expects betrayal) ..= 100 (trusts completely)
/// * `fear`     —    0 (unafraid)         ..= 100 (terrified)
/// * `affinity` — -100 (hates)            ..= 100 (loves)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(from = "RawRelationship")]
pub struct Relationship {
    trust: i32,
    fear: i32,
    affinity: i32,
}

/// Unclamped wire form; clamped on the way in so stored data can never carry
/// out-of-range values.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRelationship {
    trust: i32,
    fear: i32,
    affinity: i32,
}

impl From<RawRelationship> for Relationship {
    fn from(raw: RawRelationship) -> Self {
        Self::new(raw.trust, raw.fear, raw.affinity)
    }
}

/// A signed change to a [`Relationship`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RelationshipDelta {
    pub trust: i32,
    pub fear: i32,
    pub affinity: i32,
}

impl RelationshipDelta {
    pub const fn new(trust: i32, fear: i32, affinity: i32) -> Self {
        Self {
            trust,
            fear,
            affinity,
        }
    }

    pub fn is_zero(&self) -> bool {
        *self == Self::default()
    }

    /// Halve the change (rounding toward zero). Used for second-hand news.
    pub fn halved(self) -> Self {
        Self::new(self.trust / 2, self.fear / 2, self.affinity / 2)
    }
}

/// Coarse summary of a relationship, derived deterministically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Hostile,
    Wary,
    Neutral,
    Friendly,
    Loyal,
}

impl Relationship {
    /// Build a relationship, clamping each dimension into its range.
    pub fn new(trust: i32, fear: i32, affinity: i32) -> Self {
        Self {
            trust: trust.clamp(RELATIONSHIP_MIN, RELATIONSHIP_MAX),
            fear: fear.clamp(0, RELATIONSHIP_MAX),
            affinity: affinity.clamp(RELATIONSHIP_MIN, RELATIONSHIP_MAX),
        }
    }

    pub fn trust(&self) -> i32 {
        self.trust
    }

    pub fn fear(&self) -> i32 {
        self.fear
    }

    pub fn affinity(&self) -> i32 {
        self.affinity
    }

    /// Apply a change, saturating at the bounds.
    pub fn apply(&mut self, delta: RelationshipDelta) {
        *self = Self::new(
            self.trust.saturating_add(delta.trust),
            self.fear.saturating_add(delta.fear),
            self.affinity.saturating_add(delta.affinity),
        );
    }

    pub fn is_ally(&self) -> bool {
        self.affinity >= ALLY_AFFINITY
    }

    /// `trust + affinity - fear`, bucketed.
    pub fn disposition(&self) -> Disposition {
        match self.trust + self.affinity - self.fear {
            i32::MIN..=-60 => Disposition::Hostile,
            -59..=-15 => Disposition::Wary,
            -14..=14 => Disposition::Neutral,
            15..=99 => Disposition::Friendly,
            _ => Disposition::Loyal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_neutral() {
        let r = Relationship::default();
        assert_eq!((r.trust(), r.fear(), r.affinity()), (0, 0, 0));
        assert_eq!(r.disposition(), Disposition::Neutral);
        assert!(!r.is_ally());
    }

    #[test]
    fn updates_are_deterministic_and_bounded() {
        let mut a = Relationship::default();
        let mut b = Relationship::default();
        for r in [&mut a, &mut b] {
            r.apply(RelationshipDelta::new(15, -5, 20));
            r.apply(RelationshipDelta::new(-40, 30, -10));
        }
        assert_eq!(a, b);
        assert_eq!((a.trust(), a.fear(), a.affinity()), (-25, 30, 10));

        for _ in 0..50 {
            a.apply(RelationshipDelta::new(-35, 30, -35));
        }
        assert_eq!((a.trust(), a.fear(), a.affinity()), (-100, 100, -100));
        a.apply(RelationshipDelta::new(i32::MAX, i32::MIN, i32::MAX));
        assert_eq!((a.trust(), a.fear(), a.affinity()), (100, 0, 100));
    }

    #[test]
    fn disposition_buckets() {
        assert_eq!(
            Relationship::new(-40, 30, -30).disposition(),
            Disposition::Hostile
        );
        assert_eq!(
            Relationship::new(-10, 10, 0).disposition(),
            Disposition::Wary
        );
        assert_eq!(
            Relationship::new(5, 0, 5).disposition(),
            Disposition::Neutral
        );
        assert_eq!(
            Relationship::new(20, 0, 20).disposition(),
            Disposition::Friendly
        );
        assert_eq!(
            Relationship::new(60, 0, 60).disposition(),
            Disposition::Loyal
        );
    }

    #[test]
    fn deserialization_clamps_and_rejects_unknown_dimensions() {
        let r: Relationship =
            serde_json::from_str(r#"{"trust": 900, "fear": -5, "affinity": -900}"#).unwrap();
        assert_eq!((r.trust(), r.fear(), r.affinity()), (100, 0, -100));
        assert!(
            serde_json::from_str::<Relationship>(
                r#"{"trust": 0, "fear": 0, "affinity": 0, "jealousy": 3}"#
            )
            .is_err()
        );
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<Relationship>(&json).unwrap(), r);
    }

    #[test]
    fn halved_delta_rounds_toward_zero() {
        assert_eq!(
            RelationshipDelta::new(-25, 15, -30).halved(),
            RelationshipDelta::new(-12, 7, -15)
        );
    }
}
