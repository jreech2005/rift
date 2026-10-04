"""Scenario Builder — a compiled WorldBible becomes a runtime scenario.

The output is the JSON the Rust runtime loads through ``RIFT_SCENARIO``:
``{ plan, world, secrets }`` (``backend/src/runtime/world.rs``). It is a small
playable entry slice built from the opening conflict, not the universe: one
mission, at most two objectives, one beat, the opening's cast.

The transformation is deterministic and offline. Nothing here calls a model:
every field of the scenario is derived from the WorldBible.

    uv run python -m rift_canon.scenario --world PATH --output PATH
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from collections.abc import Iterator, Sequence
from pathlib import Path
from typing import Any

from pydantic import ValidationError

from rift_canon.world_bible import PLAYER_ID, Character, WorldBible

# Mirrors `backend/src/narrative/model.rs` and `validate.rs`.
NARRATIVE_SCHEMA_VERSION = 1
MAX_IDENTIFIER_LEN = 64
MAX_KEY_LEN = 128
_ID_CHARSET = re.compile(r"^[A-Za-z0-9_.:-]+$")

# The same ids `narrative::build_initial` gives the plan it derives in Rust,
# so a universe keeps its ids whether or not a scenario is loaded.
MISSION_ID = "opening_conflict"
REACH_OBJECTIVE_ID = "opening_conflict.reach_scene"
RESOLVE_OBJECTIVE_ID = "opening_conflict.resolve"
REPORT_OBJECTIVE_ID = "opening_conflict.report"
CHECKPOINT_ID = "opening_conflict_resolved"
RESOLVED_FLAG = "opening_conflict.resolved"

Scenario = dict[str, Any]


def choice_target(n: int) -> str:
    """The entity the player interacts with to take the ``n``th decision option."""
    return f"choice_{n}"


def choice_flag(n: int) -> str:
    """The flag that commits to the ``n``th option without an interaction."""
    return f"opening_conflict.choice_{n}"


def interacted_flag(target: str) -> str:
    """The flag the runtime sets on the first ``interact`` with ``target``."""
    return f"interacted:{target}"


def _flag_is(flag: str) -> dict[str, Any]:
    return {"type": "flag_is", "flag": flag, "value": True}


def _objective_completed(objective_id: str) -> dict[str, Any]:
    condition = {"type": "objective_is", "objective_id": objective_id, "status": "completed"}
    return {"condition": condition}


def contact(bible: WorldBible) -> Character:
    """The on-scene character the player answers to.

    The first character of the opening conflict the player role is connected
    to, else the first one the player has a relationship with, else the first
    character of the conflict.
    """
    involved = bible.opening_conflict.involved_character_ids
    connected = [c.character_id for c in bible.player_role.connections]
    for relationship in bible.relationships:
        if PLAYER_ID in (relationship.from_id, relationship.to_id):
            connected += [relationship.from_id, relationship.to_id]
    chosen = next((i for i in connected if i in involved), involved[0])
    return next(c for c in bible.characters if c.id == chosen)


def build_scenario(bible: WorldBible) -> Scenario:
    """The entry slice of ``bible`` as a runtime scenario."""
    opening = bible.opening_conflict
    start = bible.starting_location.location_id
    scene = opening.location_id
    involved = list(dict.fromkeys(opening.involved_character_ids))
    options = opening.decision_options
    giver = contact(bible)

    # Taking any decision option resolves the conflict: by interacting with
    # its entity, or by the flag a Director decision can set.
    resolve: dict[str, Any] = {
        "objective_id": RESOLVE_OBJECTIVE_ID,
        "description": opening.immediate_goal,
        "completes_when": {
            "type": "any",
            "conditions": [
                _flag_is(flag)
                for n in range(1, len(options) + 1)
                for flag in (interacted_flag(choice_target(n)), choice_flag(n))
            ],
        },
        "success_effects": [{"type": "set_flag", "flag": RESOLVED_FLAG, "value": True}],
    }
    if start != scene:
        name = next(item.name for item in bible.locations if item.id == scene)
        reach = {
            "objective_id": REACH_OBJECTIVE_ID,
            "description": f"Go to {name}.",
            "completes_when": {"type": "player_at", "location_id": scene},
        }
        resolve["prerequisites"] = [_objective_completed(REACH_OBJECTIVE_ID)]
        objectives = [reach, resolve]
    else:
        report = {
            "objective_id": REPORT_OBJECTIVE_ID,
            "description": f"Report to {giver.name}.",
            "prerequisites": [_objective_completed(RESOLVE_OBJECTIVE_ID)],
            "completes_when": _flag_is(interacted_flag(giver.id)),
        }
        objectives = [resolve, report]

    metadata = {
        "source": "world_bible.opening_conflict",
        "giver_npc_id": giver.id,
        "player_role": bible.player_role.title,
        "stakes": opening.stakes,
    }
    for n, option in enumerate(options, start=1):
        metadata[f"decision_option_{n}"] = option

    mission = {
        "mission_id": MISSION_ID,
        "title": opening.title,
        "description": opening.summary,
        "canon_relation": "generated",
        "importance": "critical",
        "related_characters": involved,
        "related_locations": [scene],
        "prerequisites": [
            {"condition": {"type": "location_available", "location_id": scene}},
            *(
                {"condition": {"type": "character_alive", "character_id": character_id}}
                for character_id in involved
            ),
        ],
        "metadata": metadata,
        "objectives": objectives,
    }
    checkpoint = {
        "checkpoint_id": CHECKPOINT_ID,
        "title": f"{opening.title} resolved",
        "description": opening.stakes,
        "canon_relation": "generated",
        "importance": "critical",
        "reached_when": {"type": "mission_is", "mission_id": MISSION_ID, "status": "completed"},
    }
    return {
        "plan": {
            "schema_version": NARRATIVE_SCHEMA_VERSION,
            "plan_id": f"{bible.universe.universe_id}.entry",
            "universe_id": bible.universe.universe_id,
            "missions": [mission],
            "checkpoints": [checkpoint],
        },
        "world": {
            "locations": list(dict.fromkeys([start, scene])),
            "characters": {character_id: {"location": scene} for character_id in involved},
            "player_location": start,
            "flags": {RESOLVED_FLAG: False},
            "truths": {
                f"world_rule_{n}": {
                    "statement": rule.text,
                    "holds": True,
                    "canon_relation": rule.classification.value,
                }
                for n, rule in enumerate(bible.world_rules, start=1)
            },
        },
        "secrets": [],
    }


def _conditions(node: Any) -> Iterator[dict[str, Any]]:
    """Every condition object under ``node``, at any depth."""
    if isinstance(node, dict):
        if "type" in node and "condition" not in node:
            yield node
        for value in node.values():
            yield from _conditions(value)
    elif isinstance(node, list):
        for item in node:
            yield from _conditions(item)


def scenario_errors(scenario: Scenario, bible: WorldBible) -> list[str]:
    """What the runtime would refuse at load, checked without starting it.

    Covers the invariants of ``narrative::validate`` and ``RuntimeWorld::check``
    that a generated scenario can break. The Rust runtime stays the authority.
    """
    errors: list[str] = []
    plan, world = scenario["plan"], scenario["world"]

    def check_id(path: str, value: str, max_len: int = MAX_IDENTIFIER_LEN) -> None:
        if not (0 < len(value) <= max_len and _ID_CHARSET.match(value)):
            errors.append(f"{path}: {value!r} is not a valid identifier")

    def known(path: str, value: str, allowed: set[str], kind: str) -> None:
        if value not in allowed:
            errors.append(f"{path}: {value!r} is not a known {kind}")

    if plan["schema_version"] != NARRATIVE_SCHEMA_VERSION:
        errors.append(f"plan.schema_version: {plan['schema_version']} is not supported")
    check_id("plan.plan_id", plan["plan_id"], MAX_KEY_LEN)
    if plan["universe_id"] != bible.universe.universe_id:
        errors.append("plan.universe_id: does not match the WorldBible")

    bible_locations = {item.id for item in bible.locations}
    bible_characters = {item.id for item in bible.characters}
    locations = set(world["locations"])
    characters = set(world["characters"])
    for location_id in locations:
        check_id("world.locations", location_id)
        known("world.locations", location_id, bible_locations, "WorldBible location")
    for character_id, facts in world["characters"].items():
        check_id("world.characters", character_id)
        known("world.characters", character_id, bible_characters, "WorldBible character")
        known(f"world.characters.{character_id}.location", facts["location"], locations, "location")
    if PLAYER_ID in characters:
        errors.append(f"world.characters: {PLAYER_ID!r} is reserved for the player")
    known("world.player_location", world["player_location"], locations, "location")
    for flag in world["flags"]:
        check_id("world.flags", flag, MAX_KEY_LEN)
    for truth_id in world["truths"]:
        check_id("world.truths", truth_id)

    missions = [mission["mission_id"] for mission in plan["missions"]]
    objectives = [o["objective_id"] for m in plan["missions"] for o in m["objectives"]]
    checkpoints = [checkpoint["checkpoint_id"] for checkpoint in plan["checkpoints"]]
    for kind, ids in (
        ("mission", missions),
        ("objective", objectives),
        ("checkpoint", checkpoints),
    ):
        for value in ids:
            check_id(f"plan.{kind}s", value)
        if len(set(ids)) != len(ids):
            errors.append(f"plan: duplicate {kind} ids")
    for mission in plan["missions"]:
        path = f"mission {mission['mission_id']}"
        if all(objective.get("optional", False) for objective in mission["objectives"]):
            errors.append(f"{path}: needs at least one required objective")
        for character_id in mission["related_characters"]:
            known(f"{path}.related_characters", character_id, characters, "character")
        for location_id in mission["related_locations"]:
            known(f"{path}.related_locations", location_id, locations, "location")
        giver = mission["metadata"].get("giver_npc_id")
        if giver is not None:
            known(f"{path}.metadata.giver_npc_id", giver, characters, "character")

    references = {
        "character_id": (characters, "character"),
        "location_id": (locations, "location"),
        "objective_id": (set(objectives), "objective"),
        "mission_id": (set(missions), "mission"),
    }
    for condition in _conditions(plan):
        for key, (allowed, kind) in references.items():
            if key in condition:
                known(f"condition {condition['type']}", condition[key], allowed, kind)
        if "flag" in condition:
            check_id(f"condition {condition['type']}", condition["flag"], MAX_KEY_LEN)
    return errors


def dumps(scenario: Scenario) -> str:
    return json.dumps(scenario, indent=2, ensure_ascii=False) + "\n"


def write_scenario(path: Path, scenario: Scenario) -> Path:
    """Write atomically, so a crash never leaves a half-written scenario."""
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp")
    temporary.write_text(dumps(scenario), encoding="utf-8")
    os.replace(temporary, path)
    return path


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="rift_canon.scenario",
        description="Build a runtime scenario (the playable entry slice) from a WorldBible.",
    )
    parser.add_argument(
        "--world", type=Path, required=True, metavar="PATH", help="compiled WorldBible JSON"
    )
    parser.add_argument(
        "--output",
        type=Path,
        metavar="PATH",
        help="where to write the scenario (default: print it on stdout)",
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        bible = WorldBible.model_validate_json(args.world.read_bytes())
    except OSError as exc:
        print(f"ERROR: cannot read {args.world}: {exc}", file=sys.stderr)
        return 1
    except ValidationError as exc:
        print(f"ERROR: {args.world} is not a valid WorldBible:\n{exc}", file=sys.stderr)
        return 1

    scenario = build_scenario(bible)
    errors = scenario_errors(scenario, bible)
    if errors:
        print("ERROR: the generated scenario is not loadable:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    if args.output is None:
        print(dumps(scenario), end="")
        return 0

    write_scenario(args.output, scenario)
    mission = scenario["plan"]["missions"][0]
    print(f"Universe: {bible.universe.title} ({bible.universe.universe_id})")
    print(f"Mission: {mission['title']}")
    for objective in mission["objectives"]:
        print(f"Objective: {objective['objective_id']}: {objective['description']}")
    print(f"Starting location: {scenario['world']['player_location']}")
    print(f"Characters: {', '.join(scenario['world']['characters'])}")
    for n, option in enumerate(bible.opening_conflict.decision_options, start=1):
        print(f"Option {n} (interact {choice_target(n)}): {option}")
    print("")
    print("Written:")
    print(args.output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
