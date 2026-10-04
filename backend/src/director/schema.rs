//! JSON Schemas for the Director contract, built from code.
//!
//! * [`decision_schema`] documents `DirectorDecision` V1 and is exported to
//!   `shared/schemas/director/v1/` (a test fails if the file drifts).
//! * [`proposal_schema`] is what a model must return. Given a context it is
//!   narrowed: reference fields become enums of ids that exist right now and
//!   actions with nothing to refer to are left out, so a constrained decoder
//!   cannot even express most invalid output.
//! * [`to_gemini_schema`] reduces a schema to the keywords Gemini accepts.
//!
//! A schema is a hint to the provider, never the safety boundary: Rust
//! validation (`validate.rs`) checks everything again.

use serde_json::{Map, Value, json};

use super::actions::{Disposition, WorldEventKind};
use super::context::{DirectorContext, MissionStatus, ObjectiveStatus, Trigger};
use super::decision::ReasonCode;
use super::{DIRECTOR_SCHEMA_VERSION, PLAYER_ID, RESERVED_FLAG_PREFIX, limits};

const IDENTIFIER_PATTERN: &str = "^[A-Za-z0-9_.:-]{1,64}$";
const SNAKE_ID_PATTERN: &str = "^[a-z0-9_]{1,64}$";
const ACTION_ID_PATTERN: &str = "^[a-z0-9_]{1,32}$";
const FLAG_PATTERN: &str = "^[A-Za-z0-9_.:-]{1,128}$";

/// Ids a narrowed schema may reference. `None` means "any well-formed id".
#[derive(Debug, Default)]
struct Known {
    npcs: Option<Vec<String>>,
    locations: Option<Vec<String>>,
    active_objectives: Option<Vec<String>>,
    active_missions: Option<Vec<String>>,
    missions: Option<Vec<String>>,
    set_flags: Option<Vec<String>>,
}

impl Known {
    fn from_context(ctx: &DirectorContext) -> Self {
        let owned = |ids: Vec<&str>| ids.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let narrative = ctx.narrative.as_ref();
        Self {
            npcs: Some(owned(ctx.actionable_npc_ids())),
            locations: Some(ctx.world.locations.iter().map(|l| l.id.clone()).collect()),
            active_objectives: narrative.map(|n| {
                n.objectives
                    .iter()
                    .filter(|o| o.status == ObjectiveStatus::Active)
                    .map(|o| o.objective_id.clone())
                    .collect()
            }),
            active_missions: narrative.map(|n| {
                n.missions
                    .iter()
                    .filter(|m| m.status == MissionStatus::Active)
                    .map(|m| m.mission_id.clone())
                    .collect()
            }),
            missions: narrative.map(|n| n.missions.iter().map(|m| m.mission_id.clone()).collect()),
            set_flags: Some(
                ctx.world_flags
                    .iter()
                    .filter(|(key, value)| **value && !key.starts_with(RESERVED_FLAG_PREFIX))
                    .map(|(key, _)| key.clone())
                    .collect(),
            ),
        }
    }
}

/// `true` when a reference of this kind can be satisfied at all.
fn available(ids: &Option<Vec<String>>) -> bool {
    ids.as_ref().is_none_or(|ids| !ids.is_empty())
}

fn reference(ids: &Option<Vec<String>>, pattern: &str, max_len: usize, description: &str) -> Value {
    match ids {
        Some(ids) => json!({"type": "string", "enum": ids, "description": description}),
        None => json!({
            "type": "string", "pattern": pattern, "maxLength": max_len, "description": description
        }),
    }
}

fn text(max: usize, description: &str) -> Value {
    json!({"type": "string", "minLength": 1, "maxLength": max, "description": description})
}

fn string_enum(values: &[&str], description: &str) -> Value {
    json!({"type": "string", "enum": values, "description": description})
}

/// One action variant: `type` and `action_id` plus its own fields.
fn variant(kind: &str, description: &str, fields: Vec<(&str, Value, bool)>) -> Value {
    let mut properties = Map::new();
    properties.insert("type".into(), json!({"type": "string", "enum": [kind]}));
    properties.insert(
        "action_id".into(),
        json!({
            "type": "string", "pattern": ACTION_ID_PATTERN, "maxLength": limits::MAX_ACTION_ID_LEN,
            "description": "Unique within this decision, e.g. a1, a2."
        }),
    );
    let mut required = vec![json!("type"), json!("action_id")];
    for (name, schema, is_required) in fields {
        properties.insert(name.to_owned(), schema);
        if is_required {
            required.push(json!(name));
        }
    }
    json!({
        "type": "object",
        "description": description,
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn action_variants(known: &Known) -> Vec<Value> {
    let id_len = crate::action::MAX_IDENTIFIER_LEN;
    let npc = |description: &str| reference(&known.npcs, IDENTIFIER_PATTERN, id_len, description);
    let location =
        |description: &str| reference(&known.locations, IDENTIFIER_PATTERN, id_len, description);
    let actor = |description: &str| {
        let with_player = known.npcs.as_ref().map(|npcs| {
            std::iter::once(PLAYER_ID.to_owned())
                .chain(npcs.iter().cloned())
                .collect::<Vec<_>>()
        });
        reference(&with_player, IDENTIFIER_PATTERN, id_len, description)
    };
    let active_objective = |description: &str| {
        reference(
            &known.active_objectives,
            IDENTIFIER_PATTERN,
            id_len,
            description,
        )
    };
    let reason = || text(limits::MAX_REASON_CHARS, "Why, in one plain sentence.");
    let has_npcs = available(&known.npcs);
    let has_locations = available(&known.locations);

    let mut variants = Vec::new();

    let mut set_objective = vec![
        (
            "objective_id",
            json!({
                "type": "string", "pattern": SNAKE_ID_PATTERN, "maxLength": id_len,
                "description": "A new lowercase snake_case id. Must not reuse an existing objective id."
            }),
            true,
        ),
        (
            "title",
            text(
                limits::MAX_TITLE_CHARS,
                "Short imperative shown to the player.",
            ),
            true,
        ),
        (
            "description",
            text(limits::MAX_TEXT_CHARS, "What the player should do and why."),
            true,
        ),
    ];
    if available(&known.active_missions) {
        set_objective.push((
            "mission_id",
            reference(
                &known.active_missions,
                IDENTIFIER_PATTERN,
                id_len,
                "Active mission this objective belongs to. Omit for a stand-alone or replacement objective.",
            ),
            false,
        ));
    }
    variants.push(variant(
        "set_objective",
        "Give the player a new objective.",
        set_objective,
    ));

    if available(&known.active_objectives) {
        variants.push(variant(
            "complete_objective",
            "Mark an active objective as completed.",
            vec![(
                "objective_id",
                active_objective("An active objective."),
                true,
            )],
        ));
        variants.push(variant(
            "fail_objective",
            "Mark an active objective as failed.",
            vec![
                (
                    "objective_id",
                    active_objective("An active objective."),
                    true,
                ),
                ("reason", reason(), true),
            ],
        ));
    }

    if has_npcs && has_locations {
        variants.push(variant(
            "activate_npc",
            "Bring a known character who is not yet in play into the world at a location.",
            vec![
                (
                    "npc_id",
                    npc("A known, living character who is not active."),
                    true,
                ),
                ("location_id", location("Where they appear."), true),
            ],
        ));
        variants.push(variant(
            "move_npc",
            "Move an NPC to another location for a plausible in-world reason.",
            vec![
                ("npc_id", npc("A known, living character."), true),
                ("location_id", location("Where they go."), true),
                ("reason", reason(), true),
            ],
        ));
    }

    if has_npcs {
        variants.push(variant(
            "set_npc_disposition",
            "Change how an NPC feels about the player or another character.",
            vec![
                ("npc_id", npc("The NPC whose feelings change."), true),
                (
                    "toward",
                    actor("Whom the feeling is about: \"player\" or a character id."),
                    true,
                ),
                (
                    "disposition",
                    string_enum(&Disposition::ALL, "The new disposition."),
                    true,
                ),
                ("reason", reason(), true),
            ],
        ));
    }

    let mut reveal = vec![
        (
            "recipient_id",
            actor("Who learns it: \"player\" or a character id."),
            true,
        ),
        (
            "text",
            text(
                limits::MAX_TEXT_CHARS,
                "The information, as plain in-world prose.",
            ),
            true,
        ),
    ];
    if has_npcs {
        reveal.push((
            "source_npc_id",
            npc("The NPC it comes from, if any."),
            false,
        ));
    }
    variants.push(variant(
        "reveal_information",
        "Let the player or an NPC learn something.",
        reveal,
    ));

    let flag_description = format!(
        "Flag key such as disclosed_to:some_npc. Must not start with {RESERVED_FLAG_PREFIX:?}."
    );
    variants.push(variant(
        "set_world_flag",
        "Record a lasting fact about this playthrough.",
        vec![(
            "flag",
            json!({
                "type": "string", "pattern": FLAG_PATTERN, "maxLength": limits::MAX_FLAG_KEY_LEN,
                "description": flag_description
            }),
            true,
        )],
    ));
    if available(&known.set_flags) {
        variants.push(variant(
            "clear_world_flag",
            "Clear a flag that is currently set.",
            vec![(
                "flag",
                reference(
                    &known.set_flags,
                    FLAG_PATTERN,
                    limits::MAX_FLAG_KEY_LEN,
                    "A flag that is currently set.",
                ),
                true,
            )],
        ));
    }

    let mut world_event = vec![
        (
            "event",
            string_enum(&WorldEventKind::ALL, "What kind of event to stage."),
            true,
        ),
        (
            "description",
            text(
                limits::MAX_TEXT_CHARS,
                "What happens, as plain in-world prose.",
            ),
            true,
        ),
    ];
    if has_locations {
        world_event.push((
            "location_id",
            location("Where it happens, if anywhere specific."),
            false,
        ));
    }
    if has_npcs {
        world_event.push((
            "npc_ids",
            json!({
                "type": "array", "maxItems": limits::MAX_EVENT_NPCS,
                "items": npc("An NPC involved."),
                "description": "NPCs involved, without duplicates."
            }),
            false,
        ));
    }
    variants.push(variant(
        "trigger_world_event",
        "Stage one of the allowed world events.",
        world_event,
    ));

    if has_npcs {
        variants.push(variant(
            "start_dialogue",
            "An NPC opens a conversation with the player.",
            vec![
                ("npc_id", npc("The NPC who speaks."), true),
                (
                    "opening_line",
                    text(limits::MAX_TEXT_CHARS, "Their first line, in character."),
                    true,
                ),
            ],
        ));
    }

    if available(&known.active_missions) {
        variants.push(variant(
            "invalidate_mission",
            "Declare a mission impossible or meaningless because of what the player did.",
            vec![
                (
                    "mission_id",
                    reference(
                        &known.active_missions,
                        IDENTIFIER_PATTERN,
                        id_len,
                        "An active mission.",
                    ),
                    true,
                ),
                ("reason", reason(), true),
            ],
        ));
    }

    let mut replan = vec![("reason", reason(), true)];
    if available(&known.missions) {
        replan.push((
            "mission_id",
            reference(
                &known.missions,
                IDENTIFIER_PATTERN,
                id_len,
                "Mission to replan; omit for the whole story.",
            ),
            false,
        ));
    }
    variants.push(variant(
        "request_replan",
        "Ask the narrative engine to plan again.",
        replan,
    ));

    variants
}

fn proposal_properties(known: &Known) -> Map<String, Value> {
    let mut properties = Map::new();
    properties.insert(
        "reason_code".into(),
        string_enum(
            &ReasonCode::ALL,
            "Why this decision. no_change requires an empty actions list.",
        ),
    );
    properties.insert(
        "actions".into(),
        json!({
            "type": "array",
            "maxItems": limits::MAX_ACTIONS,
            "description": "Typed actions in the order they should happen. May be empty.",
            "items": {"anyOf": action_variants(known)}
        }),
    );
    properties.insert(
        "narrative_summary".into(),
        text(
            limits::MAX_SUMMARY_CHARS,
            "One or two sentences: what the player did and how the world responds.",
        ),
    );
    properties.insert(
        "confidence".into(),
        json!({
            "type": "number", "minimum": 0, "maximum": 1,
            "description": "How well this decision fits the canon and the player's action."
        }),
    );
    properties
}

/// Schema of a provider's proposal. With a context, references are narrowed
/// to ids that exist in it.
pub fn proposal_schema(ctx: Option<&DirectorContext>) -> Value {
    let known = ctx.map(Known::from_context).unwrap_or_default();
    json!({
        "type": "object",
        "properties": proposal_properties(&known),
        "required": ["reason_code", "actions", "confidence"],
        "additionalProperties": false
    })
}

/// Canonical JSON Schema of `DirectorDecision` V1.
pub fn decision_schema() -> Value {
    let mut properties = proposal_properties(&Known::default());
    let mut insert = |name: &str, schema: Value| {
        properties.insert(name.to_owned(), schema);
    };
    insert("schema_version", json!({"const": DIRECTOR_SCHEMA_VERSION}));
    insert("decision_id", json!({"type": "string", "format": "uuid"}));
    insert("session_id", json!({"type": "string", "format": "uuid"}));
    insert(
        "universe_id",
        json!({"type": "string", "pattern": "^[a-z0-9_]{1,128}$"}),
    );
    insert(
        "trigger",
        json!({
            "type": "object",
            "description": "Why the Director was consulted. Copied from the DirectorContext.",
            "properties": {
                "kind": {"type": "string", "enum": Trigger::KINDS},
                "objective_id": {"type": "string", "pattern": IDENTIFIER_PATTERN},
                "npc_id": {"type": "string", "pattern": IDENTIFIER_PATTERN}
            },
            "required": ["kind"],
            "additionalProperties": false
        }),
    );
    insert(
        "metadata",
        json!({
            "type": "object",
            "description": "Assigned by the engine, never by a model.",
            "properties": {
                "provider": {"type": "string"},
                "model": {"type": "string"},
                "attempts": {"type": "integer", "minimum": 1, "maximum": 2},
                "repaired": {"type": "boolean"},
                "latency_ms": {"type": "integer", "minimum": 0},
                "usage": {"type": "object", "additionalProperties": {"type": "integer", "minimum": 0}},
                "created_at": {"type": "string", "format": "date-time"},
                "based_on_event_count": {"type": "integer", "minimum": 0},
                "fallback_reason": {"type": "string"}
            },
            "required": ["provider", "model", "attempts", "repaired", "latency_ms", "created_at",
                         "based_on_event_count"],
            "additionalProperties": false
        }),
    );
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "rift://schemas/director/v1/director_decision",
        "$comment": "Generated from backend/src/director/schema.rs. Do not edit; run RIFT_UPDATE_SCHEMAS=1 cargo test.",
        "title": "DirectorDecision",
        "description": "A validated Director decision: zero or more typed, allowlisted actions. Not a wire message. Source of truth: backend/src/director/.",
        "type": "object",
        "properties": properties,
        "required": ["schema_version", "decision_id", "session_id", "universe_id", "trigger",
                     "reason_code", "actions", "confidence", "metadata"],
        "additionalProperties": false
    })
}

/// Keywords Gemini's constrained decoder accepts (`properties` is handled
/// separately because its keys are field names, not keywords).
const GEMINI_KEYWORDS: [&str; 8] = [
    "additionalProperties",
    "anyOf",
    "description",
    "enum",
    "format",
    "items",
    "required",
    "type",
];

/// Size limits are stated in the description instead of as keywords: bounded
/// nested arrays make the constrained decoder reject the whole schema (seen
/// live in Phase 1). Rust enforces them on the response.
fn limits_hint(node: &Map<String, Value>) -> Option<String> {
    let number = |key: &str| node.get(key).and_then(Value::as_f64);
    if let Some(max) = number("maxItems") {
        return Some(format!("At most {max} item(s)."));
    }
    if let Some(max) = number("maxLength") {
        return Some(format!("At most {max} characters."));
    }
    match (number("minimum"), number("maximum")) {
        (Some(min), Some(max)) => Some(format!("{min} to {max}.")),
        _ => None,
    }
}

/// Reduce a JSON Schema to the subset Gemini's structured output supports.
pub fn to_gemini_schema(schema: &Value) -> Value {
    match schema {
        Value::Array(items) => Value::Array(items.iter().map(to_gemini_schema).collect()),
        Value::Object(node) => {
            let mut out = Map::new();
            for (key, value) in node {
                match key.as_str() {
                    "const" => {
                        out.insert("enum".into(), json!([value]));
                    }
                    "properties" => {
                        let fields: Map<String, Value> = value
                            .as_object()
                            .into_iter()
                            .flatten()
                            .map(|(name, field)| (name.clone(), to_gemini_schema(field)))
                            .collect();
                        out.insert(key.clone(), Value::Object(fields));
                    }
                    keyword if GEMINI_KEYWORDS.contains(&keyword) => {
                        out.insert(key.clone(), to_gemini_schema(value));
                    }
                    _ => {}
                }
            }
            if let Some(hint) = limits_hint(node) {
                let description = match out.get("description").and_then(Value::as_str) {
                    Some(existing) => format!("{existing} {hint}"),
                    None => hint,
                };
                out.insert("description".into(), Value::String(description));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// The response schema sent to Gemini for one context.
pub fn gemini_response_schema(ctx: &DirectorContext) -> Value {
    to_gemini_schema(&proposal_schema(Some(ctx)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::director::actions::DirectorAction;
    use crate::director::testing::sample_context;
    use std::collections::BTreeSet;
    use std::path::Path;

    fn variants(schema: &Value) -> Vec<&Value> {
        schema["properties"]["actions"]["items"]["anyOf"]
            .as_array()
            .unwrap()
            .iter()
            .collect()
    }

    fn variant_named<'a>(schema: &'a Value, kind: &str) -> Option<&'a Value> {
        variants(schema)
            .into_iter()
            .find(|v| v["properties"]["type"]["enum"][0] == kind)
    }

    fn type_names(schema: &Value) -> Vec<String> {
        variants(schema)
            .iter()
            .map(|v| {
                v["properties"]["type"]["enum"][0]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }

    /// Every keyword used anywhere in a schema (field names excluded).
    fn keywords(schema: &Value, found: &mut BTreeSet<String>) {
        match schema {
            Value::Array(items) => items.iter().for_each(|item| keywords(item, found)),
            Value::Object(node) => {
                for (key, value) in node {
                    found.insert(key.clone());
                    if key == "properties" {
                        value
                            .as_object()
                            .unwrap()
                            .values()
                            .for_each(|field| keywords(field, found));
                    } else {
                        keywords(value, found);
                    }
                }
            }
            _ => {}
        }
    }

    #[test]
    fn canonical_schema_lists_exactly_the_allowlisted_actions() {
        let schema = proposal_schema(None);
        assert_eq!(type_names(&schema), DirectorAction::TYPES);
        for variant in variants(&schema) {
            assert_eq!(variant["additionalProperties"], false);
            assert_eq!(variant["required"][0], "type");
            assert_eq!(variant["required"][1], "action_id");
        }
        assert_eq!(
            schema["properties"]["actions"]["maxItems"],
            limits::MAX_ACTIONS
        );
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn schema_required_fields_match_the_decoder() {
        // An object holding only the schema's required fields must decode, and
        // dropping any one of them must not.
        let sample = |name: &str| match name {
            "disposition" => json!("wary"),
            "event" => json!("rumor_spreads"),
            _ => json!("x"),
        };
        let schema = proposal_schema(None);
        for variant in variants(&schema) {
            let required: Vec<&str> = variant["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r.as_str().unwrap())
                .collect();
            let kind = variant["properties"]["type"]["enum"][0].clone();
            let mut object = Map::new();
            for name in &required {
                object.insert((*name).to_owned(), sample(name));
            }
            object.insert("type".into(), kind.clone());
            let full = Value::Object(object.clone());
            assert!(
                serde_json::from_value::<DirectorAction>(full).is_ok(),
                "{kind}: required fields must be enough to decode"
            );
            for name in required.iter().filter(|n| **n != "toward") {
                let mut partial = object.clone();
                partial.remove(*name);
                assert!(
                    serde_json::from_value::<DirectorAction>(Value::Object(partial)).is_err(),
                    "{kind}: {name} is required by the schema but not by the decoder"
                );
            }
            // Every schema property is a field the decoder knows.
            for name in variant["properties"].as_object().unwrap().keys() {
                let mut with_field = object.clone();
                let value = if name == "npc_ids" {
                    json!(["x"])
                } else {
                    sample(name)
                };
                with_field.insert(name.clone(), value);
                with_field.insert("type".into(), kind.clone());
                assert!(
                    serde_json::from_value::<DirectorAction>(Value::Object(with_field)).is_ok(),
                    "{kind}: schema property {name} is unknown to the decoder"
                );
            }
        }
    }

    #[test]
    fn context_narrows_references_to_known_ids() {
        let ctx = sample_context();
        let schema = proposal_schema(Some(&ctx));

        let activate = variant_named(&schema, "activate_npc").unwrap();
        assert_eq!(
            activate["properties"]["npc_id"]["enum"],
            json!(["captain_ines", "keeper_tomas", "broker_wen"]),
            "dead characters are not offered"
        );
        assert_eq!(
            activate["properties"]["location_id"]["enum"],
            json!(["harbor_office", "lighthouse", "night_market"])
        );
        assert!(activate["properties"]["npc_id"].get("pattern").is_none());

        let disposition = variant_named(&schema, "set_npc_disposition").unwrap();
        assert_eq!(disposition["properties"]["toward"]["enum"][0], "player");
        assert_eq!(
            disposition["properties"]["disposition"]["enum"],
            json!(Disposition::ALL)
        );

        let fail = variant_named(&schema, "fail_objective").unwrap();
        assert_eq!(
            fail["properties"]["objective_id"]["enum"],
            json!(["hide_ledger"])
        );
        let invalidate = variant_named(&schema, "invalidate_mission").unwrap();
        assert_eq!(
            invalidate["properties"]["mission_id"]["enum"],
            json!(["smuggling_cover"])
        );
        let replan = variant_named(&schema, "request_replan").unwrap();
        assert_eq!(
            replan["properties"]["mission_id"]["enum"],
            json!(["smuggling_cover", "first_delivery"])
        );
        let clear = variant_named(&schema, "clear_world_flag").unwrap();
        assert_eq!(
            clear["properties"]["flag"]["enum"],
            json!(["ledger_hidden"]),
            "reserved flags are not offered"
        );
        assert_eq!(type_names(&schema).len(), DirectorAction::TYPES.len());
    }

    #[test]
    fn actions_without_a_possible_target_are_not_offered() {
        let mut ctx = sample_context();
        let narrative = ctx.narrative.as_mut().unwrap();
        narrative
            .objectives
            .retain(|o| o.status != ObjectiveStatus::Active);
        narrative
            .missions
            .retain(|m| m.status != MissionStatus::Active);
        ctx.world_flags.clear();
        let schema = proposal_schema(Some(&ctx));
        let names = type_names(&schema);
        for absent in [
            "complete_objective",
            "fail_objective",
            "invalidate_mission",
            "clear_world_flag",
        ] {
            assert!(
                !names.iter().any(|n| n == absent),
                "{absent} should be omitted"
            );
        }
        let set_objective = variant_named(&schema, "set_objective").unwrap();
        assert!(set_objective["properties"].get("mission_id").is_none());
        assert!(names.iter().any(|n| n == "set_objective"));
        assert!(names.iter().any(|n| n == "request_replan"));

        // Without a narrative view nothing is known, so nothing is excluded.
        ctx.narrative = None;
        let schema = proposal_schema(Some(&ctx));
        let fail = variant_named(&schema, "fail_objective").unwrap();
        assert!(fail["properties"]["objective_id"].get("enum").is_none());
        assert_eq!(
            fail["properties"]["objective_id"]["pattern"],
            IDENTIFIER_PATTERN
        );
    }

    #[test]
    fn gemini_schema_uses_only_supported_keywords() {
        let schema = gemini_response_schema(&sample_context());
        let mut found = BTreeSet::new();
        keywords(&schema, &mut found);
        let allowed: BTreeSet<String> = GEMINI_KEYWORDS
            .iter()
            .chain(["properties"].iter())
            .map(|k| (*k).to_owned())
            .collect();
        assert!(
            found.is_subset(&allowed),
            "unsupported: {:?}",
            found.difference(&allowed)
        );

        // Limits survive as prose.
        let actions = &schema["properties"]["actions"];
        assert!(actions.get("maxItems").is_none());
        assert!(
            actions["description"]
                .as_str()
                .unwrap()
                .ends_with("At most 8 item(s).")
        );
        let confidence = schema["properties"]["confidence"]["description"]
            .as_str()
            .unwrap();
        assert!(confidence.ends_with("0 to 1."), "{confidence}");
        let summary = schema["properties"]["narrative_summary"]["description"]
            .as_str()
            .unwrap();
        assert!(summary.ends_with("At most 400 characters."), "{summary}");
        // Structure is preserved.
        assert_eq!(type_names(&schema).len(), DirectorAction::TYPES.len());
        assert_eq!(
            schema["required"],
            json!(["reason_code", "actions", "confidence"])
        );
    }

    #[test]
    fn gemini_reduction_handles_const_and_field_names_that_look_like_keywords() {
        let reduced = to_gemini_schema(&json!({
            "type": "object",
            "title": "dropped",
            "properties": {
                "type": {"const": "x", "pattern": "dropped"},
                "pattern": {"type": "string", "minLength": 1}
            }
        }));
        assert_eq!(
            reduced,
            json!({
                "type": "object",
                "properties": {
                    "type": {"enum": ["x"]},
                    "pattern": {"type": "string"}
                }
            })
        );
    }

    #[test]
    fn exported_decision_schema_matches_code() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../shared/schemas/director/v1/director_decision.schema.json");
        let expected = format!(
            "{}\n",
            serde_json::to_string_pretty(&decision_schema()).unwrap()
        );
        if std::env::var_os("RIFT_UPDATE_SCHEMAS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &expected).unwrap();
        }
        let actual = std::fs::read_to_string(&path)
            .expect("schema file missing; run RIFT_UPDATE_SCHEMAS=1 cargo test");
        assert!(
            actual == expected,
            "{} drifted from schema.rs; run RIFT_UPDATE_SCHEMAS=1 cargo test",
            path.display()
        );
    }

    #[test]
    fn decision_schema_covers_the_serialized_decision() {
        let schema = decision_schema();
        let properties = schema["properties"].as_object().unwrap();
        for required in schema["required"].as_array().unwrap() {
            assert!(properties.contains_key(required.as_str().unwrap()));
        }
        assert_eq!(schema["properties"]["schema_version"]["const"], 1);
        assert_eq!(
            schema["properties"]["trigger"]["properties"]["kind"]["enum"],
            json!(Trigger::KINDS)
        );
        assert_eq!(type_names(&schema), DirectorAction::TYPES);
    }
}
