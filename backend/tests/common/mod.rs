//! Shared fixtures for the Director integration tests.
#![allow(dead_code)]

use rift_backend::director::{DirectorContext, WorldBibleView, WorldSummary};
use serde_json::Value;

/// The WorldBible compiled in Phase 1 (`cache/universes/breaking_bad_tv_1396.json`).
pub const WORLD_BIBLE: &str = include_str!("../fixtures/director/world_bible_breaking_bad.json");
/// Everything of the context except `world`: the player was asked to hide
/// Walter's phone and told Hank instead.
pub const SCENARIO: &str = include_str!("../fixtures/director/scenario_hank_disclosure.json");
/// A representative proposal for that scenario, as a model returns it.
pub const DECISION: &str = include_str!("../fixtures/director/decision_hank_disclosure.json");

/// The scenario on top of the digest of the real WorldBible.
pub fn hank_context() -> DirectorContext {
    let bible = WorldBibleView::from_json_str(WORLD_BIBLE).expect("world bible fixture");
    let mut value: Value = serde_json::from_str(SCENARIO).expect("scenario fixture");
    value["world"] = serde_json::to_value(WorldSummary::from_world_bible(&bible)).unwrap();
    serde_json::from_value(value).expect("scenario decodes as a DirectorContext")
}
