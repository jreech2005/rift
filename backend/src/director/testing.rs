//! Shared test fixtures. The sample universe is deliberately not the demo
//! universe: Director logic must not depend on any particular story.

use serde_json::json;

use super::actions::{DirectorAction, Disposition, WorldEventKind};
use super::context::DirectorContext;
use super::decision::{DirectorProposal, ReasonCode};

/// The WorldBible compiled in Phase 1 (`cache/universes/breaking_bad_tv_1396.json`).
pub(crate) const BREAKING_BAD_WORLD_BIBLE: &str =
    include_str!("../../tests/fixtures/director/world_bible_breaking_bad.json");

/// "Lantern Bay": the player was asked to hide the keeper's ledger and told
/// the harbor captain about it instead.
pub(crate) fn sample_context() -> DirectorContext {
    serde_json::from_value(json!({
        "schema_version": 1,
        "session_id": "0b5e0f53-0a52-4c8f-8f0e-9d0a1b2c3d4e",
        "universe_id": "lantern_bay",
        "event_count": 5,
        "trigger": {
            "kind": "player_disclosure",
            "npc_id": "captain_ines",
            "objective_id": "hide_ledger"
        },
        "world": {
            "title": "Lantern Bay",
            "setting": "A fog-bound harbor town where the lighthouse keeper quietly runs contraband past the harbor watch.",
            "era": "An age of sail and oil lamps.",
            "canon_cutoff": "Before the autumn storms",
            "timeline_summary": "The keeper has moved contraband through the lighthouse for a year. The harbor watch suspects it but has no proof.",
            "rules": [
                "The harbor watch answers to the captain alone.",
                "Nothing leaves the bay unseen from the lighthouse."
            ],
            "locations": [
                {"id": "harbor_office", "name": "Harbor Office", "description": "The watch's cramped office on the quay."},
                {"id": "lighthouse", "name": "The Lighthouse", "description": "A storm-worn tower at the mouth of the bay."},
                {"id": "night_market", "name": "Night Market", "description": "Stalls that open after the last ferry."}
            ],
            "characters": [
                {"id": "captain_ines", "name": "Captain Ines", "role": "Commander of the harbor watch.",
                 "status": "Hunting for proof of the smuggling ring.", "traits": ["dogged", "fair"],
                 "goals": ["Expose the smuggling ring."], "canon": true},
                {"id": "keeper_tomas", "name": "Keeper Tomas", "role": "Lighthouse keeper and smuggler.",
                 "status": "Running contraband and trusting the player with his ledger.",
                 "traits": ["charming", "ruthless"], "goals": ["Keep the ledger hidden."], "canon": true},
                {"id": "broker_wen", "name": "Broker Wen", "role": "Buys and sells whatever arrives by night.",
                 "status": "Waiting at the market for the next shipment.", "traits": ["patient"],
                 "goals": [], "canon": false},
                {"id": "old_marlow", "name": "Old Marlow", "role": "The previous keeper.",
                 "status": "Drowned last winter.", "traits": ["stubborn"], "goals": [], "canon": true}
            ],
            "factions": [
                {"id": "harbor_watch", "name": "The Harbor Watch", "member_ids": ["captain_ines"]}
            ],
            "player_role": {
                "title": "Dock Clerk",
                "description": "A clerk who logs every crate that crosses the quay.",
                "capabilities": ["Access to the harbor ledgers", "Trusted by the dock crews"]
            },
            "opening": {
                "title": "The Ledger",
                "summary": "The keeper hands the player his private ledger moments before the captain arrives.",
                "location_id": "harbor_office",
                "involved_character_ids": ["keeper_tomas", "captain_ines"],
                "immediate_goal": "Decide what to do with the keeper's ledger before the captain asks for it.",
                "stakes": "If the ledger reaches the watch, the keeper hangs."
            }
        },
        "player": {"location": "harbor_office", "attributes": {"cover": "intact"}},
        "recent_events": [
            {"sequence": 4, "event_type": "location_changed", "actor_id": "player",
             "target": "harbor_office"},
            {"sequence": 5, "event_type": "speech_acknowledged", "actor_id": "player",
             "target": "captain_ines", "text": "The keeper hides a ledger in the lighthouse."}
        ],
        "world_flags": {"ledger_hidden": true, "interacted:desk": true},
        "narrative": {
            "summary": "The player has been drawn into the keeper's smuggling ring.",
            "missions": [
                {"mission_id": "smuggling_cover", "title": "Keep the keeper's secret", "status": "active"},
                {"mission_id": "first_delivery", "title": "The first delivery", "status": "completed"}
            ],
            "objectives": [
                {"objective_id": "hide_ledger", "title": "Hide the ledger", "status": "active",
                 "mission_id": "smuggling_cover", "giver_npc_id": "keeper_tomas"},
                {"objective_id": "meet_broker", "title": "Meet the broker", "status": "completed",
                 "mission_id": "first_delivery"}
            ]
        },
        "npcs": [
            {"npc_id": "captain_ines", "location": "harbor_office", "active": true, "disposition": "neutral"},
            {"npc_id": "keeper_tomas", "location": "lighthouse", "active": true, "disposition": "friendly"},
            {"npc_id": "broker_wen", "location": "night_market", "active": false},
            {"npc_id": "old_marlow", "active": false, "alive": false}
        ]
    }))
    .expect("sample context decodes")
}

/// A valid divergence decision for [`sample_context`].
pub(crate) fn sample_proposal() -> DirectorProposal {
    DirectorProposal {
        reason_code: ReasonCode::PlayerDivergence,
        actions: vec![
            DirectorAction::FailObjective {
                action_id: "a1".into(),
                objective_id: "hide_ledger".into(),
                reason: "The player told the captain about the ledger.".into(),
            },
            DirectorAction::InvalidateMission {
                action_id: "a2".into(),
                mission_id: "smuggling_cover".into(),
                reason: "The keeper's secret is out.".into(),
            },
            DirectorAction::SetWorldFlag {
                action_id: "a3".into(),
                flag: "disclosed_to:captain_ines".into(),
            },
            DirectorAction::SetNpcDisposition {
                action_id: "a4".into(),
                npc_id: "keeper_tomas".into(),
                toward: "player".into(),
                disposition: Disposition::Hostile,
                reason: "He learns who talked.".into(),
            },
            DirectorAction::TriggerWorldEvent {
                action_id: "a5".into(),
                event: WorldEventKind::AuthoritiesAlerted,
                description: "The harbor watch musters to search the lighthouse.".into(),
                location_id: Some("harbor_office".into()),
                npc_ids: vec!["captain_ines".into()],
            },
            DirectorAction::SetObjective {
                action_id: "a6".into(),
                objective_id: "choose_a_side".into(),
                title: "Choose a side".into(),
                description: "The watch is heading for the lighthouse. Warn the keeper or stand with the captain.".into(),
                mission_id: None,
            },
        ],
        narrative_summary: Some(
            "The player gave the keeper's secret to the captain; the cover mission is dead.".into(),
        ),
        confidence: 0.9,
    }
}
