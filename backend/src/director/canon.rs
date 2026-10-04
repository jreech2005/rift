//! A read-only view of WorldBible V1 (`shared/schemas/universe/v1/`).
//!
//! Only the fields the Director needs are declared and unknown fields are
//! ignored, so additive WorldBible changes do not break the backend. The
//! Python universe compiler stays the source of truth and has already
//! validated the file; this reader checks the version and the shape it uses.

use serde::Deserialize;
use serde_json::Value;

/// WorldBible schema version this reader understands.
pub const WORLD_BIBLE_SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CanonError {
    #[error("world bible is not valid JSON: {0}")]
    Malformed(String),
    #[error("world bible schema_version {0} is not supported")]
    UnsupportedVersion(String),
    #[error("world bible does not match WorldBible V1: {0}")]
    Shape(String),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ClaimView {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UniverseView {
    pub universe_id: String,
    pub title: String,
    pub era: ClaimView,
    pub setting: ClaimView,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LocationView {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CharacterView {
    pub id: String,
    pub name: String,
    pub role: String,
    pub status: String,
    #[serde(default)]
    pub personality_traits: Vec<String>,
    #[serde(default)]
    pub goals: Vec<ClaimView>,
    /// `canon`, `inferred` or `generated`.
    #[serde(default)]
    pub classification: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FactionView {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub member_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PlayerRoleView {
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StartingLocationView {
    pub location_id: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct OpeningConflictView {
    pub title: String,
    pub summary: String,
    pub location_id: String,
    #[serde(default)]
    pub involved_character_ids: Vec<String>,
    pub immediate_goal: String,
    pub stakes: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TimelineView {
    #[serde(default)]
    pub canon_cutoff: Option<String>,
    pub summary: String,
}

/// The parts of a WorldBible V1 the Director reads.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WorldBibleView {
    pub universe: UniverseView,
    #[serde(default)]
    pub world_rules: Vec<ClaimView>,
    pub locations: Vec<LocationView>,
    pub characters: Vec<CharacterView>,
    #[serde(default)]
    pub factions: Vec<FactionView>,
    pub player_role: PlayerRoleView,
    pub starting_location: StartingLocationView,
    pub opening_conflict: OpeningConflictView,
    pub timeline_context: TimelineView,
}

impl WorldBibleView {
    /// Parse a cached WorldBible (`cache/universes/<universe_id>.json`).
    pub fn from_json_str(text: &str) -> Result<Self, CanonError> {
        let value: Value =
            serde_json::from_str(text).map_err(|e| CanonError::Malformed(e.to_string()))?;
        match value.get("schema_version") {
            Some(v) if v.as_u64() == Some(WORLD_BIBLE_SCHEMA_VERSION) => {}
            Some(other) => return Err(CanonError::UnsupportedVersion(other.to_string())),
            None => return Err(CanonError::UnsupportedVersion("<missing>".into())),
        }
        serde_json::from_value(value).map_err(|e| CanonError::Shape(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::director::testing::BREAKING_BAD_WORLD_BIBLE;

    #[test]
    fn reads_the_compiled_world_bible() {
        let bible = WorldBibleView::from_json_str(BREAKING_BAD_WORLD_BIBLE).unwrap();
        assert_eq!(bible.universe.universe_id, "breaking_bad_tv_1396");
        assert_eq!(bible.starting_location.location_id, "albuquerque_hospital");
        let ids: Vec<&str> = bible.characters.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["walter_white", "jesse_pinkman", "hank_schrader"]);
        assert_eq!(bible.characters[0].classification, "canon");
        assert!(!bible.opening_conflict.immediate_goal.is_empty());
    }

    #[test]
    fn rejects_other_versions_and_garbage() {
        assert!(matches!(
            WorldBibleView::from_json_str("{not json"),
            Err(CanonError::Malformed(_))
        ));
        assert!(matches!(
            WorldBibleView::from_json_str(r#"{"schema_version": 2}"#),
            Err(CanonError::UnsupportedVersion(_))
        ));
        assert!(matches!(
            WorldBibleView::from_json_str(r#"{"universe": {}}"#),
            Err(CanonError::UnsupportedVersion(_))
        ));
        assert!(matches!(
            WorldBibleView::from_json_str(r#"{"schema_version": 1, "universe": {}}"#),
            Err(CanonError::Shape(_))
        ));
    }
}
