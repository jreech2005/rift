"""Scenario Builder: WorldBible -> runtime scenario. Offline and deterministic.

The golden scenarios live with the Rust fixtures, where
``backend/tests/scenario_builder.rs`` boots the runtime from them.
"""

import copy
import json
import socket
from pathlib import Path
from typing import Any

import pytest

from rift_canon import scenario
from rift_canon.world_bible import WorldBible
from tests.support import valid_bible

BACKEND_FIXTURES = Path(__file__).parents[2] / "backend" / "tests" / "fixtures"
BUILDER_FIXTURES = BACKEND_FIXTURES / "scenario_builder"

# name -> (WorldBible, the scenario generated from it)
UNIVERSES = {
    "breaking_bad": (
        BACKEND_FIXTURES / "director" / "world_bible_breaking_bad.json",
        BUILDER_FIXTURES / "breaking_bad.scenario.json",
    ),
    "the_matrix": (
        BUILDER_FIXTURES / "the_matrix.world.json",
        BUILDER_FIXTURES / "the_matrix.scenario.json",
    ),
    "harry_potter_1": (
        BUILDER_FIXTURES / "harry_potter_1.world.json",
        BUILDER_FIXTURES / "harry_potter_1.scenario.json",
    ),
}
ALL = pytest.mark.parametrize("name", UNIVERSES)


@pytest.fixture(autouse=True)
def _no_network(monkeypatch: pytest.MonkeyPatch) -> None:
    def refuse(*args: object, **kwargs: object) -> None:
        raise AssertionError("the Scenario Builder must not open a connection")

    monkeypatch.setattr(socket.socket, "connect", refuse)


def _bible(name: str) -> WorldBible:
    return WorldBible.model_validate_json(UNIVERSES[name][0].read_bytes())


def _build(name: str) -> tuple[WorldBible, dict[str, Any]]:
    bible = _bible(name)
    return bible, scenario.build_scenario(bible)


@ALL
def test_builds_a_valid_scenario(name: str) -> None:
    bible, built = _build(name)
    assert scenario.scenario_errors(built, bible) == []
    assert set(built) == {"plan", "world", "secrets"}
    assert built["plan"]["schema_version"] == 1
    assert built["plan"]["universe_id"] == bible.universe.universe_id


@ALL
def test_matches_the_scenario_the_rust_runtime_is_tested_with(name: str) -> None:
    _, built = _build(name)
    assert scenario.dumps(built) == UNIVERSES[name][1].read_text(encoding="utf-8")


@ALL
def test_is_a_small_entry_slice(name: str) -> None:
    bible, built = _build(name)
    opening = bible.opening_conflict
    plan, world = built["plan"], built["world"]

    assert len(plan["missions"]) == 1
    assert len(plan["checkpoints"]) == 1
    mission = plan["missions"][0]
    assert mission["title"] == opening.title
    assert 1 <= len(mission["objectives"]) <= 2
    resolve = mission["objectives"][-2 if len(mission["objectives"]) == 2 else 0]
    assert resolve["description"] == opening.immediate_goal

    assert world["player_location"] == bible.starting_location.location_id
    assert set(world["locations"]) == {bible.starting_location.location_id, opening.location_id}
    assert list(world["characters"]) == opening.involved_character_ids
    assert world["flags"] == {scenario.RESOLVED_FLAG: False}
    assert [t["statement"] for t in world["truths"].values()] == [
        rule.text for rule in bible.world_rules
    ]
    assert built["secrets"] == []


@ALL
def test_referenced_characters_and_locations_exist(name: str) -> None:
    bible, built = _build(name)
    characters = {item.id for item in bible.characters}
    locations = {item.id for item in bible.locations}
    referenced_characters = set(built["world"]["characters"])
    referenced_locations = set(built["world"]["locations"])
    for condition in scenario._conditions(built["plan"]):
        if "character_id" in condition:
            referenced_characters.add(condition["character_id"])
        if "location_id" in condition:
            referenced_locations.add(condition["location_id"])
    mission = built["plan"]["missions"][0]
    referenced_characters |= {*mission["related_characters"], mission["metadata"]["giver_npc_id"]}
    referenced_locations |= set(mission["related_locations"])

    assert referenced_characters <= characters
    assert referenced_locations <= locations
    # ...and in the scenario's own world, which is what the runtime checks.
    assert referenced_characters <= set(built["world"]["characters"])
    assert referenced_locations <= set(built["world"]["locations"])


@ALL
def test_ids_are_deterministic(name: str) -> None:
    bible, built = _build(name)
    assert scenario.dumps(scenario.build_scenario(_bible(name))) == scenario.dumps(built)

    plan = built["plan"]
    assert plan["plan_id"] == f"{bible.universe.universe_id}.entry"
    assert [m["mission_id"] for m in plan["missions"]] == ["opening_conflict"]
    assert [o["objective_id"] for o in plan["missions"][0]["objectives"]] == [
        "opening_conflict.resolve",
        "opening_conflict.report",
    ]
    assert [c["checkpoint_id"] for c in plan["checkpoints"]] == ["opening_conflict_resolved"]


@ALL
def test_every_decision_option_resolves_the_conflict(name: str) -> None:
    bible, built = _build(name)
    resolve = built["plan"]["missions"][0]["objectives"][0]
    flags = [c["flag"] for c in resolve["completes_when"]["conditions"]]
    assert flags == [
        flag
        for n in range(1, len(bible.opening_conflict.decision_options) + 1)
        for flag in (f"interacted:choice_{n}", f"opening_conflict.choice_{n}")
    ]


def test_the_contact_is_someone_the_player_knows() -> None:
    assert scenario.contact(_bible("the_matrix")).id == "dozer"
    assert scenario.contact(_bible("harry_potter_1")).id == "mr_ollivander"

    # No connection on the scene: the first character of the conflict.
    bible = valid_bible()
    stranger = bible.model_copy(
        update={
            "relationships": [],
            "player_role": bible.player_role.model_copy(update={"connections": []}),
        }
    )
    assert scenario.contact(stranger).id == bible.opening_conflict.involved_character_ids[0]


def test_an_opening_away_from_the_start_begins_with_getting_there() -> None:
    bible = valid_bible()
    elsewhere = next(
        item for item in bible.locations if item.id != bible.opening_conflict.location_id
    )
    moved = bible.model_copy(
        update={
            "starting_location": bible.starting_location.model_copy(
                update={"location_id": elsewhere.id}
            )
        }
    )
    built = scenario.build_scenario(moved)
    assert scenario.scenario_errors(built, moved) == []

    reach, resolve = built["plan"]["missions"][0]["objectives"]
    assert reach["objective_id"] == "opening_conflict.reach_scene"
    assert reach["completes_when"] == {
        "type": "player_at",
        "location_id": bible.opening_conflict.location_id,
    }
    assert resolve["objective_id"] == "opening_conflict.resolve"
    assert resolve["prerequisites"][0]["condition"]["objective_id"] == reach["objective_id"]
    assert built["world"]["player_location"] == elsewhere.id
    assert built["world"]["locations"] == [elsewhere.id, bible.opening_conflict.location_id]


def test_scenario_errors_reports_what_the_runtime_would_refuse() -> None:
    bible, built = _build("the_matrix")
    broken = copy.deepcopy(built)
    broken["plan"]["universe_id"] = "another_universe"
    broken["world"]["characters"]["agent_smith"] = {"location": "zion"}
    broken["world"]["player_location"] = "zion"
    del broken["world"]["characters"]["neo"]
    broken["plan"]["missions"][0]["objectives"][1]["objective_id"] = "has spaces"

    errors = "\n".join(scenario.scenario_errors(broken, bible))
    assert "plan.universe_id: does not match the WorldBible" in errors
    assert "'agent_smith' is not a known WorldBible character" in errors
    assert "world.player_location: 'zion' is not a known location" in errors
    assert "condition character_alive: 'neo' is not a known character" in errors
    assert "'has spaces' is not a valid identifier" in errors


def test_cli_writes_a_scenario(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    world, golden = UNIVERSES["the_matrix"]
    output = tmp_path / "nested" / "the_matrix.scenario.json"

    assert scenario.main(["--world", str(world), "--output", str(output)]) == 0
    assert output.read_text(encoding="utf-8") == golden.read_text(encoding="utf-8")
    out = capsys.readouterr().out
    assert "Universe: The Matrix (the_matrix_movie_603)" in out
    assert "Mission: Power Surge in the Simulation Deck" in out
    assert "Objective: opening_conflict.report: Report to Dozer." in out
    assert str(output) in out


def test_cli_prints_the_scenario_without_output(capsys: pytest.CaptureFixture[str]) -> None:
    world, golden = UNIVERSES["harry_potter_1"]
    assert scenario.main(["--world", str(world)]) == 0
    assert json.loads(capsys.readouterr().out) == json.loads(golden.read_text(encoding="utf-8"))


def test_cli_rejects_a_file_that_is_not_a_world_bible(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    output = tmp_path / "out.json"
    not_a_bible = UNIVERSES["the_matrix"][1]
    assert scenario.main(["--world", str(not_a_bible), "--output", str(output)]) == 1
    assert scenario.main(["--world", str(tmp_path / "missing.json")]) == 1
    assert "is not a valid WorldBible" in capsys.readouterr().err
    assert not output.exists()
