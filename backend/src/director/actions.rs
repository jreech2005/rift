//! The Director's action vocabulary: a closed, typed allowlist.
//!
//! A `DirectorAction` is inert data. There is deliberately no variant that
//! carries code, a script, a console command or an untyped payload, and
//! decoding is strict: an unknown `type` or an unknown field is an error.

use serde::{Deserialize, Serialize};

use super::PLAYER_ID;

/// How one character feels about another. Same wire strings as the NPC
/// layer's coarse disposition, so integration is a one-to-one mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Hostile,
    Wary,
    Neutral,
    Friendly,
    Loyal,
}

impl Disposition {
    pub const ALL: [&'static str; 5] = ["hostile", "wary", "neutral", "friendly", "loyal"];
}

/// World events the Director may trigger. Universe-independent on purpose:
/// the kind tells the client what to stage, the description says what it is
/// in this story.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorldEventKind {
    AlarmRaised,
    AuthoritiesAlerted,
    ReinforcementsArrive,
    RumorSpreads,
    CommunicationReceived,
    ConfrontationBegins,
    EnvironmentChanges,
    TensionEscalates,
}

impl WorldEventKind {
    pub const ALL: [&'static str; 8] = [
        "alarm_raised",
        "authorities_alerted",
        "reinforcements_arrive",
        "rumor_spreads",
        "communication_received",
        "confrontation_begins",
        "environment_changes",
        "tension_escalates",
    ];
}

fn player() -> String {
    PLAYER_ID.to_owned()
}

/// One proposed change to the world. Wire form is internally tagged:
/// `{"type": "set_world_flag", "action_id": "a1", "flag": "..."}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum DirectorAction {
    /// Give the player a new objective.
    SetObjective {
        action_id: String,
        objective_id: String,
        title: String,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mission_id: Option<String>,
    },
    /// Mark an active objective as completed.
    CompleteObjective {
        action_id: String,
        objective_id: String,
    },
    /// Mark an active objective as failed.
    FailObjective {
        action_id: String,
        objective_id: String,
        reason: String,
    },
    /// Bring a known character into play at a location ("spawn or activate").
    ActivateNpc {
        action_id: String,
        npc_id: String,
        location_id: String,
    },
    /// Move an NPC. A reason is mandatory: no unexplained teleports.
    MoveNpc {
        action_id: String,
        npc_id: String,
        location_id: String,
        reason: String,
    },
    /// Change how an NPC feels about the player or another character.
    SetNpcDisposition {
        action_id: String,
        npc_id: String,
        #[serde(default = "player")]
        toward: String,
        disposition: Disposition,
        reason: String,
    },
    /// Let the player or an NPC learn something.
    RevealInformation {
        action_id: String,
        recipient_id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_npc_id: Option<String>,
    },
    SetWorldFlag {
        action_id: String,
        flag: String,
    },
    ClearWorldFlag {
        action_id: String,
        flag: String,
    },
    /// Stage one of the allowlisted world events.
    TriggerWorldEvent {
        action_id: String,
        event: WorldEventKind,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        location_id: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        npc_ids: Vec<String>,
    },
    /// An NPC opens a conversation with the player.
    StartDialogue {
        action_id: String,
        npc_id: String,
        opening_line: String,
    },
    /// The player's actions made a mission impossible or meaningless.
    InvalidateMission {
        action_id: String,
        mission_id: String,
        reason: String,
    },
    /// Ask the narrative engine to plan again.
    RequestReplan {
        action_id: String,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mission_id: Option<String>,
    },
}

impl DirectorAction {
    /// Every allowed `type`, in declaration order.
    pub const TYPES: [&'static str; 13] = [
        "set_objective",
        "complete_objective",
        "fail_objective",
        "activate_npc",
        "move_npc",
        "set_npc_disposition",
        "reveal_information",
        "set_world_flag",
        "clear_world_flag",
        "trigger_world_event",
        "start_dialogue",
        "invalidate_mission",
        "request_replan",
    ];

    pub fn type_name(&self) -> &'static str {
        match self {
            Self::SetObjective { .. } => "set_objective",
            Self::CompleteObjective { .. } => "complete_objective",
            Self::FailObjective { .. } => "fail_objective",
            Self::ActivateNpc { .. } => "activate_npc",
            Self::MoveNpc { .. } => "move_npc",
            Self::SetNpcDisposition { .. } => "set_npc_disposition",
            Self::RevealInformation { .. } => "reveal_information",
            Self::SetWorldFlag { .. } => "set_world_flag",
            Self::ClearWorldFlag { .. } => "clear_world_flag",
            Self::TriggerWorldEvent { .. } => "trigger_world_event",
            Self::StartDialogue { .. } => "start_dialogue",
            Self::InvalidateMission { .. } => "invalidate_mission",
            Self::RequestReplan { .. } => "request_replan",
        }
    }

    pub fn action_id(&self) -> &str {
        match self {
            Self::SetObjective { action_id, .. }
            | Self::CompleteObjective { action_id, .. }
            | Self::FailObjective { action_id, .. }
            | Self::ActivateNpc { action_id, .. }
            | Self::MoveNpc { action_id, .. }
            | Self::SetNpcDisposition { action_id, .. }
            | Self::RevealInformation { action_id, .. }
            | Self::SetWorldFlag { action_id, .. }
            | Self::ClearWorldFlag { action_id, .. }
            | Self::TriggerWorldEvent { action_id, .. }
            | Self::StartDialogue { action_id, .. }
            | Self::InvalidateMission { action_id, .. }
            | Self::RequestReplan { action_id, .. } => action_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// One wire example per variant, in `TYPES` order.
    fn examples() -> Vec<Value> {
        vec![
            json!({"type": "set_objective", "action_id": "a1", "objective_id": "warn_the_keeper",
                   "title": "Warn the keeper", "description": "Reach the lighthouse first.",
                   "mission_id": "smuggling_cover"}),
            json!({"type": "complete_objective", "action_id": "a2", "objective_id": "hide_ledger"}),
            json!({"type": "fail_objective", "action_id": "a3", "objective_id": "hide_ledger",
                   "reason": "The player handed it over."}),
            json!({"type": "activate_npc", "action_id": "a4", "npc_id": "broker_wen",
                   "location_id": "night_market"}),
            json!({"type": "move_npc", "action_id": "a5", "npc_id": "captain_ines",
                   "location_id": "lighthouse", "reason": "She goes to search it."}),
            json!({"type": "set_npc_disposition", "action_id": "a6", "npc_id": "keeper_tomas",
                   "toward": "player", "disposition": "hostile", "reason": "He was betrayed."}),
            json!({"type": "reveal_information", "action_id": "a7", "recipient_id": "player",
                   "text": "The ledger lists every bribe.", "source_npc_id": "captain_ines"}),
            json!({"type": "set_world_flag", "action_id": "a8", "flag": "disclosed_to:captain_ines"}),
            json!({"type": "clear_world_flag", "action_id": "a9", "flag": "ledger_hidden"}),
            json!({"type": "trigger_world_event", "action_id": "a10", "event": "authorities_alerted",
                   "description": "The harbor watch musters.", "location_id": "harbor_office",
                   "npc_ids": ["captain_ines"]}),
            json!({"type": "start_dialogue", "action_id": "a11", "npc_id": "captain_ines",
                   "opening_line": "Show me where he keeps it."}),
            json!({"type": "invalidate_mission", "action_id": "a12", "mission_id": "smuggling_cover",
                   "reason": "The secret is out."}),
            json!({"type": "request_replan", "action_id": "a13", "reason": "The cover is blown.",
                   "mission_id": "smuggling_cover"}),
        ]
    }

    #[test]
    fn every_variant_round_trips() {
        let examples = examples();
        assert_eq!(examples.len(), DirectorAction::TYPES.len());
        for (example, expected_type) in examples.iter().zip(DirectorAction::TYPES) {
            let action: DirectorAction = serde_json::from_value(example.clone()).unwrap();
            assert_eq!(action.type_name(), expected_type);
            assert_eq!(example["type"], expected_type);
            assert_eq!(example["action_id"], action.action_id());
            assert_eq!(&serde_json::to_value(&action).unwrap(), example);
        }
    }

    #[test]
    fn optional_fields_default() {
        let action: DirectorAction = serde_json::from_value(json!({
            "type": "set_npc_disposition", "action_id": "a1", "npc_id": "keeper_tomas",
            "disposition": "wary", "reason": "r"
        }))
        .unwrap();
        assert!(matches!(
            &action,
            DirectorAction::SetNpcDisposition { toward, .. } if toward == "player"
        ));

        let action: DirectorAction = serde_json::from_value(json!({
            "type": "set_objective", "action_id": "a1", "objective_id": "o", "title": "t",
            "description": "d", "mission_id": null
        }))
        .unwrap();
        let value = serde_json::to_value(&action).unwrap();
        assert!(value.get("mission_id").is_none());
    }

    #[test]
    fn unknown_types_are_rejected() {
        for bad in [
            "execute_script",
            "custom_command",
            "eval",
            "SetWorldFlag",
            "",
        ] {
            let value = json!({"type": bad, "action_id": "a1", "command": "rm -rf /"});
            assert!(
                serde_json::from_value::<DirectorAction>(value).is_err(),
                "{bad:?} must not decode"
            );
        }
        assert!(serde_json::from_value::<DirectorAction>(json!({"action_id": "a1"})).is_err());
    }

    #[test]
    fn unknown_fields_and_values_are_rejected() {
        let extra =
            json!({"type": "set_world_flag", "action_id": "a1", "flag": "f", "script": "x"});
        assert!(serde_json::from_value::<DirectorAction>(extra).is_err());

        let missing =
            json!({"type": "move_npc", "action_id": "a1", "npc_id": "n", "location_id": "l"});
        assert!(serde_json::from_value::<DirectorAction>(missing).is_err());

        let disposition = json!({"type": "set_npc_disposition", "action_id": "a1", "npc_id": "n",
                                 "disposition": "furious", "reason": "r"});
        assert!(serde_json::from_value::<DirectorAction>(disposition).is_err());

        let event = json!({"type": "trigger_world_event", "action_id": "a1", "event": "nuke",
                           "description": "d"});
        assert!(serde_json::from_value::<DirectorAction>(event).is_err());
    }

    #[test]
    fn enum_wire_strings_match_the_lists() {
        for name in Disposition::ALL {
            assert!(serde_json::from_value::<Disposition>(json!(name)).is_ok());
        }
        for name in WorldEventKind::ALL {
            assert!(serde_json::from_value::<WorldEventKind>(json!(name)).is_ok());
        }
    }
}
