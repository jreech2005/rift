//! Validation of Director output.
//!
//! Nothing a provider returns is trusted. A proposal is parsed strictly,
//! bounded, and checked against the context it was made for. Validation is
//! all-or-nothing: one bad action rejects the whole proposal, so a decision
//! is never half-applied and invalid output can never become a `WorldEvent`.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::actions::DirectorAction;
use super::context::{DirectorContext, MissionStatus, ObjectiveStatus};
use super::decision::{DirectorProposal, ReasonCode};
use super::{
    PLAYER_ID, RESERVED_FLAG_PREFIX, is_flag_key, is_identifier, is_snake_id, limits,
    truncate_chars,
};

/// Issues reported per rejected proposal. Enough to repair, not a flood.
pub const MAX_REPORTED_ISSUES: usize = 25;

const PROPOSAL_FIELDS: [&str; 4] = ["reason_code", "actions", "narrative_summary", "confidence"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueCode {
    /// Not JSON, wrong shape, missing or unknown field.
    Malformed,
    /// An action `type` outside the allowlist.
    UnknownActionType,
    UnsupportedVersion,
    InvalidIdentifier,
    /// Empty, too long, or containing control characters.
    InvalidText,
    InvalidValue,
    TooMany,
    DuplicateId,
    /// Refers to something the context does not contain.
    UnknownReference,
    /// Would act on a dead character.
    DeadCharacter,
    /// Each part is fine; together they contradict the state or each other.
    InvalidCombination,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    /// Where, e.g. `actions[2].npc_id`. `$` is the whole document.
    pub path: String,
    pub code: IssueCode,
    pub message: String,
}

impl ValidationIssue {
    pub fn new(path: impl Into<String>, code: IssueCode, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// Collects issues, capped at [`MAX_REPORTED_ISSUES`].
#[derive(Debug, Default)]
pub(crate) struct Issues(Vec<ValidationIssue>);

impl Issues {
    pub(crate) fn push(
        &mut self,
        path: impl Into<String>,
        code: IssueCode,
        message: impl Into<String>,
    ) {
        if self.0.len() < MAX_REPORTED_ISSUES {
            self.0.push(ValidationIssue::new(path, code, message));
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn finish(self) -> Result<(), Vec<ValidationIssue>> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(self.0)
        }
    }

    pub(crate) fn identifier(&mut self, path: impl Into<String>, value: &str) -> bool {
        let ok = is_identifier(value);
        if !ok {
            self.push(
                path,
                IssueCode::InvalidIdentifier,
                "must be a 1-64 char identifier [A-Za-z0-9_.:-]",
            );
        }
        ok
    }

    /// `value` is a valid identifier and one of `known`.
    pub(crate) fn known(
        &mut self,
        path: impl Into<String>,
        value: &str,
        known: &BTreeSet<&str>,
        kind: &str,
    ) -> bool {
        let path = path.into();
        if !self.identifier(path.as_str(), value) {
            return false;
        }
        let ok = known.contains(value);
        if !ok {
            self.push(
                path,
                IssueCode::UnknownReference,
                format!("{value:?} is not a known {kind}"),
            );
        }
        ok
    }

    pub(crate) fn at_most(&mut self, path: impl Into<String>, len: usize, max: usize) {
        if len > max {
            self.push(
                path,
                IssueCode::TooMany,
                format!("{len} items; at most {max} allowed"),
            );
        }
    }

    /// Context text: present and bounded.
    pub(crate) fn context_text(
        &mut self,
        path: impl Into<String>,
        value: &str,
        max: usize,
    ) -> bool {
        let chars = value.chars().count();
        let ok = !value.trim().is_empty() && chars <= max;
        if !ok {
            self.push(
                path,
                IssueCode::InvalidText,
                format!("must be 1-{max} chars, got {chars}"),
            );
        }
        ok
    }

    /// Director-authored text: present, bounded, single line, printable.
    fn authored_text(&mut self, path: impl Into<String>, value: &str, max: usize) {
        let chars = value.chars().count();
        if value.trim().is_empty() {
            self.push(path, IssueCode::InvalidText, "must not be empty");
        } else if chars > max {
            self.push(
                path,
                IssueCode::InvalidText,
                format!("must be at most {max} chars, got {chars}"),
            );
        } else if value.chars().any(char::is_control) {
            self.push(
                path,
                IssueCode::InvalidText,
                "must not contain control characters or line breaks",
            );
        }
    }
}

/// Parse and validate raw provider output against `ctx`.
///
/// Shape is checked first and the action count before any action is decoded,
/// so oversized output is rejected cheaply. Every problem found is reported
/// (up to [`MAX_REPORTED_ISSUES`]) to give the single repair attempt its best
/// chance.
pub fn parse_proposal(
    ctx: &DirectorContext,
    raw: &str,
) -> Result<DirectorProposal, Vec<ValidationIssue>> {
    let malformed =
        |message: String| vec![ValidationIssue::new("$", IssueCode::Malformed, message)];

    let value: Value = serde_json::from_str(raw)
        .map_err(|err| malformed(format!("output is not valid JSON: {err}")))?;
    let Value::Object(object) = value else {
        return Err(malformed("output must be a JSON object".to_owned()));
    };

    let mut issues = Issues::default();
    for key in object.keys() {
        if !PROPOSAL_FIELDS.contains(&key.as_str()) {
            issues.push(
                truncate_chars(key, 64),
                IssueCode::Malformed,
                "unknown field; allowed: reason_code, actions, narrative_summary, confidence",
            );
        }
    }

    let mut actions: Vec<(usize, DirectorAction)> = Vec::new();
    match object.get("actions") {
        Some(Value::Array(items)) if items.len() > limits::MAX_ACTIONS => {
            return Err(vec![too_many_actions(items.len())]);
        }
        Some(Value::Array(items)) => {
            for (index, item) in items.iter().enumerate() {
                match parse_action(item) {
                    Ok(action) => actions.push((index, action)),
                    Err((code, message)) => issues.push(format!("actions[{index}]"), code, message),
                }
            }
        }
        _ => issues.push("actions", IssueCode::Malformed, "must be an array"),
    }

    let reason_code = object
        .get("reason_code")
        .and_then(|v| serde_json::from_value::<ReasonCode>(v.clone()).ok());
    if reason_code.is_none() {
        issues.push(
            "reason_code",
            IssueCode::InvalidValue,
            format!("must be one of: {}", ReasonCode::ALL.join(", ")),
        );
    }

    let confidence = object.get("confidence").and_then(Value::as_f64);
    if confidence.is_none() {
        issues.push(
            "confidence",
            IssueCode::InvalidValue,
            "must be a number from 0 to 1",
        );
    }

    let narrative_summary = match object.get("narrative_summary") {
        None | Some(Value::Null) => None,
        // The summary is optional; an empty one is the same as none.
        Some(Value::String(text)) if text.trim().is_empty() => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => {
            issues.push(
                "narrative_summary",
                IssueCode::Malformed,
                "must be a string or null",
            );
            None
        }
    };

    check_summary(
        &mut issues,
        reason_code,
        confidence,
        narrative_summary.as_deref(),
        actions.len(),
    );
    check_actions(ctx, &mut issues, actions.iter().map(|(i, a)| (*i, a)));
    issues.finish()?;

    match (reason_code, confidence) {
        (Some(reason_code), Some(confidence)) => Ok(DirectorProposal {
            reason_code,
            actions: actions.into_iter().map(|(_, action)| action).collect(),
            narrative_summary,
            confidence,
        }),
        // Unreachable: a missing value was reported above.
        _ => Err(malformed("incomplete proposal".to_owned())),
    }
}

/// Validate an already-typed proposal against `ctx`.
pub fn validate_proposal(
    ctx: &DirectorContext,
    proposal: &DirectorProposal,
) -> Result<(), Vec<ValidationIssue>> {
    if proposal.actions.len() > limits::MAX_ACTIONS {
        return Err(vec![too_many_actions(proposal.actions.len())]);
    }
    let mut issues = Issues::default();
    check_summary(
        &mut issues,
        Some(proposal.reason_code),
        Some(proposal.confidence),
        proposal.narrative_summary.as_deref(),
        proposal.actions.len(),
    );
    check_actions(ctx, &mut issues, proposal.actions.iter().enumerate());
    issues.finish()
}

/// Validate actions against `ctx`. Public so the integration layer can check
/// a decision again, against current state, right before applying it.
pub fn validate_actions(
    ctx: &DirectorContext,
    actions: &[DirectorAction],
) -> Result<(), Vec<ValidationIssue>> {
    if actions.len() > limits::MAX_ACTIONS {
        return Err(vec![too_many_actions(actions.len())]);
    }
    let mut issues = Issues::default();
    check_actions(ctx, &mut issues, actions.iter().enumerate());
    issues.finish()
}

fn too_many_actions(count: usize) -> ValidationIssue {
    ValidationIssue::new(
        "actions",
        IssueCode::TooMany,
        format!("{count} actions; at most {} allowed", limits::MAX_ACTIONS),
    )
}

fn parse_action(item: &Value) -> Result<DirectorAction, (IssueCode, String)> {
    let Some(object) = item.as_object() else {
        return Err((IssueCode::Malformed, "must be a JSON object".to_owned()));
    };
    match object.get("type") {
        Some(Value::String(kind)) if DirectorAction::TYPES.contains(&kind.as_str()) => {}
        Some(Value::String(kind)) => {
            return Err((
                IssueCode::UnknownActionType,
                format!(
                    "unknown action type {:?}; allowed: {}",
                    truncate_chars(kind, 48),
                    DirectorAction::TYPES.join(", ")
                ),
            ));
        }
        _ => {
            return Err((IssueCode::Malformed, "needs a string `type`".to_owned()));
        }
    }
    serde_json::from_value(item.clone())
        .map_err(|err| (IssueCode::Malformed, truncate_chars(&err.to_string(), 240)))
}

fn check_summary(
    issues: &mut Issues,
    reason_code: Option<ReasonCode>,
    confidence: Option<f64>,
    narrative_summary: Option<&str>,
    action_count: usize,
) {
    if let Some(confidence) = confidence
        && !(confidence.is_finite() && (0.0..=1.0).contains(&confidence))
    {
        issues.push(
            "confidence",
            IssueCode::InvalidValue,
            "must be a number from 0 to 1",
        );
    }
    if let Some(summary) = narrative_summary {
        issues.authored_text("narrative_summary", summary, limits::MAX_SUMMARY_CHARS);
    }
    if reason_code == Some(ReasonCode::NoChange) && action_count > 0 {
        issues.push(
            "reason_code",
            IssueCode::InvalidCombination,
            "no_change must come with zero actions",
        );
    }
}

fn check_actions<'a>(
    ctx: &'a DirectorContext,
    issues: &mut Issues,
    actions: impl Iterator<Item = (usize, &'a DirectorAction)> + Clone,
) {
    let mut checker = Checker::new(ctx, issues);
    // First pass: what the decision creates and invalidates, so the checks
    // below do not depend on action order.
    for (_, action) in actions.clone() {
        match action {
            DirectorAction::SetObjective { objective_id, .. } => {
                checker.created_objectives.insert(objective_id);
            }
            DirectorAction::InvalidateMission { mission_id, .. } => {
                checker.invalidated_missions.insert(mission_id);
            }
            _ => {}
        }
    }
    for (index, action) in actions {
        checker.check(index, action);
    }
}

struct Checker<'a, 'i> {
    ctx: &'a DirectorContext,
    issues: &'i mut Issues,
    characters: BTreeSet<&'a str>,
    locations: BTreeSet<&'a str>,
    created_objectives: BTreeSet<&'a str>,
    invalidated_missions: BTreeSet<&'a str>,
    action_ids: BTreeSet<&'a str>,
    set_objectives: BTreeSet<&'a str>,
    resolved_objectives: BTreeSet<&'a str>,
    seen_invalidations: BTreeSet<&'a str>,
    flags_set: BTreeSet<&'a str>,
    flags_cleared: BTreeSet<&'a str>,
    placed_npcs: BTreeSet<&'a str>,
    dispositions: BTreeSet<(&'a str, &'a str)>,
    dialogues: BTreeSet<&'a str>,
    replans: BTreeSet<Option<&'a str>>,
}

impl<'a, 'i> Checker<'a, 'i> {
    fn new(ctx: &'a DirectorContext, issues: &'i mut Issues) -> Self {
        Self {
            ctx,
            issues,
            characters: ctx.world.characters.iter().map(|c| c.id.as_str()).collect(),
            locations: ctx.world.locations.iter().map(|l| l.id.as_str()).collect(),
            created_objectives: BTreeSet::new(),
            invalidated_missions: BTreeSet::new(),
            action_ids: BTreeSet::new(),
            set_objectives: BTreeSet::new(),
            resolved_objectives: BTreeSet::new(),
            seen_invalidations: BTreeSet::new(),
            flags_set: BTreeSet::new(),
            flags_cleared: BTreeSet::new(),
            placed_npcs: BTreeSet::new(),
            dispositions: BTreeSet::new(),
            dialogues: BTreeSet::new(),
            replans: BTreeSet::new(),
        }
    }

    fn check(&mut self, index: usize, action: &'a DirectorAction) {
        let at = |field: &str| format!("actions[{index}].{field}");

        let action_id = action.action_id();
        if !is_snake_id(action_id, limits::MAX_ACTION_ID_LEN) {
            self.issues.push(
                at("action_id"),
                IssueCode::InvalidIdentifier,
                "must match ^[a-z0-9_]{1,32}$",
            );
        } else if !self.action_ids.insert(action_id) {
            self.issues.push(
                at("action_id"),
                IssueCode::DuplicateId,
                format!("action_id {action_id:?} is used more than once"),
            );
        }

        match action {
            DirectorAction::SetObjective {
                objective_id,
                title,
                description,
                mission_id,
                ..
            } => {
                if !is_snake_id(objective_id, crate::action::MAX_IDENTIFIER_LEN) {
                    self.issues.push(
                        at("objective_id"),
                        IssueCode::InvalidIdentifier,
                        "a new objective id must match ^[a-z0-9_]{1,64}$",
                    );
                } else if self.ctx.objective(objective_id).is_some() {
                    self.issues.push(
                        at("objective_id"),
                        IssueCode::DuplicateId,
                        format!("objective {objective_id:?} already exists; choose a new id"),
                    );
                } else if !self.set_objectives.insert(objective_id) {
                    self.issues.push(
                        at("objective_id"),
                        IssueCode::DuplicateId,
                        format!("objective {objective_id:?} is set more than once"),
                    );
                }
                self.issues
                    .authored_text(at("title"), title, limits::MAX_TITLE_CHARS);
                self.issues
                    .authored_text(at("description"), description, limits::MAX_TEXT_CHARS);
                if let Some(mission_id) = mission_id
                    && self.active_mission(at("mission_id"), mission_id)
                    && self.invalidated_missions.contains(mission_id.as_str())
                {
                    self.issues.push(
                        at("mission_id"),
                        IssueCode::InvalidCombination,
                        format!(
                            "mission {mission_id:?} is invalidated by this same decision; \
                             omit mission_id for a replacement objective"
                        ),
                    );
                }
            }
            DirectorAction::CompleteObjective { objective_id, .. } => {
                self.resolve_objective(at("objective_id"), objective_id);
            }
            DirectorAction::FailObjective {
                objective_id,
                reason,
                ..
            } => {
                self.resolve_objective(at("objective_id"), objective_id);
                self.issues
                    .authored_text(at("reason"), reason, limits::MAX_REASON_CHARS);
            }
            DirectorAction::ActivateNpc {
                npc_id,
                location_id,
                ..
            } => {
                if self.npc(at("npc_id"), npc_id) {
                    if self.ctx.npc(npc_id).is_some_and(|n| n.active) {
                        self.issues.push(
                            at("npc_id"),
                            IssueCode::InvalidCombination,
                            format!("{npc_id:?} is already active; use move_npc with a reason"),
                        );
                    }
                    self.place(at("npc_id"), npc_id);
                }
                self.location(at("location_id"), location_id);
            }
            DirectorAction::MoveNpc {
                npc_id,
                location_id,
                reason,
                ..
            } => {
                if self.npc(at("npc_id"), npc_id) {
                    self.place(at("npc_id"), npc_id);
                }
                self.location(at("location_id"), location_id);
                self.issues
                    .authored_text(at("reason"), reason, limits::MAX_REASON_CHARS);
            }
            DirectorAction::SetNpcDisposition {
                npc_id,
                toward,
                reason,
                ..
            } => {
                let npc_ok = self.npc(at("npc_id"), npc_id);
                let toward_ok = self.actor(at("toward"), toward);
                if npc_ok && toward_ok {
                    if npc_id == toward {
                        self.issues.push(
                            at("toward"),
                            IssueCode::InvalidCombination,
                            "a character cannot hold a disposition toward themselves",
                        );
                    } else if !self.dispositions.insert((npc_id.as_str(), toward.as_str())) {
                        self.issues.push(
                            at("npc_id"),
                            IssueCode::InvalidCombination,
                            format!("disposition of {npc_id:?} toward {toward:?} is set twice"),
                        );
                    }
                }
                self.issues
                    .authored_text(at("reason"), reason, limits::MAX_REASON_CHARS);
            }
            DirectorAction::RevealInformation {
                recipient_id,
                text,
                source_npc_id,
                ..
            } => {
                self.actor(at("recipient_id"), recipient_id);
                self.issues
                    .authored_text(at("text"), text, limits::MAX_TEXT_CHARS);
                if let Some(source) = source_npc_id
                    && self.npc(at("source_npc_id"), source)
                    && source == recipient_id
                {
                    self.issues.push(
                        at("source_npc_id"),
                        IssueCode::InvalidCombination,
                        "source and recipient are the same character",
                    );
                }
            }
            DirectorAction::SetWorldFlag { flag, .. } => {
                if self.writable_flag(at("flag"), flag) {
                    if !self.flags_set.insert(flag) {
                        self.issues.push(
                            at("flag"),
                            IssueCode::DuplicateId,
                            format!("flag {flag:?} is set more than once"),
                        );
                    } else if self.flags_cleared.contains(flag.as_str()) {
                        self.flag_conflict(at("flag"), flag);
                    }
                }
            }
            DirectorAction::ClearWorldFlag { flag, .. } => {
                if self.writable_flag(at("flag"), flag) {
                    if self.ctx.world_flags.get(flag) != Some(&true) {
                        self.issues.push(
                            at("flag"),
                            IssueCode::UnknownReference,
                            format!("flag {flag:?} is not currently set"),
                        );
                    } else if !self.flags_cleared.insert(flag) {
                        self.issues.push(
                            at("flag"),
                            IssueCode::DuplicateId,
                            format!("flag {flag:?} is cleared more than once"),
                        );
                    } else if self.flags_set.contains(flag.as_str()) {
                        self.flag_conflict(at("flag"), flag);
                    }
                }
            }
            DirectorAction::TriggerWorldEvent {
                description,
                location_id,
                npc_ids,
                ..
            } => {
                self.issues
                    .authored_text(at("description"), description, limits::MAX_TEXT_CHARS);
                if let Some(location_id) = location_id {
                    self.location(at("location_id"), location_id);
                }
                if npc_ids.len() > limits::MAX_EVENT_NPCS {
                    self.issues.push(
                        at("npc_ids"),
                        IssueCode::TooMany,
                        format!(
                            "{} NPCs; at most {} allowed",
                            npc_ids.len(),
                            limits::MAX_EVENT_NPCS
                        ),
                    );
                } else {
                    let mut seen: BTreeSet<&str> = BTreeSet::new();
                    for (i, npc_id) in npc_ids.iter().enumerate() {
                        let path = format!("actions[{index}].npc_ids[{i}]");
                        if self.npc(path.clone(), npc_id) && !seen.insert(npc_id) {
                            self.issues.push(
                                path,
                                IssueCode::DuplicateId,
                                format!("{npc_id:?} is listed more than once"),
                            );
                        }
                    }
                }
            }
            DirectorAction::StartDialogue {
                npc_id,
                opening_line,
                ..
            } => {
                if self.npc(at("npc_id"), npc_id) && !self.dialogues.insert(npc_id) {
                    self.issues.push(
                        at("npc_id"),
                        IssueCode::InvalidCombination,
                        format!("{npc_id:?} starts more than one dialogue"),
                    );
                }
                self.issues
                    .authored_text(at("opening_line"), opening_line, limits::MAX_TEXT_CHARS);
            }
            DirectorAction::InvalidateMission {
                mission_id, reason, ..
            } => {
                if self.active_mission(at("mission_id"), mission_id)
                    && !self.seen_invalidations.insert(mission_id)
                {
                    self.issues.push(
                        at("mission_id"),
                        IssueCode::DuplicateId,
                        format!("mission {mission_id:?} is invalidated more than once"),
                    );
                }
                self.issues
                    .authored_text(at("reason"), reason, limits::MAX_REASON_CHARS);
            }
            DirectorAction::RequestReplan {
                reason, mission_id, ..
            } => {
                self.issues
                    .authored_text(at("reason"), reason, limits::MAX_REASON_CHARS);
                let scope_ok = match mission_id {
                    Some(mission_id) => self.known_mission(at("mission_id"), mission_id),
                    None => true,
                };
                if scope_ok && !self.replans.insert(mission_id.as_deref()) {
                    self.issues.push(
                        format!("actions[{index}]"),
                        IssueCode::DuplicateId,
                        "the same replan is requested more than once",
                    );
                }
            }
        }
    }

    /// A known, living, non-player character.
    fn npc(&mut self, path: String, id: &str) -> bool {
        if !self.issues.identifier(path.as_str(), id) {
            return false;
        }
        if id == PLAYER_ID {
            self.issues.push(
                path,
                IssueCode::UnknownReference,
                "the player is not an NPC",
            );
            return false;
        }
        if !self.characters.contains(id) {
            self.issues.push(
                path,
                IssueCode::UnknownReference,
                format!("{id:?} is not a character in this world"),
            );
            return false;
        }
        if self.ctx.is_dead(id) && !self.ctx.allow_character_revival {
            self.issues.push(
                path,
                IssueCode::DeadCharacter,
                format!("{id:?} is dead and this world does not allow revival"),
            );
            return false;
        }
        true
    }

    /// The player or an NPC.
    fn actor(&mut self, path: String, id: &str) -> bool {
        id == PLAYER_ID || self.npc(path, id)
    }

    fn location(&mut self, path: String, id: &str) -> bool {
        self.issues.known(path, id, &self.locations, "location")
    }

    /// An NPC is placed (activated or moved) at most once per decision.
    fn place(&mut self, path: String, npc_id: &'a str) {
        if !self.placed_npcs.insert(npc_id) {
            self.issues.push(
                path,
                IssueCode::InvalidCombination,
                format!("{npc_id:?} is activated or moved more than once"),
            );
        }
    }

    fn resolve_objective(&mut self, path: String, objective_id: &'a str) {
        if !self.issues.identifier(path.as_str(), objective_id) {
            return;
        }
        if self.created_objectives.contains(objective_id) {
            self.issues.push(
                path,
                IssueCode::InvalidCombination,
                format!("objective {objective_id:?} is created by this same decision"),
            );
            return;
        }
        if self.ctx.narrative.is_some() {
            match self.ctx.objective(objective_id) {
                None => {
                    self.issues.push(
                        path,
                        IssueCode::UnknownReference,
                        format!("{objective_id:?} is not a known objective"),
                    );
                    return;
                }
                Some(objective) if objective.status != ObjectiveStatus::Active => {
                    self.issues.push(
                        path,
                        IssueCode::InvalidCombination,
                        format!("objective {objective_id:?} is no longer active"),
                    );
                    return;
                }
                Some(_) => {}
            }
        }
        if !self.resolved_objectives.insert(objective_id) {
            self.issues.push(
                path,
                IssueCode::InvalidCombination,
                format!("objective {objective_id:?} is completed or failed more than once"),
            );
        }
    }

    /// A valid mission id that, where the narrative is supplied, exists.
    fn known_mission(&mut self, path: String, mission_id: &str) -> bool {
        if !self.issues.identifier(path.as_str(), mission_id) {
            return false;
        }
        if self.ctx.narrative.is_some() && self.ctx.mission(mission_id).is_none() {
            self.issues.push(
                path,
                IssueCode::UnknownReference,
                format!("{mission_id:?} is not a known mission"),
            );
            return false;
        }
        true
    }

    fn active_mission(&mut self, path: String, mission_id: &str) -> bool {
        if !self.known_mission(path.clone(), mission_id) {
            return false;
        }
        if self
            .ctx
            .mission(mission_id)
            .is_some_and(|m| m.status != MissionStatus::Active)
        {
            self.issues.push(
                path,
                IssueCode::InvalidCombination,
                format!("mission {mission_id:?} is no longer active"),
            );
            return false;
        }
        true
    }

    fn writable_flag(&mut self, path: String, flag: &str) -> bool {
        if !is_flag_key(flag) {
            self.issues.push(
                path,
                IssueCode::InvalidIdentifier,
                "must be a 1-128 char flag key [A-Za-z0-9_.:-]",
            );
            return false;
        }
        if flag.starts_with(RESERVED_FLAG_PREFIX) {
            self.issues.push(
                path,
                IssueCode::InvalidValue,
                format!(
                    "flags starting with {RESERVED_FLAG_PREFIX:?} are reserved for gameplay rules"
                ),
            );
            return false;
        }
        true
    }

    fn flag_conflict(&mut self, path: String, flag: &str) {
        self.issues.push(
            path,
            IssueCode::InvalidCombination,
            format!("flag {flag:?} is both set and cleared"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::director::actions::Disposition;
    use crate::director::testing::{sample_context, sample_proposal};
    use serde_json::json;

    fn proposal_json(actions: Value) -> String {
        json!({
            "reason_code": "player_divergence",
            "actions": actions,
            "narrative_summary": "The player told the captain.",
            "confidence": 0.8
        })
        .to_string()
    }

    /// Issues from validating `actions` against the sample context.
    fn issues_for(actions: Value) -> Vec<ValidationIssue> {
        parse_proposal(&sample_context(), &proposal_json(actions))
            .err()
            .unwrap_or_default()
    }

    #[track_caller]
    fn assert_issue(actions: Value, path: &str, code: IssueCode) {
        let issues = issues_for(actions);
        assert!(
            issues.iter().any(|i| i.path == path && i.code == code),
            "expected {code:?} at {path}, got {issues:#?}"
        );
    }

    fn flag(action_id: &str, name: &str) -> Value {
        json!({"type": "set_world_flag", "action_id": action_id, "flag": name})
    }

    #[test]
    fn accepts_the_sample_proposal() {
        let ctx = sample_context();
        let proposal = sample_proposal();
        assert_eq!(validate_proposal(&ctx, &proposal), Ok(()));
        assert_eq!(validate_actions(&ctx, &proposal.actions), Ok(()));

        let text = serde_json::to_string(&proposal).unwrap();
        assert_eq!(parse_proposal(&ctx, &text), Ok(proposal));
    }

    #[test]
    fn accepts_zero_actions() {
        let ctx = sample_context();
        let raw = json!({"reason_code": "no_change", "actions": [], "confidence": 1}).to_string();
        let proposal = parse_proposal(&ctx, &raw).unwrap();
        assert!(proposal.actions.is_empty());
        assert_eq!(proposal.narrative_summary, None);

        // An empty or null summary is the same as no summary, not an error.
        for summary in [json!(""), json!("   "), json!(null)] {
            let raw = json!({"reason_code": "no_change", "actions": [], "confidence": 1,
                             "narrative_summary": summary})
            .to_string();
            assert_eq!(parse_proposal(&ctx, &raw).unwrap().narrative_summary, None);
        }
        let raw = json!({"reason_code": "no_change", "actions": [], "confidence": 1,
                         "narrative_summary": 7})
        .to_string();
        assert_eq!(
            parse_proposal(&ctx, &raw).unwrap_err()[0].path,
            "narrative_summary"
        );
    }

    #[test]
    fn accepts_every_action_type_in_one_valid_decision() {
        // 13 variants do not fit in one decision (max 8); two decisions cover them.
        let ctx = sample_context();
        let first = json!([
            {"type": "fail_objective", "action_id": "a1", "objective_id": "hide_ledger", "reason": "Handed over."},
            {"type": "invalidate_mission", "action_id": "a2", "mission_id": "smuggling_cover", "reason": "Exposed."},
            {"type": "set_world_flag", "action_id": "a3", "flag": "disclosed_to:captain_ines"},
            {"type": "clear_world_flag", "action_id": "a4", "flag": "ledger_hidden"},
            {"type": "set_objective", "action_id": "a5", "objective_id": "warn_the_keeper",
             "title": "Warn the keeper", "description": "Reach the lighthouse before the watch."},
            {"type": "set_npc_disposition", "action_id": "a6", "npc_id": "keeper_tomas",
             "toward": "player", "disposition": "hostile", "reason": "Betrayed."},
            {"type": "trigger_world_event", "action_id": "a7", "event": "authorities_alerted",
             "description": "The harbor watch musters.", "location_id": "harbor_office",
             "npc_ids": ["captain_ines"]},
            {"type": "request_replan", "action_id": "a8", "reason": "Cover blown.",
             "mission_id": "smuggling_cover"}
        ]);
        let second = json!([
            {"type": "complete_objective", "action_id": "a1", "objective_id": "hide_ledger"},
            {"type": "activate_npc", "action_id": "a2", "npc_id": "broker_wen", "location_id": "night_market"},
            {"type": "move_npc", "action_id": "a3", "npc_id": "captain_ines",
             "location_id": "lighthouse", "reason": "She goes to search it."},
            {"type": "reveal_information", "action_id": "a4", "recipient_id": "player",
             "text": "The ledger lists every bribe.", "source_npc_id": "captain_ines"},
            {"type": "start_dialogue", "action_id": "a5", "npc_id": "captain_ines",
             "opening_line": "Show me where he keeps it."}
        ]);
        let mut seen = BTreeSet::new();
        for actions in [first, second] {
            let proposal = parse_proposal(&ctx, &proposal_json(actions))
                .unwrap_or_else(|issues| panic!("{issues:#?}"));
            seen.extend(proposal.actions.iter().map(DirectorAction::type_name));
        }
        assert_eq!(seen.len(), DirectorAction::TYPES.len());
    }

    #[test]
    fn rejects_non_json_and_wrong_shapes() {
        let ctx = sample_context();
        for raw in [
            "",
            "I think the captain should...",
            "{\"actions\": [",
            "[]",
            "\"x\"",
            "null",
        ] {
            let issues = parse_proposal(&ctx, raw).unwrap_err();
            assert_eq!(issues.len(), 1, "{raw:?}");
            assert_eq!(issues[0].code, IssueCode::Malformed);
            assert_eq!(issues[0].path, "$");
        }
        let fenced = format!("```json\n{}\n```", proposal_json(json!([])));
        assert!(parse_proposal(&ctx, &fenced).is_err());
    }

    #[test]
    fn rejects_missing_and_unknown_top_level_fields() {
        let ctx = sample_context();
        let issues = parse_proposal(&ctx, "{}").unwrap_err();
        let paths: Vec<&str> = issues.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, ["actions", "reason_code", "confidence"]);

        let raw = json!({"reason_code": "no_change", "actions": [], "confidence": 1,
                         "session_id": "x", "run": "format c:"})
        .to_string();
        let issues = parse_proposal(&ctx, &raw).unwrap_err();
        assert!(issues.iter().all(|i| i.code == IssueCode::Malformed));
        assert_eq!(issues.len(), 2);
    }

    #[test]
    fn rejects_unknown_action_types() {
        for bad in ["execute_script", "custom_command", "eval", "spawn_item"] {
            let actions = json!([
                flag("a1", "ok_flag"),
                {"type": bad, "action_id": "a2", "command": "give_all"}
            ]);
            assert_issue(actions, "actions[1]", IssueCode::UnknownActionType);
        }
        assert_issue(
            json!([{"action_id": "a1"}]),
            "actions[0]",
            IssueCode::Malformed,
        );
        assert_issue(
            json!(["set_world_flag"]),
            "actions[0]",
            IssueCode::Malformed,
        );
    }

    #[test]
    fn rejects_unknown_fields_and_bad_enum_values_in_actions() {
        assert_issue(
            json!([{"type": "set_world_flag", "action_id": "a1", "flag": "f", "script": "x"}]),
            "actions[0]",
            IssueCode::Malformed,
        );
        assert_issue(
            json!([{"type": "set_npc_disposition", "action_id": "a1", "npc_id": "keeper_tomas",
                    "disposition": "furious", "reason": "r"}]),
            "actions[0]",
            IssueCode::Malformed,
        );
        assert_issue(
            json!([{"type": "trigger_world_event", "action_id": "a1", "event": "meteor_strike",
                    "description": "d"}]),
            "actions[0]",
            IssueCode::Malformed,
        );
    }

    #[test]
    fn rejects_too_many_actions_before_decoding_them() {
        let nine: Vec<Value> = (0..9).map(|i| json!({"type": "bogus", "n": i})).collect();
        let issues = issues_for(json!(nine));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, IssueCode::TooMany);
        assert_eq!(issues[0].path, "actions");

        let eight: Vec<Value> = (0..8)
            .map(|i| flag(&format!("a{i}"), &format!("f{i}")))
            .collect();
        assert!(issues_for(json!(eight)).is_empty());

        let ctx = sample_context();
        let typed: Vec<DirectorAction> = (0..9)
            .map(|i| DirectorAction::SetWorldFlag {
                action_id: format!("a{i}"),
                flag: format!("f{i}"),
            })
            .collect();
        assert_eq!(
            validate_actions(&ctx, &typed).unwrap_err()[0].code,
            IssueCode::TooMany
        );
    }

    #[test]
    fn rejects_invalid_identifiers() {
        assert_issue(
            json!([flag("A-1", "f")]),
            "actions[0].action_id",
            IssueCode::InvalidIdentifier,
        );
        assert_issue(
            json!([flag("", "f")]),
            "actions[0].action_id",
            IssueCode::InvalidIdentifier,
        );
        assert_issue(
            json!([flag("a1", "has space")]),
            "actions[0].flag",
            IssueCode::InvalidIdentifier,
        );
        assert_issue(
            json!([flag("a1", &"f".repeat(129))]),
            "actions[0].flag",
            IssueCode::InvalidIdentifier,
        );
        assert_issue(
            json!([{"type": "set_objective", "action_id": "a1", "objective_id": "Warn The Keeper",
                    "title": "t", "description": "d"}]),
            "actions[0].objective_id",
            IssueCode::InvalidIdentifier,
        );
        assert_issue(
            json!([{"type": "move_npc", "action_id": "a1", "npc_id": "keeper tomas; DROP",
                    "location_id": "lighthouse", "reason": "r"}]),
            "actions[0].npc_id",
            IssueCode::InvalidIdentifier,
        );
    }

    #[test]
    fn rejects_duplicate_action_ids() {
        assert_issue(
            json!([flag("a1", "one"), flag("a1", "two")]),
            "actions[1].action_id",
            IssueCode::DuplicateId,
        );
    }

    #[test]
    fn rejects_duplicate_targets() {
        assert_issue(
            json!([flag("a1", "same"), flag("a2", "same")]),
            "actions[1].flag",
            IssueCode::DuplicateId,
        );
        let objective = |id: &str| {
            json!({"type": "set_objective", "action_id": id, "objective_id": "new_goal",
                   "title": "t", "description": "d"})
        };
        assert_issue(
            json!([objective("a1"), objective("a2")]),
            "actions[1].objective_id",
            IssueCode::DuplicateId,
        );
        assert_issue(
            json!([{"type": "set_objective", "action_id": "a1", "objective_id": "hide_ledger",
                    "title": "t", "description": "d"}]),
            "actions[0].objective_id",
            IssueCode::DuplicateId,
        );
        let invalidate = |id: &str| {
            json!({"type": "invalidate_mission", "action_id": id, "mission_id": "smuggling_cover",
                   "reason": "r"})
        };
        assert_issue(
            json!([invalidate("a1"), invalidate("a2")]),
            "actions[1].mission_id",
            IssueCode::DuplicateId,
        );
        assert_issue(
            json!([{"type": "trigger_world_event", "action_id": "a1", "event": "rumor_spreads",
                    "description": "d", "npc_ids": ["captain_ines", "captain_ines"]}]),
            "actions[0].npc_ids[1]",
            IssueCode::DuplicateId,
        );
    }

    #[test]
    fn rejects_unknown_references() {
        assert_issue(
            json!([{"type": "activate_npc", "action_id": "a1", "npc_id": "gus_fring",
                    "location_id": "night_market"}]),
            "actions[0].npc_id",
            IssueCode::UnknownReference,
        );
        assert_issue(
            json!([{"type": "activate_npc", "action_id": "a1", "npc_id": "broker_wen",
                    "location_id": "the_moon"}]),
            "actions[0].location_id",
            IssueCode::UnknownReference,
        );
        assert_issue(
            json!([{"type": "start_dialogue", "action_id": "a1", "npc_id": "player",
                    "opening_line": "Hello me."}]),
            "actions[0].npc_id",
            IssueCode::UnknownReference,
        );
        assert_issue(
            json!([{"type": "complete_objective", "action_id": "a1", "objective_id": "find_treasure"}]),
            "actions[0].objective_id",
            IssueCode::UnknownReference,
        );
        assert_issue(
            json!([{"type": "invalidate_mission", "action_id": "a1", "mission_id": "ghost_mission",
                    "reason": "r"}]),
            "actions[0].mission_id",
            IssueCode::UnknownReference,
        );
        assert_issue(
            json!([{"type": "clear_world_flag", "action_id": "a1", "flag": "never_set"}]),
            "actions[0].flag",
            IssueCode::UnknownReference,
        );
        assert_issue(
            json!([{"type": "reveal_information", "action_id": "a1", "recipient_id": "the_reader",
                    "text": "t"}]),
            "actions[0].recipient_id",
            IssueCode::UnknownReference,
        );
        assert_issue(
            json!([{"type": "set_npc_disposition", "action_id": "a1", "npc_id": "keeper_tomas",
                    "toward": "nobody", "disposition": "wary", "reason": "r"}]),
            "actions[0].toward",
            IssueCode::UnknownReference,
        );
    }

    #[test]
    fn rejects_invalid_text_and_values() {
        assert_issue(
            json!([{"type": "fail_objective", "action_id": "a1", "objective_id": "hide_ledger",
                    "reason": "   "}]),
            "actions[0].reason",
            IssueCode::InvalidText,
        );
        assert_issue(
            json!([{"type": "set_objective", "action_id": "a1", "objective_id": "new_goal",
                    "title": "t".repeat(limits::MAX_TITLE_CHARS + 1), "description": "d"}]),
            "actions[0].title",
            IssueCode::InvalidText,
        );
        assert_issue(
            json!([{"type": "start_dialogue", "action_id": "a1", "npc_id": "captain_ines",
                    "opening_line": "line one\nline two"}]),
            "actions[0].opening_line",
            IssueCode::InvalidText,
        );
        assert_issue(
            json!([{"type": "trigger_world_event", "action_id": "a1", "event": "rumor_spreads",
                    "description": "d".repeat(limits::MAX_TEXT_CHARS + 1)}]),
            "actions[0].description",
            IssueCode::InvalidText,
        );
        assert_issue(
            json!([flag("a1", "interacted:desk")]),
            "actions[0].flag",
            IssueCode::InvalidValue,
        );
        assert_issue(
            json!([{"type": "clear_world_flag", "action_id": "a1", "flag": "interacted:desk"}]),
            "actions[0].flag",
            IssueCode::InvalidValue,
        );
        assert_issue(
            json!([{"type": "trigger_world_event", "action_id": "a1", "event": "rumor_spreads",
                    "description": "d",
                    "npc_ids": ["captain_ines", "keeper_tomas", "broker_wen", "captain_ines", "keeper_tomas"]}]),
            "actions[0].npc_ids",
            IssueCode::TooMany,
        );

        let ctx = sample_context();
        for confidence in [json!(1.5), json!(-0.1), json!("high"), json!(null)] {
            let raw = json!({"reason_code": "no_change", "actions": [], "confidence": confidence})
                .to_string();
            let issues = parse_proposal(&ctx, &raw).unwrap_err();
            assert_eq!(issues[0].path, "confidence", "{confidence}");
            assert_eq!(issues[0].code, IssueCode::InvalidValue);
        }
        let mut proposal = sample_proposal();
        proposal.confidence = f64::NAN;
        assert_eq!(
            validate_proposal(&ctx, &proposal).unwrap_err()[0].code,
            IssueCode::InvalidValue
        );

        let raw = json!({"reason_code": "vibes", "actions": [], "confidence": 1}).to_string();
        assert_eq!(
            parse_proposal(&ctx, &raw).unwrap_err()[0].path,
            "reason_code"
        );

        let raw = json!({"reason_code": "no_change", "actions": [], "confidence": 1,
                         "narrative_summary": "s".repeat(limits::MAX_SUMMARY_CHARS + 1)})
        .to_string();
        assert_eq!(
            parse_proposal(&ctx, &raw).unwrap_err()[0].code,
            IssueCode::InvalidText
        );
    }

    #[test]
    fn dead_characters_stay_dead() {
        let dead = |action: Value| {
            assert_issue(
                json!([action]),
                "actions[0].npc_id",
                IssueCode::DeadCharacter,
            )
        };
        dead(
            json!({"type": "activate_npc", "action_id": "a1", "npc_id": "old_marlow",
                    "location_id": "lighthouse"}),
        );
        dead(
            json!({"type": "move_npc", "action_id": "a1", "npc_id": "old_marlow",
                    "location_id": "lighthouse", "reason": "He walks in."}),
        );
        dead(
            json!({"type": "start_dialogue", "action_id": "a1", "npc_id": "old_marlow",
                    "opening_line": "I'm back."}),
        );
        dead(
            json!({"type": "set_npc_disposition", "action_id": "a1", "npc_id": "old_marlow",
                    "disposition": "hostile", "reason": "r"}),
        );
        assert_issue(
            json!([{"type": "reveal_information", "action_id": "a1", "recipient_id": "old_marlow",
                    "text": "t"}]),
            "actions[0].recipient_id",
            IssueCode::DeadCharacter,
        );

        // A universe that explicitly supports revival may do it.
        let mut ctx = sample_context();
        ctx.allow_character_revival = true;
        let raw = proposal_json(json!([{"type": "activate_npc", "action_id": "a1",
                                        "npc_id": "old_marlow", "location_id": "lighthouse"}]));
        assert!(parse_proposal(&ctx, &raw).is_ok());
    }

    #[test]
    fn rejects_invalid_combinations() {
        // Set and clear the same flag, in either order.
        assert_issue(
            json!([flag("a1", "ledger_hidden"),
                   {"type": "clear_world_flag", "action_id": "a2", "flag": "ledger_hidden"}]),
            "actions[1].flag",
            IssueCode::InvalidCombination,
        );
        assert_issue(
            json!([{"type": "clear_world_flag", "action_id": "a1", "flag": "ledger_hidden"},
                   flag("a2", "ledger_hidden")]),
            "actions[1].flag",
            IssueCode::InvalidCombination,
        );
        // Complete and fail the same objective.
        assert_issue(
            json!([{"type": "complete_objective", "action_id": "a1", "objective_id": "hide_ledger"},
                   {"type": "fail_objective", "action_id": "a2", "objective_id": "hide_ledger", "reason": "r"}]),
            "actions[1].objective_id",
            IssueCode::InvalidCombination,
        );
        // Resolve an objective that is already resolved.
        assert_issue(
            json!([{"type": "complete_objective", "action_id": "a1", "objective_id": "meet_broker"}]),
            "actions[0].objective_id",
            IssueCode::InvalidCombination,
        );
        // Resolve an objective created by the same decision, in either order.
        assert_issue(
            json!([{"type": "complete_objective", "action_id": "a1", "objective_id": "new_goal"},
                   {"type": "set_objective", "action_id": "a2", "objective_id": "new_goal",
                    "title": "t", "description": "d"}]),
            "actions[0].objective_id",
            IssueCode::InvalidCombination,
        );
        // New objective under a mission this decision invalidates, in either order.
        assert_issue(
            json!([{"type": "set_objective", "action_id": "a1", "objective_id": "new_goal",
                    "title": "t", "description": "d", "mission_id": "smuggling_cover"},
                   {"type": "invalidate_mission", "action_id": "a2", "mission_id": "smuggling_cover",
                    "reason": "r"}]),
            "actions[0].mission_id",
            IssueCode::InvalidCombination,
        );
        // New objective under a mission that already ended.
        assert_issue(
            json!([{"type": "set_objective", "action_id": "a1", "objective_id": "new_goal",
                    "title": "t", "description": "d", "mission_id": "first_delivery"}]),
            "actions[0].mission_id",
            IssueCode::InvalidCombination,
        );
        // Activate someone already in play; place the same NPC twice.
        assert_issue(
            json!([{"type": "activate_npc", "action_id": "a1", "npc_id": "captain_ines",
                    "location_id": "lighthouse"}]),
            "actions[0].npc_id",
            IssueCode::InvalidCombination,
        );
        let go = |id: &str, to: &str| {
            json!({"type": "move_npc", "action_id": id, "npc_id": "captain_ines",
                   "location_id": to, "reason": "r"})
        };
        assert_issue(
            json!([go("a1", "lighthouse"), go("a2", "night_market")]),
            "actions[1].npc_id",
            IssueCode::InvalidCombination,
        );
        // Disposition toward oneself, or set twice.
        assert_issue(
            json!([{"type": "set_npc_disposition", "action_id": "a1", "npc_id": "keeper_tomas",
                    "toward": "keeper_tomas", "disposition": "wary", "reason": "r"}]),
            "actions[0].toward",
            IssueCode::InvalidCombination,
        );
        // no_change with actions.
        let ctx = sample_context();
        let raw =
            json!({"reason_code": "no_change", "actions": [flag("a1", "f")], "confidence": 1})
                .to_string();
        let issues = parse_proposal(&ctx, &raw).unwrap_err();
        assert_eq!(issues[0].path, "reason_code");
        assert_eq!(issues[0].code, IssueCode::InvalidCombination);
    }

    #[test]
    fn references_are_format_checked_when_narrative_is_not_supplied() {
        let mut ctx = sample_context();
        ctx.narrative = None;
        ctx.trigger = crate::director::Trigger::PlayerAction;
        let ok = proposal_json(json!([
            {"type": "complete_objective", "action_id": "a1", "objective_id": "anything_goes"},
            {"type": "invalidate_mission", "action_id": "a2", "mission_id": "some_mission", "reason": "r"}
        ]));
        assert!(parse_proposal(&ctx, &ok).is_ok());
        let bad = proposal_json(json!([
            {"type": "complete_objective", "action_id": "a1", "objective_id": "not valid!"}
        ]));
        assert_eq!(
            parse_proposal(&ctx, &bad).unwrap_err()[0].code,
            IssueCode::InvalidIdentifier
        );
    }

    #[test]
    fn reports_every_problem_at_once_and_caps_the_list() {
        let issues = issues_for(json!([
            {"type": "execute_script", "action_id": "a1"},
            {"type": "activate_npc", "action_id": "a2", "npc_id": "gus_fring", "location_id": "the_moon"},
            flag("a2", "interacted:desk")
        ]));
        let found: Vec<(&str, IssueCode)> =
            issues.iter().map(|i| (i.path.as_str(), i.code)).collect();
        assert_eq!(
            found,
            [
                ("actions[0]", IssueCode::UnknownActionType),
                ("actions[1].npc_id", IssueCode::UnknownReference),
                ("actions[1].location_id", IssueCode::UnknownReference),
                ("actions[2].action_id", IssueCode::DuplicateId),
                ("actions[2].flag", IssueCode::InvalidValue),
            ]
        );

        let mut issues = Issues::default();
        for i in 0..100 {
            issues.push(format!("p{i}"), IssueCode::Malformed, "m");
        }
        assert_eq!(issues.finish().unwrap_err().len(), MAX_REPORTED_ISSUES);
    }

    #[test]
    fn validation_does_not_depend_on_wire_defaults() {
        // `toward` defaults to the player.
        let ctx = sample_context();
        let raw = proposal_json(json!([{"type": "set_npc_disposition", "action_id": "a1",
                                        "npc_id": "keeper_tomas", "disposition": "hostile",
                                        "reason": "Betrayed."}]));
        let proposal = parse_proposal(&ctx, &raw).unwrap();
        assert_eq!(
            proposal.actions[0],
            DirectorAction::SetNpcDisposition {
                action_id: "a1".into(),
                npc_id: "keeper_tomas".into(),
                toward: "player".into(),
                disposition: Disposition::Hostile,
                reason: "Betrayed.".into(),
            }
        );
    }
}
