//! The universe a runtime serves: one compiled WorldBible, read by the three
//! Phase 2 systems, plus an optional authored scenario.
//!
//! Loaded once at startup and immutable afterwards. Every session created
//! while it is configured starts from a copy of it.

use std::path::Path;

use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::director::{CanonError, WorldBibleView, WorldSummary};
use crate::narrative::{
    NarrativeEngine, NarrativeError, NarrativePlan, WorldBibleSeed, WorldFacts, build_initial,
};
use crate::npc::{Fact, FactId, NpcDirectory, NpcError};

/// Maximum number of secrets in a scenario.
pub const MAX_SECRETS: usize = 32;
/// Maximum number of keywords per secret.
pub const MAX_SECRET_KEYWORDS: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum WorldError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("world bible: {0}")]
    Canon(#[from] CanonError),
    #[error("{what} is not valid JSON for this build: {message}")]
    Shape { what: &'static str, message: String },
    #[error("narrative plan: {0}")]
    Narrative(#[from] NarrativeError),
    #[error("npc seed: {0}")]
    Npc(#[from] NpcError),
    #[error("invalid scenario: {0}")]
    Invalid(String),
}

/// Something the player can give away by talking.
///
/// This is the deterministic rule that turns a `speak` action into a
/// disclosure: the player tells an NPC the secret when the spoken `content`
/// contains one of `keywords` (case-insensitive). No model reads the text.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Secret {
    /// Shared by the NPC layer (`FactId`) and the narrative layer (`fact_id`).
    pub fact_id: String,
    /// What an NPC who is told learns, in plain words.
    pub statement: String,
    pub keywords: Vec<String>,
    /// The objective that asked the player to keep it, if any.
    #[serde(default)]
    pub objective_id: Option<String>,
}

impl Secret {
    pub(crate) fn fact(&self) -> Result<Fact, NpcError> {
        Fact::new(FactId::new(self.fact_id.clone())?, &self.statement)
    }

    pub(crate) fn matches(&self, content: &str) -> bool {
        let content = content.to_lowercase();
        self.keywords
            .iter()
            .any(|keyword| content.contains(&keyword.to_lowercase()))
    }
}

/// An authored story on top of a WorldBible: the narrative plan, the world it
/// starts in and the secrets the player can give away. Replaces the minimal
/// opening plan derived from the WorldBible alone.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub plan: NarrativePlan,
    pub world: WorldFacts,
    #[serde(default)]
    pub secrets: Vec<Secret>,
}

#[derive(Debug, Clone)]
pub struct RuntimeWorld {
    pub(crate) universe_id: String,
    /// The WorldBible as compiled; seeds the NPC layer.
    pub(crate) bible: Value,
    /// The bounded digest the Director reads.
    pub(crate) summary: WorldSummary,
    pub(crate) plan: NarrativePlan,
    pub(crate) facts: WorldFacts,
    pub(crate) secrets: Vec<Secret>,
}

impl RuntimeWorld {
    /// A world with the minimal opening plan derived from the WorldBible
    /// (`narrative::build_initial`) and no secrets.
    pub fn from_world_bible(text: &str) -> Result<Self, WorldError> {
        let view = WorldBibleView::from_json_str(text)?;
        let shape = |e: serde_json::Error| WorldError::Shape {
            what: "world bible",
            message: e.to_string(),
        };
        let bible: Value = serde_json::from_str(text).map_err(shape)?;
        let seed: WorldBibleSeed = serde_json::from_value(bible.clone()).map_err(shape)?;
        let (plan, facts) = build_initial(&seed)?;
        let world = Self {
            universe_id: view.universe.universe_id.clone(),
            summary: WorldSummary::from_world_bible(&view),
            bible,
            plan,
            facts,
            secrets: Vec::new(),
        };
        world.check()?;
        Ok(world)
    }

    /// Replace the derived opening plan with an authored scenario.
    pub fn with_scenario(mut self, scenario: Scenario) -> Result<Self, WorldError> {
        self.plan = scenario.plan;
        self.facts = scenario.world;
        self.secrets = scenario.secrets;
        self.check()?;
        Ok(self)
    }

    /// [`RuntimeWorld::with_scenario`] from scenario JSON.
    pub fn with_scenario_json(self, text: &str) -> Result<Self, WorldError> {
        let scenario = serde_json::from_str(text).map_err(|e| WorldError::Shape {
            what: "scenario",
            message: e.to_string(),
        })?;
        self.with_scenario(scenario)
    }

    /// Load a WorldBible (`cache/universes/<id>.json`) and, optionally, a
    /// scenario from disk.
    pub fn from_files(bible: &Path, scenario: Option<&Path>) -> Result<Self, WorldError> {
        let read = |path: &Path| {
            std::fs::read_to_string(path).map_err(|source| WorldError::Io {
                path: path.display().to_string(),
                source,
            })
        };
        let world = Self::from_world_bible(&read(bible)?)?;
        match scenario {
            Some(path) => world.with_scenario_json(&read(path)?),
            None => Ok(world),
        }
    }

    pub fn universe_id(&self) -> &str {
        &self.universe_id
    }

    pub fn secrets(&self) -> &[Secret] {
        &self.secrets
    }

    /// Everything a session start will do, done once up front so a bad world
    /// is refused at load instead of failing every `create_session`.
    fn check(&self) -> Result<(), WorldError> {
        let invalid = |message: String| Err(WorldError::Invalid(message));

        if self.plan.universe_id != self.universe_id {
            return invalid(format!(
                "plan is for universe {:?}, world bible is {:?}",
                self.plan.universe_id, self.universe_id
            ));
        }
        NarrativeEngine::start(self.plan.clone(), self.facts.clone())?;
        NpcDirectory::new().seed_from_world_bible(Uuid::nil(), &self.bible, Utc::now())?;

        if self.secrets.len() > MAX_SECRETS {
            return invalid(format!("more than {MAX_SECRETS} secrets"));
        }
        for secret in &self.secrets {
            secret.fact()?;
            let keywords = &secret.keywords;
            if keywords.is_empty() || keywords.len() > MAX_SECRET_KEYWORDS {
                return invalid(format!(
                    "secret {:?} needs 1..={MAX_SECRET_KEYWORDS} keywords",
                    secret.fact_id
                ));
            }
            // A short keyword would turn small talk into a confession.
            if keywords.iter().any(|k| k.trim().chars().count() < 4) {
                return invalid(format!(
                    "secret {:?} has a keyword shorter than 4 characters",
                    secret.fact_id
                ));
            }
            if let Some(objective_id) = &secret.objective_id
                && self.plan.objective(objective_id).is_none()
            {
                return invalid(format!(
                    "secret {:?} names unknown objective {objective_id:?}",
                    secret.fact_id
                ));
            }
        }
        Ok(())
    }
}
