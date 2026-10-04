//! Deterministic Director: fixed rules, no LLM, no I/O.
//!
//! It exists so the rest of Rift can be developed, tested and demoed without
//! a live model. It does not try to be clever: it reads only the structured
//! parts of the context (the trigger, the narrative view, NPC views) and never
//! interprets free text. It knows nothing about any particular universe.
//!
//! Its proposals go through the same validation as any other provider's.

use std::collections::BTreeMap;

use futures_util::future::BoxFuture;

use super::actions::{DirectorAction, Disposition, WorldEventKind};
use super::context::{DirectorContext, MissionStatus, ObjectiveStatus, ObjectiveView, Trigger};
use super::decision::{DirectorProposal, ReasonCode};
use super::provider::{DirectorProvider, ProviderError, ProviderOutput, ProviderRequest};
use super::{PLAYER_ID, limits, truncate_chars};

/// The rule set version, recorded as the "model" of fallback decisions.
pub const RULES_VERSION: &str = "rules-v1";

/// Id of the objective the fallback derives from the WorldBible's opening conflict.
pub const OPENING_OBJECTIVE_ID: &str = "opening_goal";

#[derive(Debug, Clone, Copy, Default)]
pub struct FallbackDirector;

impl FallbackDirector {
    pub const NAME: &'static str = "fallback";
}

impl DirectorProvider for FallbackDirector {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn propose<'a>(
        &'a self,
        request: ProviderRequest<'a>,
    ) -> BoxFuture<'a, Result<ProviderOutput, ProviderError>> {
        let proposal = propose(request.context);
        Box::pin(async move {
            // A proposal is plain data; serialization cannot fail.
            let text = serde_json::to_string(&proposal).unwrap_or_default();
            Ok(ProviderOutput {
                text,
                model: RULES_VERSION.to_owned(),
                usage: BTreeMap::new(),
                provider: None,
            })
        })
    }
}

/// The deterministic proposal for `ctx`. Same context, same proposal.
pub fn propose(ctx: &DirectorContext) -> DirectorProposal {
    let mut plan = Plan::new(ctx);
    let reason_code = match &ctx.trigger {
        Trigger::SessionStart => plan.open_scene(),
        Trigger::ObjectiveRefused { objective_id } => plan.diverge(Some(objective_id), None),
        Trigger::PlayerDisclosure {
            npc_id,
            objective_id,
        } => plan.diverge(objective_id.as_deref(), Some(npc_id)),
        Trigger::ObjectiveCompleted { objective_id } => plan.resolve(objective_id, true),
        Trigger::ObjectiveFailed { objective_id } => plan.resolve(objective_id, false),
        Trigger::PlayerAction | Trigger::Idle => ReasonCode::NoChange,
    };
    plan.finish(reason_code)
}

struct Plan<'a> {
    ctx: &'a DirectorContext,
    actions: Vec<DirectorAction>,
    summary: Option<String>,
}

impl<'a> Plan<'a> {
    fn new(ctx: &'a DirectorContext) -> Self {
        Self {
            ctx,
            actions: Vec::new(),
            summary: None,
        }
    }

    fn next_id(&self) -> String {
        format!("a{}", self.actions.len() + 1)
    }

    fn push(&mut self, build: impl FnOnce(String) -> DirectorAction) {
        if self.actions.len() < limits::MAX_ACTIONS {
            let action = build(self.next_id());
            self.actions.push(action);
        }
    }

    fn finish(self, reason_code: ReasonCode) -> DirectorProposal {
        let reason_code = if self.actions.is_empty() {
            ReasonCode::NoChange
        } else {
            reason_code
        };
        DirectorProposal {
            reason_code,
            narrative_summary: self.summary.filter(|_| !self.actions.is_empty()),
            actions: self.actions,
            confidence: 1.0,
        }
    }

    /// A character the Director may act on: known and not dead.
    fn usable_npc(&self, id: &str) -> bool {
        id != PLAYER_ID
            && self.ctx.world.character(id).is_some()
            && (self.ctx.allow_character_revival || !self.ctx.is_dead(id))
    }

    fn name_of(&self, id: &str) -> String {
        self.ctx
            .world
            .character(id)
            .map_or_else(|| id.to_owned(), |c| c.name.clone())
    }

    fn active_objective(&self, id: &str) -> Option<&'a ObjectiveView> {
        self.ctx
            .objective(id)
            .filter(|o| o.status == ObjectiveStatus::Active)
    }

    /// Whether `id` may be completed or failed: active where the narrative is
    /// supplied, otherwise taken on trust (it is format-checked later).
    fn resolvable(&self, id: &str) -> bool {
        self.ctx.narrative.is_none() || self.active_objective(id).is_some()
    }

    fn active_mission_of(&self, objective: Option<&ObjectiveView>) -> Option<String> {
        let mission_id = objective?.mission_id.as_deref()?;
        self.ctx
            .mission(mission_id)
            .filter(|m| m.status == MissionStatus::Active)
            .map(|m| m.mission_id.clone())
    }

    /// A new objective id derived from `base` that does not collide with an
    /// existing objective.
    fn fresh_objective_id(&self, prefix: &str, base: &str) -> Option<String> {
        let slug: String = format!("{prefix}_{base}")
            .to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(crate::action::MAX_IDENTIFIER_LEN - 3)
            .collect();
        std::iter::once(slug.clone())
            .chain((2..=9).map(|n| format!("{slug}_{n}")))
            .find(|candidate| self.ctx.objective(candidate).is_none())
    }

    fn open_scene(&mut self) -> ReasonCode {
        let ctx = self.ctx;
        let Some(opening) = &ctx.world.opening else {
            return ReasonCode::NoChange;
        };
        if ctx.objective(OPENING_OBJECTIVE_ID).is_none() {
            self.push(|action_id| DirectorAction::SetObjective {
                action_id,
                objective_id: OPENING_OBJECTIVE_ID.to_owned(),
                title: truncate_chars(&opening.title, limits::MAX_TITLE_CHARS),
                description: truncate_chars(&opening.immediate_goal, limits::MAX_TEXT_CHARS),
                mission_id: None,
            });
        }
        if ctx.world.location(&opening.location_id).is_some() {
            let cast: Vec<&String> = opening
                .involved_character_ids
                .iter()
                .filter(|id| self.usable_npc(id) && !ctx.npc(id).is_some_and(|n| n.active))
                .take(2)
                .collect();
            for npc_id in cast {
                self.push(|action_id| DirectorAction::ActivateNpc {
                    action_id,
                    npc_id: npc_id.clone(),
                    location_id: opening.location_id.clone(),
                });
            }
        }
        self.summary = Some(truncate_chars(
            &format!("Opening scene: {}", opening.title),
            limits::MAX_SUMMARY_CHARS,
        ));
        ReasonCode::StorySetup
    }

    /// The player refused an objective, or told `told_npc` about it.
    fn diverge(&mut self, objective_id: Option<&str>, told_npc: Option<&str>) -> ReasonCode {
        let ctx = self.ctx;
        let objective = objective_id.and_then(|id| self.active_objective(id));
        let mission_id = self.active_mission_of(objective);
        let giver = objective
            .and_then(|o| o.giver_npc_id.as_deref())
            .filter(|id| self.usable_npc(id) && Some(*id) != told_npc);
        let told = told_npc.filter(|id| self.usable_npc(id));
        let told_name = told.map(|id| self.name_of(id));
        let giver_name = giver.map(|id| self.name_of(id));

        let (did, reason) = match &told_name {
            Some(name) => (
                format!("told {name} instead of doing what was asked"),
                format!("The player told {name} about it."),
            ),
            None => (
                "refused what was asked".to_owned(),
                "The player refused to do it.".to_owned(),
            ),
        };
        let reason = truncate_chars(&reason, limits::MAX_REASON_CHARS);

        if let Some(objective_id) = objective_id.filter(|id| self.resolvable(id)) {
            self.push(|action_id| DirectorAction::FailObjective {
                action_id,
                objective_id: objective_id.to_owned(),
                reason: reason.clone(),
            });
        }
        if let Some(mission_id) = &mission_id {
            self.push(|action_id| DirectorAction::InvalidateMission {
                action_id,
                mission_id: mission_id.clone(),
                reason: reason.clone(),
            });
        }

        let flag = match (told, objective_id) {
            (Some(npc_id), _) => Some(format!("disclosed_to:{npc_id}")),
            (None, Some(objective_id)) => Some(format!("objective_refused:{objective_id}")),
            (None, None) => None,
        };
        if let Some(flag) = flag.filter(|f| f.len() <= limits::MAX_FLAG_KEY_LEN) {
            self.push(|action_id| DirectorAction::SetWorldFlag { action_id, flag });
        }

        if let Some(giver) = giver {
            let disposition = if told.is_some() {
                Disposition::Hostile
            } else {
                Disposition::Wary
            };
            self.push(|action_id| DirectorAction::SetNpcDisposition {
                action_id,
                npc_id: giver.to_owned(),
                toward: PLAYER_ID.to_owned(),
                disposition,
                reason: reason.clone(),
            });
        }
        if let (Some(told), Some(giver)) = (told, giver) {
            self.push(|action_id| DirectorAction::SetNpcDisposition {
                action_id,
                npc_id: told.to_owned(),
                toward: giver.to_owned(),
                disposition: Disposition::Wary,
                reason: "Acting on what the player disclosed.".to_owned(),
            });
        }

        let involved: Vec<String> = [told, giver]
            .into_iter()
            .flatten()
            .map(str::to_owned)
            .collect();
        let description = match (&told_name, &giver_name) {
            (Some(told), Some(giver)) => {
                format!("{told} now knows what {giver} wanted kept quiet.")
            }
            (Some(told), None) => format!("{told} now knows what the player was hiding."),
            (None, Some(giver)) => format!("{giver} learns the player will not go along."),
            (None, None) => "Word of the player's choice begins to spread.".to_owned(),
        };
        let here = ctx
            .player
            .location
            .as_deref()
            .filter(|id| ctx.world.location(id).is_some())
            .map(str::to_owned);
        // The one telemetry rule. A player who is under pressure, or who has
        // been playing by talking, gets a conversation here instead of another
        // escalation: same slot, lower intensity.
        let speaker = told.or(giver).filter(|_| {
            ctx.telemetry
                .is_some_and(|telemetry| telemetry.prefers_dialogue())
        });
        match speaker {
            Some(npc_id) => {
                let opening_line = if told == Some(npc_id) {
                    "Slow down. Tell me exactly what you know, from the start."
                } else {
                    "Before this goes any further, you and I need to talk."
                };
                self.push(|action_id| DirectorAction::StartDialogue {
                    action_id,
                    npc_id: npc_id.to_owned(),
                    opening_line: opening_line.to_owned(),
                });
            }
            None => self.push(|action_id| DirectorAction::TriggerWorldEvent {
                action_id,
                event: if told.is_some() {
                    WorldEventKind::RumorSpreads
                } else {
                    WorldEventKind::TensionEscalates
                },
                description: truncate_chars(&description, limits::MAX_TEXT_CHARS),
                location_id: here,
                npc_ids: involved,
            }),
        }

        let base = objective_id.or(told).unwrap_or("choice");
        if let Some(new_id) = self.fresh_objective_id("aftermath", base) {
            let description = match (&told_name, &giver_name) {
                (Some(told), Some(giver)) => format!(
                    "You told {told} instead of doing what {giver} asked. Decide whether you stand with {told} or {giver} before they act."
                ),
                (Some(told), None) => {
                    format!("{told} knows now. Decide what you do before they act on it.")
                }
                (None, Some(giver)) => {
                    format!("You refused what {giver} asked. Find out what that costs you.")
                }
                (None, None) => "You went your own way. Find out what that costs you.".to_owned(),
            };
            self.push(|action_id| DirectorAction::SetObjective {
                action_id,
                objective_id: new_id,
                title: "Deal with the fallout".to_owned(),
                description: truncate_chars(&description, limits::MAX_TEXT_CHARS),
                mission_id: None,
            });
        }

        self.push(|action_id| DirectorAction::RequestReplan {
            action_id,
            reason: truncate_chars(
                &format!("The player {did}; the plan no longer fits."),
                limits::MAX_REASON_CHARS,
            ),
            mission_id,
        });

        self.summary = Some(truncate_chars(
            &format!("The player {did}. The world registers the divergence."),
            limits::MAX_SUMMARY_CHARS,
        ));
        ReasonCode::PlayerDivergence
    }

    /// The narrative engine reports an objective as completed or failed.
    fn resolve(&mut self, objective_id: &str, completed: bool) -> ReasonCode {
        let objective = self.active_objective(objective_id);
        let mission_id = self.active_mission_of(objective);
        if !self.resolvable(objective_id) {
            return ReasonCode::NoChange;
        }
        let outcome = if completed { "completed" } else { "failed" };
        self.push(|action_id| {
            if completed {
                DirectorAction::CompleteObjective {
                    action_id,
                    objective_id: objective_id.to_owned(),
                }
            } else {
                DirectorAction::FailObjective {
                    action_id,
                    objective_id: objective_id.to_owned(),
                    reason: "The objective can no longer be achieved.".to_owned(),
                }
            }
        });
        self.push(|action_id| DirectorAction::RequestReplan {
            action_id,
            reason: format!("An objective was {outcome}; plan the next beat."),
            mission_id,
        });
        self.summary = Some(format!("The player {outcome} an objective."));
        ReasonCode::ObjectiveProgress
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::director::PlayerTelemetry;
    use crate::director::testing::sample_context;
    use crate::director::validate::validate_proposal;

    fn types(proposal: &DirectorProposal) -> Vec<&'static str> {
        proposal
            .actions
            .iter()
            .map(DirectorAction::type_name)
            .collect()
    }

    #[track_caller]
    fn valid(ctx: &DirectorContext) -> DirectorProposal {
        assert_eq!(ctx.validate(), Ok(()), "context");
        let proposal = propose(ctx);
        assert_eq!(validate_proposal(ctx, &proposal), Ok(()), "{proposal:#?}");
        proposal
    }

    #[test]
    fn is_deterministic() {
        let ctx = sample_context();
        assert_eq!(propose(&ctx), propose(&ctx));
        assert_eq!(
            serde_json::to_string(&propose(&ctx)).unwrap(),
            serde_json::to_string(&propose(&ctx.clone())).unwrap()
        );
    }

    #[test]
    fn disclosure_produces_the_full_divergence_response() {
        let ctx = sample_context();
        let proposal = valid(&ctx);
        assert_eq!(proposal.reason_code, ReasonCode::PlayerDivergence);
        assert_eq!(
            types(&proposal),
            [
                "fail_objective",
                "invalidate_mission",
                "set_world_flag",
                "set_npc_disposition",
                "set_npc_disposition",
                "trigger_world_event",
                "set_objective",
                "request_replan",
            ]
        );
        assert!(
            proposal
                .actions
                .contains(&DirectorAction::InvalidateMission {
                    action_id: "a2".into(),
                    mission_id: "smuggling_cover".into(),
                    reason: "The player told Captain Ines about it.".into(),
                })
        );
        assert!(proposal.actions.contains(&DirectorAction::SetWorldFlag {
            action_id: "a3".into(),
            flag: "disclosed_to:captain_ines".into(),
        }));
        assert!(
            proposal
                .actions
                .contains(&DirectorAction::SetNpcDisposition {
                    action_id: "a4".into(),
                    npc_id: "keeper_tomas".into(),
                    toward: "player".into(),
                    disposition: Disposition::Hostile,
                    reason: "The player told Captain Ines about it.".into(),
                })
        );
        assert!(matches!(
            &proposal.actions[6],
            DirectorAction::SetObjective { objective_id, mission_id: None, .. }
                if objective_id == "aftermath_hide_ledger"
        ));
        assert!(proposal.narrative_summary.is_some());
    }

    #[test]
    fn refusal_fails_the_objective_and_invalidates_its_mission() {
        let mut ctx = sample_context();
        ctx.trigger = Trigger::ObjectiveRefused {
            objective_id: "hide_ledger".into(),
        };
        let proposal = valid(&ctx);
        assert_eq!(proposal.reason_code, ReasonCode::PlayerDivergence);
        assert_eq!(
            types(&proposal),
            [
                "fail_objective",
                "invalidate_mission",
                "set_world_flag",
                "set_npc_disposition",
                "trigger_world_event",
                "set_objective",
                "request_replan",
            ]
        );
        assert!(proposal.actions.contains(&DirectorAction::SetWorldFlag {
            action_id: "a3".into(),
            flag: "objective_refused:hide_ledger".into(),
        }));
        assert!(matches!(
            &proposal.actions[3],
            DirectorAction::SetNpcDisposition {
                disposition: Disposition::Wary,
                ..
            }
        ));
    }

    #[test]
    fn session_start_sets_the_opening_objective_and_cast() {
        let mut ctx = sample_context();
        ctx.trigger = Trigger::SessionStart;
        ctx.narrative = None;
        ctx.npcs.clear();
        let proposal = valid(&ctx);
        assert_eq!(proposal.reason_code, ReasonCode::StorySetup);
        assert_eq!(
            types(&proposal),
            ["set_objective", "activate_npc", "activate_npc"]
        );
        assert!(matches!(
            &proposal.actions[0],
            DirectorAction::SetObjective { objective_id, title, .. }
                if objective_id == "opening_goal" && title == "The Ledger"
        ));
        assert!(matches!(
            &proposal.actions[1],
            DirectorAction::ActivateNpc { npc_id, location_id, .. }
                if npc_id == "keeper_tomas" && location_id == "harbor_office"
        ));

        // Characters already in play are not activated again.
        let mut ctx = sample_context();
        ctx.trigger = Trigger::SessionStart;
        assert_eq!(types(&valid(&ctx)), ["set_objective"]);
    }

    #[test]
    fn completion_and_failure_follow_the_narrative_engine() {
        let mut ctx = sample_context();
        ctx.trigger = Trigger::ObjectiveCompleted {
            objective_id: "hide_ledger".into(),
        };
        let proposal = valid(&ctx);
        assert_eq!(proposal.reason_code, ReasonCode::ObjectiveProgress);
        assert_eq!(types(&proposal), ["complete_objective", "request_replan"]);

        ctx.trigger = Trigger::ObjectiveFailed {
            objective_id: "hide_ledger".into(),
        };
        assert_eq!(types(&valid(&ctx)), ["fail_objective", "request_replan"]);

        // Already resolved: nothing to do.
        ctx.trigger = Trigger::ObjectiveCompleted {
            objective_id: "meet_broker".into(),
        };
        let proposal = valid(&ctx);
        assert_eq!(proposal.reason_code, ReasonCode::NoChange);
        assert!(proposal.actions.is_empty());
        assert_eq!(proposal.narrative_summary, None);
    }

    fn telemetry(combat: u8, deaths: u8, engagement: u8) -> PlayerTelemetry {
        PlayerTelemetry {
            window_seconds: 300,
            combat_intensity: combat,
            recent_deaths: deaths,
            npc_engagement: engagement,
            exploration_activity: 0,
        }
    }

    #[test]
    fn quiet_telemetry_changes_nothing() {
        let baseline = propose(&sample_context());
        for quiet in [telemetry(0, 0, 0), telemetry(59, 1, 59)] {
            let mut ctx = sample_context();
            ctx.telemetry = Some(quiet);
            assert_eq!(valid(&ctx), baseline);
        }
    }

    #[test]
    fn pressure_or_engagement_swaps_the_escalation_for_a_conversation() {
        for busy in [
            telemetry(60, 0, 0),
            telemetry(0, 2, 0),
            telemetry(0, 0, 60),
            telemetry(100, 100, 100),
        ] {
            let mut ctx = sample_context();
            ctx.telemetry = Some(busy);
            let proposal = valid(&ctx);
            assert_eq!(proposal.reason_code, ReasonCode::PlayerDivergence);
            assert_eq!(
                types(&proposal),
                [
                    "fail_objective",
                    "invalidate_mission",
                    "set_world_flag",
                    "set_npc_disposition",
                    "set_npc_disposition",
                    "start_dialogue",
                    "set_objective",
                    "request_replan",
                ]
            );
            // The NPC the player told is the one who speaks.
            assert!(proposal.actions.iter().any(|a| matches!(
                a,
                DirectorAction::StartDialogue { npc_id, .. } if npc_id == "captain_ines"
            )));
        }
    }

    #[test]
    fn a_refusal_under_pressure_is_answered_by_the_giver() {
        let mut ctx = sample_context();
        ctx.trigger = Trigger::ObjectiveRefused {
            objective_id: "hide_ledger".to_owned(),
        };
        assert!(types(&valid(&ctx)).contains(&"trigger_world_event"));

        ctx.telemetry = Some(telemetry(80, 0, 0));
        let proposal = valid(&ctx);
        assert!(!types(&proposal).contains(&"trigger_world_event"));
        assert!(proposal.actions.iter().any(|a| matches!(
            a,
            DirectorAction::StartDialogue { npc_id, .. } if npc_id == "keeper_tomas"
        )));
    }

    #[test]
    fn with_nobody_to_talk_to_the_world_event_stays() {
        let mut ctx = sample_context();
        ctx.telemetry = Some(telemetry(100, 5, 100));
        ctx.trigger = Trigger::PlayerDisclosure {
            npc_id: "old_marlow".to_owned(),
            objective_id: None,
        };
        let proposal = valid(&ctx);
        assert!(types(&proposal).contains(&"trigger_world_event"));
        assert!(!types(&proposal).contains(&"start_dialogue"));
    }

    #[test]
    fn plain_actions_and_idle_change_nothing() {
        for trigger in [Trigger::PlayerAction, Trigger::Idle] {
            let mut ctx = sample_context();
            ctx.trigger = trigger;
            let proposal = valid(&ctx);
            assert_eq!(proposal.reason_code, ReasonCode::NoChange);
            assert!(proposal.actions.is_empty());
        }
    }

    #[test]
    fn stays_valid_when_views_are_missing_or_characters_are_dead() {
        // No narrative or NPC views at all.
        let mut ctx = sample_context();
        ctx.narrative = None;
        ctx.npcs.clear();
        let proposal = valid(&ctx);
        assert_eq!(
            types(&proposal),
            [
                "fail_objective",
                "set_world_flag",
                "trigger_world_event",
                "set_objective",
                "request_replan"
            ]
        );

        // The objective's giver is dead: no disposition change for them.
        let mut ctx = sample_context();
        let giver = ctx
            .npcs
            .iter_mut()
            .find(|n| n.npc_id == "keeper_tomas")
            .unwrap();
        giver.alive = false;
        giver.active = false;
        let proposal = valid(&ctx);
        assert!(!types(&proposal).contains(&"set_npc_disposition"));

        // Disclosure with no objective named.
        let mut ctx = sample_context();
        ctx.trigger = Trigger::PlayerDisclosure {
            npc_id: "captain_ines".into(),
            objective_id: None,
        };
        let proposal = valid(&ctx);
        assert_eq!(
            types(&proposal),
            [
                "set_world_flag",
                "trigger_world_event",
                "set_objective",
                "request_replan"
            ]
        );

        // The player is somewhere the WorldBible does not know.
        let mut ctx = sample_context();
        ctx.player.location = Some("test_door".into());
        let proposal = valid(&ctx);
        assert!(proposal.actions.iter().any(|a| matches!(
            a,
            DirectorAction::TriggerWorldEvent {
                location_id: None,
                ..
            }
        )));
    }

    #[test]
    fn replacement_objective_ids_do_not_collide() {
        let mut ctx = sample_context();
        let narrative = ctx.narrative.as_mut().unwrap();
        let mut taken = narrative.objectives[1].clone();
        taken.objective_id = "aftermath_hide_ledger".into();
        narrative.objectives.push(taken);
        let proposal = valid(&ctx);
        assert!(proposal.actions.iter().any(|a| matches!(
            a,
            DirectorAction::SetObjective { objective_id, .. } if objective_id == "aftermath_hide_ledger_2"
        )));
    }

    #[tokio::test]
    async fn provider_returns_the_proposal_as_json() {
        let ctx = sample_context();
        let output = FallbackDirector
            .propose(ProviderRequest {
                context: &ctx,
                repair: None,
            })
            .await
            .unwrap();
        assert_eq!(output.model, RULES_VERSION);
        let parsed: DirectorProposal = serde_json::from_str(&output.text).unwrap();
        assert_eq!(parsed, propose(&ctx));
        assert_eq!(FallbackDirector.name(), "fallback");
    }
}
