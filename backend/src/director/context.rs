//! `DirectorContext`: the bounded, strongly typed input of one decision.
//!
//! The context is a snapshot. It is built outside any lock, sent to a provider
//! and never written back. It carries a deterministic digest of the WorldBible
//! ([`WorldSummary`]) instead of the raw document, plus small read-only views
//! of systems owned elsewhere ([`NarrativeView`], [`NpcView`]) so the Director
//! does not depend on their internals.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::action::{MAX_CONTENT_LEN, WorldEvent};
use crate::session::GameSession;

use super::actions::Disposition;
use super::canon::WorldBibleView;
use super::validate::{IssueCode, Issues, ValidationIssue};
use super::{
    DIRECTOR_SCHEMA_VERSION, PLAYER_ID, is_flag_key, is_identifier, is_snake_id, limits,
    truncate_chars,
};

/// Why the Director is being consulted. Chosen by the orchestrator, never by
/// a model. The structured variants let the deterministic fallback react
/// without interpreting free text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    /// A session just began; nothing has happened yet.
    SessionStart,
    /// The player did something; see the newest `recent_events`.
    PlayerAction,
    /// The player explicitly refused or abandoned an objective.
    ObjectiveRefused {
        objective_id: String,
    },
    ObjectiveCompleted {
        objective_id: String,
    },
    ObjectiveFailed {
        objective_id: String,
    },
    /// The player told `npc_id` something that undermines an objective.
    PlayerDisclosure {
        npc_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        objective_id: Option<String>,
    },
    /// Nothing happened for a while.
    Idle,
}

impl Trigger {
    pub const KINDS: [&'static str; 7] = [
        "session_start",
        "player_action",
        "objective_refused",
        "objective_completed",
        "objective_failed",
        "player_disclosure",
        "idle",
    ];

    pub fn kind(&self) -> &'static str {
        match self {
            Self::SessionStart => "session_start",
            Self::PlayerAction => "player_action",
            Self::ObjectiveRefused { .. } => "objective_refused",
            Self::ObjectiveCompleted { .. } => "objective_completed",
            Self::ObjectiveFailed { .. } => "objective_failed",
            Self::PlayerDisclosure { .. } => "player_disclosure",
            Self::Idle => "idle",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocationSummary {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterSummary {
    pub id: String,
    pub name: String,
    pub role: String,
    /// Their situation at the canon cutoff. Runtime state lives in [`NpcView`].
    pub status: String,
    #[serde(default)]
    pub traits: Vec<String>,
    #[serde(default)]
    pub goals: Vec<String>,
    /// `true` when the WorldBible classifies the character as canon.
    #[serde(default)]
    pub canon: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactionSummary {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub member_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerRoleSummary {
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpeningSummary {
    pub title: String,
    pub summary: String,
    pub location_id: String,
    #[serde(default)]
    pub involved_character_ids: Vec<String>,
    pub immediate_goal: String,
    pub stakes: String,
}

/// Deterministic, bounded digest of a WorldBible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldSummary {
    pub title: String,
    pub setting: String,
    pub era: String,
    #[serde(default)]
    pub canon_cutoff: Option<String>,
    #[serde(default)]
    pub timeline_summary: Option<String>,
    #[serde(default)]
    pub rules: Vec<String>,
    pub locations: Vec<LocationSummary>,
    pub characters: Vec<CharacterSummary>,
    #[serde(default)]
    pub factions: Vec<FactionSummary>,
    pub player_role: PlayerRoleSummary,
    #[serde(default)]
    pub opening: Option<OpeningSummary>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerView {
    /// `GameSession::current_location`. Any protocol identifier; it need not
    /// be a WorldBible location.
    #[serde(default)]
    pub location: Option<String>,
    /// Small free-form state the orchestrator wants the Director to see,
    /// e.g. `{"cover": "blown"}`.
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

/// What the player has been doing recently: a few bounded scores computed
/// from gameplay telemetry (`crate::telemetry`). Never raw events.
///
/// Scores run from 0 (none) to [`limits::MAX_TELEMETRY_SCORE`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerTelemetry {
    /// Length of the window the scores cover.
    pub window_seconds: u32,
    /// Damage taken and enemies killed.
    #[serde(default)]
    pub combat_intensity: u8,
    /// Player deaths, counted.
    #[serde(default)]
    pub recent_deaths: u8,
    /// Conversations and interactions with NPCs.
    #[serde(default)]
    pub npc_engagement: u8,
    /// Distinct places entered.
    #[serde(default)]
    pub exploration_activity: u8,
}

impl PlayerTelemetry {
    /// `combat_intensity` from which the player is under pressure.
    pub const HIGH_COMBAT: u8 = 60;
    /// `recent_deaths` from which the player is under pressure.
    pub const HIGH_DEATHS: u8 = 2;
    /// `npc_engagement` from which the player is playing socially.
    pub const HIGH_ENGAGEMENT: u8 = 60;

    /// The same summary with every value cut to its bound, so a misbehaving
    /// telemetry backend cannot make a context invalid.
    pub fn clamped(self) -> Self {
        let score = |value: u8| value.min(limits::MAX_TELEMETRY_SCORE);
        Self {
            window_seconds: self
                .window_seconds
                .clamp(1, limits::MAX_TELEMETRY_WINDOW_SECONDS),
            combat_intensity: score(self.combat_intensity),
            recent_deaths: score(self.recent_deaths),
            npc_engagement: score(self.npc_engagement),
            exploration_activity: score(self.exploration_activity),
        }
    }

    /// The player has been fighting hard or dying.
    pub fn under_pressure(&self) -> bool {
        self.combat_intensity >= Self::HIGH_COMBAT || self.recent_deaths >= Self::HIGH_DEATHS
    }

    /// The player has been talking to people.
    pub fn socially_engaged(&self) -> bool {
        self.npc_engagement >= Self::HIGH_ENGAGEMENT
    }

    /// A conversation serves this player better than another escalation:
    /// they need breathing room, or talking is how they are playing.
    pub fn prefers_dialogue(&self) -> bool {
        self.under_pressure() || self.socially_engaged()
    }
}

/// One entry of recent history, derived from a `WorldEvent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventView {
    pub sequence: u64,
    /// Kept as a string so new event types do not break the Director.
    pub event_type: String,
    #[serde(default)]
    pub actor_id: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    /// What was said, for speech events. Untrusted player input.
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionStatus {
    Active,
    Completed,
    Failed,
    Invalidated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectiveStatus {
    Active,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MissionView {
    pub mission_id: String,
    pub title: String,
    pub status: MissionStatus,
    #[serde(default)]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectiveView {
    pub objective_id: String,
    pub title: String,
    pub status: ObjectiveStatus,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub mission_id: Option<String>,
    /// The NPC who asked the player to do this, if any.
    #[serde(default)]
    pub giver_npc_id: Option<String>,
}

/// What the Director needs to know about the story so far. Supplied by the
/// narrative engine; `None` on the context means "not supplied", in which case
/// objective and mission references are format-checked only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NarrativeView {
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub missions: Vec<MissionView>,
    #[serde(default)]
    pub objectives: Vec<ObjectiveView>,
}

fn alive() -> bool {
    true
}

/// What the Director needs to know about one NPC right now. Supplied by the
/// NPC layer. A character without a view is assumed alive and not in play.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NpcView {
    pub npc_id: String,
    #[serde(default)]
    pub location: Option<String>,
    /// Present in the world and able to act.
    #[serde(default)]
    pub active: bool,
    #[serde(default = "alive")]
    pub alive: bool,
    /// How they currently feel about the player.
    #[serde(default)]
    pub disposition: Option<Disposition>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectorContext {
    pub schema_version: u32,
    pub session_id: Uuid,
    pub universe_id: String,
    /// `GameSession::event_count` when the snapshot was taken. Echoed on the
    /// decision so a stale decision can be recognised.
    pub event_count: u64,
    pub trigger: Trigger,
    pub world: WorldSummary,
    #[serde(default)]
    pub player: PlayerView,
    /// Oldest first.
    #[serde(default)]
    pub recent_events: Vec<EventView>,
    #[serde(default)]
    pub world_flags: BTreeMap<String, bool>,
    #[serde(default)]
    pub narrative: Option<NarrativeView>,
    #[serde(default)]
    pub npcs: Vec<NpcView>,
    /// Only a universe whose rules allow it may bring the dead back.
    #[serde(default)]
    pub allow_character_revival: bool,
    /// Recent player behaviour, when telemetry is available. Absent means
    /// unknown, not quiet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<PlayerTelemetry>,
}

/// Keep the first `max` distinct ids, `first` taking priority over `rest`.
fn prioritized<'a>(
    first: impl IntoIterator<Item = &'a str>,
    rest: impl IntoIterator<Item = &'a str>,
    max: usize,
) -> Vec<&'a str> {
    let mut kept: Vec<&str> = Vec::new();
    for id in first.into_iter().chain(rest) {
        if kept.len() == max {
            break;
        }
        if !kept.contains(&id) {
            kept.push(id);
        }
    }
    kept
}

impl WorldSummary {
    /// Digest a WorldBible. Same input, same output; every list and string is
    /// capped. When a list must be cut, the starting location and the opening
    /// conflict's cast are kept first.
    pub fn from_world_bible(bible: &WorldBibleView) -> Self {
        let opening = &bible.opening_conflict;

        let location_ids = prioritized(
            [
                bible.starting_location.location_id.as_str(),
                opening.location_id.as_str(),
            ],
            bible.locations.iter().map(|l| l.id.as_str()),
            limits::MAX_LOCATIONS,
        );
        let locations: Vec<LocationSummary> = location_ids
            .iter()
            .filter_map(|id| bible.locations.iter().find(|l| l.id == *id))
            .map(|l| LocationSummary {
                id: l.id.clone(),
                name: truncate_chars(&l.name, limits::MAX_NAME_CHARS),
                description: truncate_chars(&l.description, limits::MAX_BRIEF_CHARS),
            })
            .collect();

        let character_ids = prioritized(
            opening.involved_character_ids.iter().map(String::as_str),
            bible.characters.iter().map(|c| c.id.as_str()),
            limits::MAX_CHARACTERS,
        );
        let characters: Vec<CharacterSummary> = character_ids
            .iter()
            .filter_map(|id| bible.characters.iter().find(|c| c.id == *id))
            .map(|c| CharacterSummary {
                id: c.id.clone(),
                name: truncate_chars(&c.name, limits::MAX_NAME_CHARS),
                role: truncate_chars(&c.role, limits::MAX_BRIEF_CHARS),
                status: truncate_chars(&c.status, limits::MAX_BRIEF_CHARS),
                traits: c
                    .personality_traits
                    .iter()
                    .take(limits::MAX_TRAITS)
                    .map(|t| truncate_chars(t, limits::MAX_TAG_CHARS))
                    .collect(),
                goals: c
                    .goals
                    .iter()
                    .take(limits::MAX_GOALS)
                    .map(|g| truncate_chars(&g.text, limits::MAX_BRIEF_CHARS))
                    .collect(),
                canon: c.classification == "canon",
            })
            .collect();

        let known_character = |id: &String| characters.iter().any(|c| c.id == *id);
        let known_location = |id: &str| locations.iter().any(|l| l.id == id);

        let factions = bible
            .factions
            .iter()
            .take(limits::MAX_FACTIONS)
            .map(|f| FactionSummary {
                id: f.id.clone(),
                name: truncate_chars(&f.name, limits::MAX_NAME_CHARS),
                member_ids: f
                    .member_ids
                    .iter()
                    .filter(|id| known_character(id))
                    .take(limits::MAX_FACTION_MEMBERS)
                    .cloned()
                    .collect(),
            })
            .collect();

        let opening = known_location(&opening.location_id).then(|| OpeningSummary {
            title: truncate_chars(&opening.title, limits::MAX_NAME_CHARS),
            summary: truncate_chars(&opening.summary, limits::MAX_CONTEXT_TEXT_CHARS),
            location_id: opening.location_id.clone(),
            involved_character_ids: opening
                .involved_character_ids
                .iter()
                .filter(|id| known_character(id))
                .take(limits::MAX_OPENING_CHARACTERS)
                .cloned()
                .collect(),
            immediate_goal: truncate_chars(&opening.immediate_goal, limits::MAX_TEXT_CHARS),
            stakes: truncate_chars(&opening.stakes, limits::MAX_TEXT_CHARS),
        });

        Self {
            title: truncate_chars(&bible.universe.title, limits::MAX_NAME_CHARS),
            setting: truncate_chars(&bible.universe.setting.text, limits::MAX_CONTEXT_TEXT_CHARS),
            era: truncate_chars(&bible.universe.era.text, limits::MAX_CONTEXT_TEXT_CHARS),
            canon_cutoff: bible
                .timeline_context
                .canon_cutoff
                .as_deref()
                .map(|c| truncate_chars(c, limits::MAX_BRIEF_CHARS))
                .filter(|c| !c.is_empty()),
            timeline_summary: Some(truncate_chars(
                &bible.timeline_context.summary,
                limits::MAX_CONTEXT_TEXT_CHARS,
            ))
            .filter(|s| !s.is_empty()),
            rules: bible
                .world_rules
                .iter()
                .take(limits::MAX_WORLD_RULES)
                .map(|r| truncate_chars(&r.text, limits::MAX_TEXT_CHARS))
                .collect(),
            locations,
            characters,
            factions,
            player_role: PlayerRoleSummary {
                title: truncate_chars(&bible.player_role.title, limits::MAX_NAME_CHARS),
                description: truncate_chars(
                    &bible.player_role.description,
                    limits::MAX_CONTEXT_TEXT_CHARS,
                ),
                capabilities: bible
                    .player_role
                    .capabilities
                    .iter()
                    .take(limits::MAX_CAPABILITIES)
                    .map(|c| truncate_chars(c, limits::MAX_BRIEF_CHARS))
                    .collect(),
            },
            opening,
        }
    }

    pub fn character(&self, id: &str) -> Option<&CharacterSummary> {
        self.characters.iter().find(|c| c.id == id)
    }

    pub fn location(&self, id: &str) -> Option<&LocationSummary> {
        self.locations.iter().find(|l| l.id == id)
    }
}

impl EventView {
    /// Reduce a `WorldEvent` to what the Director reads. Unknown payload
    /// shapes are simply not carried over.
    pub fn from_world_event(event: &WorldEvent) -> Self {
        let event_type = serde_json::to_value(event.event_type)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".to_owned());
        let field = |key: &str| event.payload.get(key).and_then(Value::as_str);
        Self {
            sequence: event.sequence,
            event_type,
            actor_id: field("actor_id").map(str::to_owned),
            target: event.target.clone(),
            text: field("content").map(|c| truncate_chars(c, MAX_CONTENT_LEN)),
        }
    }
}

impl DirectorContext {
    /// Build a context from a session snapshot (`SessionStore::get`). History
    /// and flags are cut to the context bounds; `narrative` and `npcs` start
    /// empty for the owning systems to fill in.
    pub fn from_session(
        session: &GameSession,
        universe_id: impl Into<String>,
        world: WorldSummary,
        trigger: Trigger,
    ) -> Self {
        let skip = session
            .recent_events
            .len()
            .saturating_sub(limits::MAX_RECENT_EVENTS);
        Self {
            schema_version: DIRECTOR_SCHEMA_VERSION,
            session_id: session.session_id,
            universe_id: universe_id.into(),
            event_count: session.event_count,
            trigger,
            world,
            player: PlayerView {
                location: session.current_location.clone(),
                attributes: BTreeMap::new(),
            },
            recent_events: session
                .recent_events
                .iter()
                .skip(skip)
                .map(EventView::from_world_event)
                .collect(),
            world_flags: session
                .world_flags
                .iter()
                .take(limits::MAX_WORLD_FLAGS)
                .map(|(key, value)| (key.clone(), *value))
                .collect(),
            narrative: None,
            npcs: Vec::new(),
            allow_character_revival: false,
            telemetry: None,
        }
    }

    pub fn npc(&self, id: &str) -> Option<&NpcView> {
        self.npcs.iter().find(|n| n.npc_id == id)
    }

    /// Known dead. A character without an [`NpcView`] is assumed alive.
    pub fn is_dead(&self, npc_id: &str) -> bool {
        self.npc(npc_id).is_some_and(|n| !n.alive)
    }

    /// Characters the Director may act on: known and, unless the universe
    /// allows revival, not dead.
    pub fn actionable_npc_ids(&self) -> Vec<&str> {
        self.world
            .characters
            .iter()
            .map(|c| c.id.as_str())
            .filter(|id| self.allow_character_revival || !self.is_dead(id))
            .collect()
    }

    pub fn objective(&self, id: &str) -> Option<&ObjectiveView> {
        self.narrative
            .as_ref()?
            .objectives
            .iter()
            .find(|o| o.objective_id == id)
    }

    pub fn mission(&self, id: &str) -> Option<&MissionView> {
        self.narrative
            .as_ref()?
            .missions
            .iter()
            .find(|m| m.mission_id == id)
    }

    /// Check every bound and reference. A context that fails is never sent to
    /// a provider.
    pub fn validate(&self) -> Result<(), Vec<ValidationIssue>> {
        let mut issues = Issues::default();

        if self.schema_version != DIRECTOR_SCHEMA_VERSION {
            issues.push(
                "schema_version",
                IssueCode::UnsupportedVersion,
                format!(
                    "schema_version {} is not supported; expected {DIRECTOR_SCHEMA_VERSION}",
                    self.schema_version
                ),
            );
        }
        if !is_snake_id(&self.universe_id, limits::MAX_UNIVERSE_ID_LEN) {
            issues.push(
                "universe_id",
                IssueCode::InvalidIdentifier,
                "must match ^[a-z0-9_]{1,128}$",
            );
        }

        let characters = self.validate_world(&mut issues);
        self.validate_player_and_history(&mut issues);
        let objectives = self.validate_narrative(&mut issues, &characters);
        self.validate_npcs(&mut issues, &characters);
        self.validate_trigger(&mut issues, &characters, objectives.as_ref());
        self.validate_telemetry(&mut issues);

        if issues.is_empty() {
            let bytes = serde_json::to_vec(self)
                .map(|b| b.len())
                .unwrap_or(usize::MAX);
            if bytes > limits::MAX_CONTEXT_BYTES {
                issues.push(
                    "$",
                    IssueCode::TooMany,
                    format!(
                        "context is {bytes} bytes; the limit is {}",
                        limits::MAX_CONTEXT_BYTES
                    ),
                );
            }
        }
        issues.finish()
    }

    /// Returns the known character ids.
    fn validate_world<'a>(&'a self, issues: &mut Issues) -> BTreeSet<&'a str> {
        let world = &self.world;
        issues.context_text("world.title", &world.title, limits::MAX_NAME_CHARS);
        issues.context_text(
            "world.setting",
            &world.setting,
            limits::MAX_CONTEXT_TEXT_CHARS,
        );
        issues.context_text("world.era", &world.era, limits::MAX_CONTEXT_TEXT_CHARS);
        if let Some(cutoff) = &world.canon_cutoff {
            issues.context_text("world.canon_cutoff", cutoff, limits::MAX_BRIEF_CHARS);
        }
        if let Some(summary) = &world.timeline_summary {
            issues.context_text(
                "world.timeline_summary",
                summary,
                limits::MAX_CONTEXT_TEXT_CHARS,
            );
        }
        issues.at_most("world.rules", world.rules.len(), limits::MAX_WORLD_RULES);
        for (i, rule) in world.rules.iter().enumerate() {
            issues.context_text(format!("world.rules[{i}]"), rule, limits::MAX_TEXT_CHARS);
        }

        let mut all_ids: BTreeSet<&str> = BTreeSet::new();
        let mut entity = |issues: &mut Issues, path: String, id: &'a str| {
            if !is_identifier(id) {
                issues.push(
                    path,
                    IssueCode::InvalidIdentifier,
                    "is not a valid identifier",
                );
            } else if id == PLAYER_ID {
                issues.push(path, IssueCode::InvalidIdentifier, "\"player\" is reserved");
            } else if !all_ids.insert(id) {
                issues.push(path, IssueCode::DuplicateId, format!("duplicate id {id:?}"));
            }
        };

        issues.at_most(
            "world.locations",
            world.locations.len(),
            limits::MAX_LOCATIONS,
        );
        for (i, location) in world.locations.iter().enumerate() {
            let at = format!("world.locations[{i}]");
            entity(issues, format!("{at}.id"), &location.id);
            issues.context_text(format!("{at}.name"), &location.name, limits::MAX_NAME_CHARS);
            issues.context_text(
                format!("{at}.description"),
                &location.description,
                limits::MAX_BRIEF_CHARS,
            );
        }

        issues.at_most(
            "world.characters",
            world.characters.len(),
            limits::MAX_CHARACTERS,
        );
        for (i, character) in world.characters.iter().enumerate() {
            let at = format!("world.characters[{i}]");
            entity(issues, format!("{at}.id"), &character.id);
            issues.context_text(
                format!("{at}.name"),
                &character.name,
                limits::MAX_NAME_CHARS,
            );
            issues.context_text(
                format!("{at}.role"),
                &character.role,
                limits::MAX_BRIEF_CHARS,
            );
            issues.context_text(
                format!("{at}.status"),
                &character.status,
                limits::MAX_BRIEF_CHARS,
            );
            issues.at_most(
                format!("{at}.traits"),
                character.traits.len(),
                limits::MAX_TRAITS,
            );
            for (j, item) in character.traits.iter().enumerate() {
                issues.context_text(format!("{at}.traits[{j}]"), item, limits::MAX_TAG_CHARS);
            }
            issues.at_most(
                format!("{at}.goals"),
                character.goals.len(),
                limits::MAX_GOALS,
            );
            for (j, item) in character.goals.iter().enumerate() {
                issues.context_text(format!("{at}.goals[{j}]"), item, limits::MAX_BRIEF_CHARS);
            }
        }

        issues.at_most("world.factions", world.factions.len(), limits::MAX_FACTIONS);
        for (i, faction) in world.factions.iter().enumerate() {
            let at = format!("world.factions[{i}]");
            entity(issues, format!("{at}.id"), &faction.id);
            issues.context_text(format!("{at}.name"), &faction.name, limits::MAX_NAME_CHARS);
        }

        let characters: BTreeSet<&str> = world.characters.iter().map(|c| c.id.as_str()).collect();
        let locations: BTreeSet<&str> = world.locations.iter().map(|l| l.id.as_str()).collect();

        for (i, faction) in world.factions.iter().enumerate() {
            let at = format!("world.factions[{i}].member_ids");
            issues.at_most(
                at.clone(),
                faction.member_ids.len(),
                limits::MAX_FACTION_MEMBERS,
            );
            for (j, member) in faction.member_ids.iter().enumerate() {
                issues.known(format!("{at}[{j}]"), member, &characters, "character");
            }
        }

        let role = &world.player_role;
        issues.context_text(
            "world.player_role.title",
            &role.title,
            limits::MAX_NAME_CHARS,
        );
        issues.context_text(
            "world.player_role.description",
            &role.description,
            limits::MAX_CONTEXT_TEXT_CHARS,
        );
        issues.at_most(
            "world.player_role.capabilities",
            role.capabilities.len(),
            limits::MAX_CAPABILITIES,
        );
        for (i, item) in role.capabilities.iter().enumerate() {
            issues.context_text(
                format!("world.player_role.capabilities[{i}]"),
                item,
                limits::MAX_BRIEF_CHARS,
            );
        }

        if let Some(opening) = &world.opening {
            issues.context_text(
                "world.opening.title",
                &opening.title,
                limits::MAX_NAME_CHARS,
            );
            issues.context_text(
                "world.opening.summary",
                &opening.summary,
                limits::MAX_CONTEXT_TEXT_CHARS,
            );
            issues.context_text(
                "world.opening.immediate_goal",
                &opening.immediate_goal,
                limits::MAX_TEXT_CHARS,
            );
            issues.context_text(
                "world.opening.stakes",
                &opening.stakes,
                limits::MAX_TEXT_CHARS,
            );
            issues.known(
                "world.opening.location_id",
                &opening.location_id,
                &locations,
                "location",
            );
            issues.at_most(
                "world.opening.involved_character_ids",
                opening.involved_character_ids.len(),
                limits::MAX_OPENING_CHARACTERS,
            );
            for (i, id) in opening.involved_character_ids.iter().enumerate() {
                issues.known(
                    format!("world.opening.involved_character_ids[{i}]"),
                    id,
                    &characters,
                    "character",
                );
            }
        }

        characters
    }

    fn validate_telemetry(&self, issues: &mut Issues) {
        let Some(telemetry) = &self.telemetry else {
            return;
        };
        if telemetry.window_seconds == 0
            || telemetry.window_seconds > limits::MAX_TELEMETRY_WINDOW_SECONDS
        {
            issues.push(
                "telemetry.window_seconds",
                IssueCode::InvalidValue,
                format!("must be 1-{} seconds", limits::MAX_TELEMETRY_WINDOW_SECONDS),
            );
        }
        for (name, value) in [
            ("combat_intensity", telemetry.combat_intensity),
            ("recent_deaths", telemetry.recent_deaths),
            ("npc_engagement", telemetry.npc_engagement),
            ("exploration_activity", telemetry.exploration_activity),
        ] {
            if value > limits::MAX_TELEMETRY_SCORE {
                issues.push(
                    format!("telemetry.{name}"),
                    IssueCode::InvalidValue,
                    format!("must be 0-{}", limits::MAX_TELEMETRY_SCORE),
                );
            }
        }
    }

    fn validate_player_and_history(&self, issues: &mut Issues) {
        if let Some(location) = &self.player.location {
            issues.identifier("player.location", location);
        }
        issues.at_most(
            "player.attributes",
            self.player.attributes.len(),
            limits::MAX_PLAYER_ATTRIBUTES,
        );
        for (key, value) in &self.player.attributes {
            issues.identifier(format!("player.attributes.{key}"), key);
            issues.context_text(
                format!("player.attributes.{key}"),
                value,
                limits::MAX_BRIEF_CHARS,
            );
        }

        issues.at_most(
            "recent_events",
            self.recent_events.len(),
            limits::MAX_RECENT_EVENTS,
        );
        let mut previous: Option<u64> = None;
        for (i, event) in self.recent_events.iter().enumerate() {
            let at = format!("recent_events[{i}]");
            if previous.is_some_and(|p| event.sequence <= p) {
                issues.push(
                    format!("{at}.sequence"),
                    IssueCode::InvalidValue,
                    "events must be ordered oldest first with increasing sequence",
                );
            }
            if event.sequence > self.event_count {
                issues.push(
                    format!("{at}.sequence"),
                    IssueCode::InvalidValue,
                    "sequence is newer than event_count",
                );
            }
            previous = Some(event.sequence);
            issues.identifier(format!("{at}.event_type"), &event.event_type);
            if let Some(actor) = &event.actor_id {
                issues.identifier(format!("{at}.actor_id"), actor);
            }
            if let Some(target) = &event.target {
                issues.identifier(format!("{at}.target"), target);
            }
            if let Some(text) = &event.text {
                issues.context_text(format!("{at}.text"), text, MAX_CONTENT_LEN);
            }
        }

        issues.at_most(
            "world_flags",
            self.world_flags.len(),
            limits::MAX_WORLD_FLAGS,
        );
        for key in self.world_flags.keys() {
            if !is_flag_key(key) {
                issues.push(
                    format!("world_flags.{}", truncate_chars(key, 32)),
                    IssueCode::InvalidIdentifier,
                    "is not a valid flag key",
                );
            }
        }
    }

    /// Returns the known objective ids when a narrative view is supplied.
    fn validate_narrative<'a>(
        &'a self,
        issues: &mut Issues,
        characters: &BTreeSet<&str>,
    ) -> Option<BTreeSet<&'a str>> {
        let narrative = self.narrative.as_ref()?;
        if let Some(summary) = &narrative.summary {
            issues.context_text("narrative.summary", summary, limits::MAX_CONTEXT_TEXT_CHARS);
        }

        issues.at_most(
            "narrative.missions",
            narrative.missions.len(),
            limits::MAX_MISSIONS,
        );
        let mut missions: BTreeSet<&str> = BTreeSet::new();
        for (i, mission) in narrative.missions.iter().enumerate() {
            let at = format!("narrative.missions[{i}]");
            if issues.identifier(format!("{at}.mission_id"), &mission.mission_id)
                && !missions.insert(mission.mission_id.as_str())
            {
                issues.push(
                    format!("{at}.mission_id"),
                    IssueCode::DuplicateId,
                    "duplicate mission id",
                );
            }
            issues.context_text(
                format!("{at}.title"),
                &mission.title,
                limits::MAX_NAME_CHARS,
            );
            if let Some(summary) = &mission.summary {
                issues.context_text(
                    format!("{at}.summary"),
                    summary,
                    limits::MAX_CONTEXT_TEXT_CHARS,
                );
            }
        }

        issues.at_most(
            "narrative.objectives",
            narrative.objectives.len(),
            limits::MAX_OBJECTIVES,
        );
        let mut objectives: BTreeSet<&str> = BTreeSet::new();
        for (i, objective) in narrative.objectives.iter().enumerate() {
            let at = format!("narrative.objectives[{i}]");
            if issues.identifier(format!("{at}.objective_id"), &objective.objective_id)
                && !objectives.insert(objective.objective_id.as_str())
            {
                issues.push(
                    format!("{at}.objective_id"),
                    IssueCode::DuplicateId,
                    "duplicate objective id",
                );
            }
            issues.context_text(
                format!("{at}.title"),
                &objective.title,
                limits::MAX_NAME_CHARS,
            );
            if let Some(description) = &objective.description {
                issues.context_text(
                    format!("{at}.description"),
                    description,
                    limits::MAX_CONTEXT_TEXT_CHARS,
                );
            }
            if let Some(mission_id) = &objective.mission_id {
                issues.known(format!("{at}.mission_id"), mission_id, &missions, "mission");
            }
            if let Some(giver) = &objective.giver_npc_id {
                issues.known(format!("{at}.giver_npc_id"), giver, characters, "character");
            }
        }
        Some(objectives)
    }

    fn validate_npcs(&self, issues: &mut Issues, characters: &BTreeSet<&str>) {
        issues.at_most("npcs", self.npcs.len(), limits::MAX_NPCS);
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for (i, npc) in self.npcs.iter().enumerate() {
            let at = format!("npcs[{i}]");
            if issues.known(format!("{at}.npc_id"), &npc.npc_id, characters, "character")
                && !seen.insert(npc.npc_id.as_str())
            {
                issues.push(
                    format!("{at}.npc_id"),
                    IssueCode::DuplicateId,
                    "duplicate npc view",
                );
            }
            if let Some(location) = &npc.location {
                issues.identifier(format!("{at}.location"), location);
            }
            if let Some(status) = &npc.status {
                issues.context_text(format!("{at}.status"), status, limits::MAX_BRIEF_CHARS);
            }
            if npc.active && !npc.alive {
                issues.push(
                    format!("{at}.active"),
                    IssueCode::InvalidCombination,
                    "a dead NPC cannot be active",
                );
            }
        }
    }

    fn validate_trigger(
        &self,
        issues: &mut Issues,
        characters: &BTreeSet<&str>,
        objectives: Option<&BTreeSet<&str>>,
    ) {
        let objective = |issues: &mut Issues, id: &str| match objectives {
            Some(known) => {
                issues.known("trigger.objective_id", id, known, "objective");
            }
            None => {
                issues.identifier("trigger.objective_id", id);
            }
        };
        match &self.trigger {
            Trigger::SessionStart | Trigger::PlayerAction | Trigger::Idle => {}
            Trigger::ObjectiveRefused { objective_id }
            | Trigger::ObjectiveCompleted { objective_id }
            | Trigger::ObjectiveFailed { objective_id } => objective(issues, objective_id),
            Trigger::PlayerDisclosure {
                npc_id,
                objective_id,
            } => {
                issues.known("trigger.npc_id", npc_id, characters, "character");
                if let Some(objective_id) = objective_id {
                    objective(issues, objective_id);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{self, ActionType, ValidatedAction};
    use crate::director::testing::{BREAKING_BAD_WORLD_BIBLE, sample_context};
    use chrono::Utc;
    use serde_json::json;

    fn codes(ctx: &DirectorContext) -> Vec<(String, IssueCode)> {
        ctx.validate()
            .err()
            .unwrap_or_default()
            .into_iter()
            .map(|i| (i.path, i.code))
            .collect()
    }

    fn has(ctx: &DirectorContext, path: &str, code: IssueCode) -> bool {
        codes(ctx).iter().any(|(p, c)| p == path && *c == code)
    }

    #[test]
    fn sample_context_is_valid_and_round_trips() {
        let ctx = sample_context();
        assert_eq!(ctx.validate(), Ok(()));
        let text = serde_json::to_string(&ctx).unwrap();
        let back: DirectorContext = serde_json::from_str(&text).unwrap();
        assert_eq!(back, ctx);
    }

    #[test]
    fn telemetry_is_optional_bounded_and_round_trips() {
        let mut ctx = sample_context();
        assert_eq!(ctx.telemetry, None);
        let without = serde_json::to_value(&ctx).unwrap();
        assert!(without.get("telemetry").is_none());

        ctx.telemetry = Some(PlayerTelemetry {
            window_seconds: 300,
            combat_intensity: 70,
            recent_deaths: 2,
            npc_engagement: 40,
            exploration_activity: 100,
        });
        assert_eq!(ctx.validate(), Ok(()));
        let json = serde_json::to_string(&ctx).unwrap();
        assert_eq!(serde_json::from_str::<DirectorContext>(&json).unwrap(), ctx);
        // A summary, not a history: a handful of numbers.
        let added = json.len() - serde_json::to_string(&without).unwrap().len();
        assert!(added < 160, "telemetry adds {added} bytes");

        ctx.telemetry = Some(PlayerTelemetry {
            window_seconds: 0,
            combat_intensity: 101,
            recent_deaths: 255,
            npc_engagement: 0,
            exploration_activity: 0,
        });
        let issues = ctx.validate().unwrap_err();
        let paths: Vec<&str> = issues.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "telemetry.window_seconds",
                "telemetry.combat_intensity",
                "telemetry.recent_deaths"
            ]
        );
        assert!(issues.iter().all(|i| i.code == IssueCode::InvalidValue));

        // Raw events cannot be smuggled in.
        let mut raw = serde_json::to_value(sample_context()).unwrap();
        raw["telemetry"] = serde_json::json!({ "window_seconds": 300, "events": [] });
        assert!(serde_json::from_value::<DirectorContext>(raw).is_err());
    }

    #[test]
    fn player_telemetry_thresholds() {
        let quiet = PlayerTelemetry::default();
        assert!(!quiet.prefers_dialogue());
        let fighting = PlayerTelemetry {
            combat_intensity: PlayerTelemetry::HIGH_COMBAT,
            ..quiet
        };
        let dying = PlayerTelemetry {
            recent_deaths: PlayerTelemetry::HIGH_DEATHS,
            ..quiet
        };
        let talking = PlayerTelemetry {
            npc_engagement: PlayerTelemetry::HIGH_ENGAGEMENT,
            ..quiet
        };
        assert!(fighting.under_pressure() && fighting.prefers_dialogue());
        assert!(dying.under_pressure() && dying.prefers_dialogue());
        assert!(!talking.under_pressure() && talking.socially_engaged());
        assert!(talking.prefers_dialogue());
        let exploring = PlayerTelemetry {
            exploration_activity: 100,
            ..quiet
        };
        assert!(!exploring.prefers_dialogue());
    }

    #[test]
    fn decoding_is_strict() {
        let mut value = serde_json::to_value(sample_context()).unwrap();
        value["debug_console"] = json!("give_all");
        assert!(serde_json::from_value::<DirectorContext>(value).is_err());

        let mut value = serde_json::to_value(sample_context()).unwrap();
        value["trigger"] = json!({"kind": "run_script"});
        assert!(serde_json::from_value::<DirectorContext>(value).is_err());
    }

    #[test]
    fn trigger_wire_form() {
        let disclosure = Trigger::PlayerDisclosure {
            npc_id: "captain_ines".into(),
            objective_id: Some("hide_ledger".into()),
        };
        assert_eq!(
            serde_json::to_value(&disclosure).unwrap(),
            json!({"kind": "player_disclosure", "npc_id": "captain_ines", "objective_id": "hide_ledger"})
        );
        assert_eq!(
            serde_json::to_value(Trigger::SessionStart).unwrap(),
            json!({"kind": "session_start"})
        );
        for kind in Trigger::KINDS {
            let value = match kind {
                "objective_refused" | "objective_completed" | "objective_failed" => {
                    json!({"kind": kind, "objective_id": "o"})
                }
                "player_disclosure" => json!({"kind": kind, "npc_id": "n"}),
                _ => json!({"kind": kind}),
            };
            let trigger: Trigger = serde_json::from_value(value).unwrap();
            assert_eq!(trigger.kind(), kind);
        }
    }

    #[test]
    fn rejects_wrong_version_and_universe_id() {
        let mut ctx = sample_context();
        ctx.schema_version = 2;
        ctx.universe_id = "Lantern Bay".into();
        assert!(has(&ctx, "schema_version", IssueCode::UnsupportedVersion));
        assert!(has(&ctx, "universe_id", IssueCode::InvalidIdentifier));
    }

    #[test]
    fn rejects_bad_and_duplicate_world_ids() {
        let mut ctx = sample_context();
        ctx.world.locations[0].id = "harbor office!".into();
        assert!(has(
            &ctx,
            "world.locations[0].id",
            IssueCode::InvalidIdentifier
        ));

        let mut ctx = sample_context();
        ctx.world.characters[1].id = ctx.world.characters[0].id.clone();
        assert!(has(&ctx, "world.characters[1].id", IssueCode::DuplicateId));

        let mut ctx = sample_context();
        ctx.world.characters[0].id = "player".into();
        assert!(has(
            &ctx,
            "world.characters[0].id",
            IssueCode::InvalidIdentifier
        ));
    }

    #[test]
    fn rejects_unknown_references() {
        let mut ctx = sample_context();
        ctx.npcs[0].npc_id = "nobody".into();
        assert!(has(&ctx, "npcs[0].npc_id", IssueCode::UnknownReference));

        let mut ctx = sample_context();
        ctx.narrative.as_mut().unwrap().objectives[0].mission_id = Some("ghost_mission".into());
        assert!(has(
            &ctx,
            "narrative.objectives[0].mission_id",
            IssueCode::UnknownReference
        ));

        let mut ctx = sample_context();
        ctx.trigger = Trigger::ObjectiveRefused {
            objective_id: "never_given".into(),
        };
        assert!(has(
            &ctx,
            "trigger.objective_id",
            IssueCode::UnknownReference
        ));

        let mut ctx = sample_context();
        ctx.world.opening.as_mut().unwrap().location_id = "atlantis".into();
        assert!(has(
            &ctx,
            "world.opening.location_id",
            IssueCode::UnknownReference
        ));
    }

    #[test]
    fn trigger_objective_is_format_checked_without_narrative() {
        let mut ctx = sample_context();
        ctx.narrative = None;
        ctx.trigger = Trigger::ObjectiveRefused {
            objective_id: "never_given".into(),
        };
        assert_eq!(ctx.validate(), Ok(()));
        ctx.trigger = Trigger::ObjectiveRefused {
            objective_id: "bad id".into(),
        };
        assert!(has(
            &ctx,
            "trigger.objective_id",
            IssueCode::InvalidIdentifier
        ));
    }

    #[test]
    fn enforces_bounds() {
        let mut ctx = sample_context();
        let event = ctx.recent_events[0].clone();
        ctx.recent_events = (1..=17)
            .map(|sequence| EventView {
                sequence,
                ..event.clone()
            })
            .collect();
        ctx.event_count = 17;
        assert!(has(&ctx, "recent_events", IssueCode::TooMany));

        let mut ctx = sample_context();
        ctx.world.setting = "x".repeat(limits::MAX_CONTEXT_TEXT_CHARS + 1);
        assert!(has(&ctx, "world.setting", IssueCode::InvalidText));

        let mut ctx = sample_context();
        ctx.world.title = "   ".into();
        assert!(has(&ctx, "world.title", IssueCode::InvalidText));

        let mut ctx = sample_context();
        ctx.world_flags = (0..65).map(|i| (format!("flag_{i}"), true)).collect();
        assert!(has(&ctx, "world_flags", IssueCode::TooMany));

        let mut ctx = sample_context();
        let npc = ctx.npcs[0].clone();
        ctx.npcs = vec![npc; 13];
        assert!(has(&ctx, "npcs", IssueCode::TooMany));
    }

    #[test]
    fn rejects_disordered_history_and_dead_but_active() {
        let mut ctx = sample_context();
        ctx.recent_events.reverse();
        assert!(has(
            &ctx,
            "recent_events[1].sequence",
            IssueCode::InvalidValue
        ));

        let mut ctx = sample_context();
        ctx.event_count = 1;
        assert!(has(
            &ctx,
            "recent_events[0].sequence",
            IssueCode::InvalidValue
        ));

        let mut ctx = sample_context();
        let dead = ctx.npcs.iter_mut().find(|n| !n.alive).unwrap();
        dead.active = true;
        assert!(
            codes(&ctx)
                .iter()
                .any(|(_, code)| *code == IssueCode::InvalidCombination)
        );
    }

    #[test]
    fn world_bible_digest_is_bounded_and_valid() {
        let bible = WorldBibleView::from_json_str(BREAKING_BAD_WORLD_BIBLE).unwrap();
        let world = WorldSummary::from_world_bible(&bible);
        assert_eq!(
            world,
            WorldSummary::from_world_bible(&bible),
            "deterministic"
        );
        assert_eq!(world.title, "Breaking Bad");
        assert_eq!(world.locations[0].id, "albuquerque_hospital");
        assert!(world.character("hank_schrader").is_some());
        assert!(world.character("walter_white").unwrap().canon);
        assert!(!world.character("hank_schrader").unwrap().canon);
        assert_eq!(
            world.opening.as_ref().unwrap().involved_character_ids,
            ["walter_white", "hank_schrader"]
        );

        let digest_bytes = serde_json::to_vec(&world).unwrap().len();
        assert!(
            digest_bytes < BREAKING_BAD_WORLD_BIBLE.len() / 2,
            "digest ({digest_bytes} B) must be much smaller than the WorldBible"
        );

        let mut ctx = sample_context();
        ctx.universe_id = bible.universe.universe_id.clone();
        ctx.world = world;
        ctx.narrative = None;
        ctx.npcs.clear();
        ctx.trigger = Trigger::SessionStart;
        assert_eq!(ctx.validate(), Ok(()));
    }

    #[test]
    fn digest_cuts_long_lists_but_keeps_the_opening_cast() {
        let mut bible = WorldBibleView::from_json_str(BREAKING_BAD_WORLD_BIBLE).unwrap();
        let template = bible.characters[0].clone();
        let mut crowd: Vec<_> = (0..20)
            .map(|i| {
                let mut extra = template.clone();
                extra.id = format!("extra_{i}");
                extra.role = "r".repeat(1000);
                extra
            })
            .collect();
        crowd.append(&mut bible.characters);
        bible.characters = crowd;
        bible.factions[0].member_ids.push("extra_19".into());

        let world = WorldSummary::from_world_bible(&bible);
        assert_eq!(world.characters.len(), limits::MAX_CHARACTERS);
        assert_eq!(world.characters[0].id, "walter_white");
        assert_eq!(world.characters[1].id, "hank_schrader");
        assert!(
            world
                .characters
                .iter()
                .all(|c| c.role.chars().count() <= limits::MAX_BRIEF_CHARS)
        );
        assert!(
            !world.factions[0]
                .member_ids
                .contains(&"extra_19".to_owned())
        );

        let mut ctx = sample_context();
        ctx.world = world;
        ctx.narrative = None;
        ctx.npcs.clear();
        ctx.trigger = Trigger::Idle;
        assert_eq!(ctx.validate(), Ok(()));
    }

    #[test]
    fn builds_from_a_session_snapshot() {
        let now = Utc::now();
        let session_id = Uuid::new_v4();
        let mut session = GameSession::new(session_id, now);
        let act = |action_type, target: &str, content: Option<&str>| ValidatedAction {
            action_id: Uuid::new_v4(),
            session_id,
            actor_id: "player".into(),
            action_type,
            target: Some(target.into()),
            content: content.map(Into::into),
        };
        let actions = [
            act(ActionType::Move, "harbor_office", None),
            act(ActionType::Interact, "desk", None),
            act(ActionType::Speak, "captain_ines", Some("He hid a ledger.")),
        ];
        for a in &actions {
            action::apply(&mut session, a, now).unwrap();
        }
        for _ in 0..20 {
            let a = act(ActionType::Inspect, "lamp", None);
            action::apply(&mut session, &a, now).unwrap();
        }

        let world = sample_context().world;
        let ctx =
            DirectorContext::from_session(&session, "lantern_bay", world, Trigger::PlayerAction);
        assert_eq!(ctx.validate(), Ok(()));
        assert_eq!(ctx.session_id, session.session_id);
        assert_eq!(ctx.event_count, 23);
        assert_eq!(ctx.player.location.as_deref(), Some("harbor_office"));
        assert_eq!(ctx.world_flags.get("interacted:desk"), Some(&true));
        assert_eq!(ctx.recent_events.len(), limits::MAX_RECENT_EVENTS);
        assert_eq!(ctx.recent_events.last().unwrap().sequence, 23);
        assert_eq!(ctx.recent_events[0].sequence, 8);

        let speech = EventView::from_world_event(&session.recent_events[2]);
        assert_eq!(speech.event_type, "speech_acknowledged");
        assert_eq!(speech.actor_id.as_deref(), Some("player"));
        assert_eq!(speech.target.as_deref(), Some("captain_ines"));
        assert_eq!(speech.text.as_deref(), Some("He hid a ledger."));
    }

    #[test]
    fn oversized_context_is_rejected_as_a_whole() {
        // Every part within its own bound, the sum over the byte ceiling.
        let mut ctx = sample_context();
        let wide = "é".repeat(limits::MAX_BRIEF_CHARS);
        let template = ctx.world.characters[0].clone();
        ctx.world.characters = (0..limits::MAX_CHARACTERS)
            .map(|i| CharacterSummary {
                id: format!("c{i}"),
                role: wide.clone(),
                status: wide.clone(),
                goals: vec![wide.clone(); limits::MAX_GOALS],
                ..template.clone()
            })
            .collect();
        ctx.world.locations = (0..limits::MAX_LOCATIONS)
            .map(|i| LocationSummary {
                id: format!("l{i}"),
                name: "n".into(),
                description: wide.clone(),
            })
            .collect();
        ctx.world.factions.clear();
        ctx.world.opening = None;
        ctx.narrative = None;
        ctx.npcs.clear();
        ctx.trigger = Trigger::Idle;
        let speech = "é".repeat(MAX_CONTENT_LEN);
        ctx.recent_events = (1..=limits::MAX_RECENT_EVENTS as u64)
            .map(|sequence| EventView {
                sequence,
                event_type: "speech_acknowledged".into(),
                actor_id: Some("player".into()),
                target: None,
                text: Some(speech.clone()),
            })
            .collect();
        ctx.event_count = limits::MAX_RECENT_EVENTS as u64;
        ctx.world_flags = (0..limits::MAX_WORLD_FLAGS)
            .map(|i| (format!("{}_{i}", "f".repeat(120)), true))
            .collect();
        ctx.world.rules = vec!["é".repeat(limits::MAX_TEXT_CHARS); limits::MAX_WORLD_RULES];
        ctx.world.setting = "é".repeat(limits::MAX_CONTEXT_TEXT_CHARS);

        let bytes = serde_json::to_vec(&ctx).unwrap().len();
        assert!(
            bytes > limits::MAX_CONTEXT_BYTES,
            "fixture is only {bytes} B"
        );
        let issues = ctx.validate().unwrap_err();
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].code, IssueCode::TooMany);
        assert_eq!(issues[0].path, "$");
    }
}
