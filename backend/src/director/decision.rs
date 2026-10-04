//! `DirectorProposal` (what a provider returns) and `DirectorDecision` V1
//! (what the engine hands out after validation).
//!
//! Identity and metadata are assigned by code. The proposal has no field
//! through which a model could set a session, a decision id or its own
//! provenance.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::DIRECTOR_SCHEMA_VERSION;
use super::actions::DirectorAction;
use super::context::{DirectorContext, Trigger};

/// Why the Director decided what it did. Chosen by the proposer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    /// Opening a scene or setting up the next beat.
    StorySetup,
    /// The player is following the story; advance it.
    ObjectiveProgress,
    /// The player refused, betrayed or went another way.
    PlayerDivergence,
    /// A character reacts to what happened.
    NpcReaction,
    /// The wider world reacts to what happened.
    WorldReaction,
    /// Nothing needs to change. Requires zero actions.
    NoChange,
}

impl ReasonCode {
    pub const ALL: [&'static str; 6] = [
        "story_setup",
        "objective_progress",
        "player_divergence",
        "npc_reaction",
        "world_reaction",
        "no_change",
    ];
}

/// A provider's proposal. Untrusted until it has passed
/// [`validate_proposal`](super::validate::validate_proposal).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectorProposal {
    pub reason_code: ReasonCode,
    pub actions: Vec<DirectorAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narrative_summary: Option<String>,
    /// The proposer's own estimate, 0.0 to 1.0.
    pub confidence: f64,
}

/// Where a decision came from. Assigned by the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionMetadata {
    /// Provider that produced the accepted proposal, e.g. `gemini`, `fallback`.
    pub provider: String,
    pub model: String,
    /// Provider calls made for the accepted proposal: 1, or 2 after a repair.
    pub attempts: u8,
    pub repaired: bool,
    pub latency_ms: u64,
    #[serde(default)]
    pub usage: BTreeMap<String, u64>,
    pub created_at: DateTime<Utc>,
    /// `DirectorContext::event_count` the decision was made for. If the
    /// session has moved on, re-validate before applying.
    pub based_on_event_count: u64,
    /// Set when the primary provider failed and the fallback answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

/// A validated decision: zero or more typed actions the integration layer may
/// apply. Every action passed validation against the context it was made for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectorDecision {
    pub schema_version: u32,
    pub decision_id: Uuid,
    pub session_id: Uuid,
    pub universe_id: String,
    pub trigger: Trigger,
    pub reason_code: ReasonCode,
    pub actions: Vec<DirectorAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narrative_summary: Option<String>,
    pub confidence: f64,
    pub metadata: DecisionMetadata,
}

impl DirectorDecision {
    /// Wrap a proposal that has already passed validation against `ctx`.
    /// Engine-only: there is no public way to build a decision from an
    /// unvalidated proposal.
    pub(crate) fn from_validated(
        ctx: &DirectorContext,
        proposal: DirectorProposal,
        metadata: DecisionMetadata,
    ) -> Self {
        Self {
            schema_version: DIRECTOR_SCHEMA_VERSION,
            decision_id: Uuid::new_v4(),
            session_id: ctx.session_id,
            universe_id: ctx.universe_id.clone(),
            trigger: ctx.trigger.clone(),
            reason_code: proposal.reason_code,
            actions: proposal.actions,
            narrative_summary: proposal.narrative_summary,
            confidence: proposal.confidence,
            metadata,
        }
    }

    /// The proposal part of this decision, e.g. to re-validate it against a
    /// newer context before applying.
    pub fn proposal(&self) -> DirectorProposal {
        DirectorProposal {
            reason_code: self.reason_code,
            actions: self.actions.clone(),
            narrative_summary: self.narrative_summary.clone(),
            confidence: self.confidence,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::director::testing::{sample_context, sample_proposal};
    use serde_json::{Value, json};

    fn metadata() -> DecisionMetadata {
        DecisionMetadata {
            provider: "scripted".into(),
            model: "fixture".into(),
            attempts: 1,
            repaired: false,
            latency_ms: 12,
            usage: BTreeMap::from([("total_tokens".to_owned(), 420)]),
            created_at: "2026-10-03T12:00:00Z".parse().unwrap(),
            based_on_event_count: 5,
            fallback_reason: None,
        }
    }

    #[test]
    fn decision_serializes_to_the_v1_shape() {
        let ctx = sample_context();
        let decision = DirectorDecision::from_validated(&ctx, sample_proposal(), metadata());
        let value = serde_json::to_value(&decision).unwrap();

        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["session_id"], json!(ctx.session_id));
        assert_eq!(value["universe_id"], "lantern_bay");
        assert_eq!(value["trigger"]["kind"], "player_disclosure");
        assert_eq!(value["reason_code"], "player_divergence");
        assert_eq!(value["actions"][0]["type"], "fail_objective");
        assert_eq!(value["actions"][0]["action_id"], "a1");
        assert_eq!(value["metadata"]["provider"], "scripted");
        assert_eq!(value["metadata"]["attempts"], 1);
        assert_eq!(value["metadata"]["based_on_event_count"], 5);
        assert_eq!(value["metadata"]["created_at"], "2026-10-03T12:00:00Z");
        assert!(value["metadata"].get("fallback_reason").is_none());
        assert!(
            value["decision_id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .is_ok()
        );

        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "actions",
                "confidence",
                "decision_id",
                "metadata",
                "narrative_summary",
                "reason_code",
                "schema_version",
                "session_id",
                "trigger",
                "universe_id"
            ]
        );
    }

    #[test]
    fn decision_round_trips() {
        let decision =
            DirectorDecision::from_validated(&sample_context(), sample_proposal(), metadata());
        let text = serde_json::to_string(&decision).unwrap();
        let back: DirectorDecision = serde_json::from_str(&text).unwrap();
        assert_eq!(back, decision);
        assert_eq!(back.proposal(), sample_proposal());
        assert!(!back.is_empty());
    }

    #[test]
    fn decision_decoding_is_strict() {
        let decision =
            DirectorDecision::from_validated(&sample_context(), sample_proposal(), metadata());
        let mut value = serde_json::to_value(&decision).unwrap();
        value["exec"] = json!("rm -rf /");
        assert!(serde_json::from_value::<DirectorDecision>(value).is_err());

        let mut value = serde_json::to_value(&decision).unwrap();
        value["actions"][0]["type"] = json!("execute_script");
        assert!(serde_json::from_value::<DirectorDecision>(value).is_err());
    }

    #[test]
    fn proposal_cannot_carry_identity() {
        let mut value: Value = serde_json::to_value(sample_proposal()).unwrap();
        assert!(serde_json::from_value::<DirectorProposal>(value.clone()).is_ok());
        value["session_id"] = json!(Uuid::new_v4());
        assert!(serde_json::from_value::<DirectorProposal>(value).is_err());
    }

    #[test]
    fn reason_codes_match_the_list() {
        for name in ReasonCode::ALL {
            assert!(serde_json::from_value::<ReasonCode>(json!(name)).is_ok());
        }
        assert!(serde_json::from_value::<ReasonCode>(json!("because")).is_err());
    }
}
