import re
from typing import Any

import pytest
from pydantic import ValidationError

from rift_canon.canon_packet import Classification
from rift_canon.world_bible import (
    ENTITY_ID_PATTERN,
    WorldBible,
    WorldBibleDraft,
    iter_attributed,
)
from tests.support import saved_packet, valid_bible, valid_draft


def _raw() -> dict[str, Any]:
    return valid_bible().model_dump(mode="json")


def _rejects(raw: dict[str, Any], message: str) -> None:
    with pytest.raises(ValidationError, match=message):
        WorldBible.model_validate(raw)


def test_valid_world_bible() -> None:
    bible = valid_bible()
    assert bible.schema_version == 1
    assert bible.universe.universe_id == "breaking_bad_tv_1396"
    assert bible.universe.title == "Breaking Bad"
    assert bible.universe.media_type == "tv"
    assert bible.universe.genres == ["Drama", "Crime"]
    assert bible.universe.era.text and bible.universe.setting.text
    assert bible.world_rules and bible.locations and bible.characters
    assert bible.factions and bible.relationships and bible.important_conflicts
    assert bible.timeline_context.canon_cutoff.startswith("Season 1")
    assert WorldBible.model_validate_json(bible.model_dump_json()) == bible


def test_player_role_is_original_and_generated() -> None:
    bible = valid_bible()
    assert bible.player_role.classification == Classification.GENERATED
    assert bible.player_role.title == "Forensic accountant"
    assert bible.player_role.title not in {c.name for c in bible.characters}
    assert bible.player_role.reason_for_presence
    assert bible.player_role.connections[0].character_id == "walter_white"


@pytest.mark.parametrize("label", ["canon", "inferred"])
def test_player_role_can_only_be_generated(label: str) -> None:
    draft = valid_draft()
    draft["player_role"]["classification"] = label
    with pytest.raises(ValidationError, match="player_role.classification"):
        WorldBibleDraft.model_validate(draft)


def test_opening_conflict_can_only_be_generated() -> None:
    draft = valid_draft()
    draft["opening_conflict"]["classification"] = "canon"
    with pytest.raises(ValidationError, match="opening_conflict.classification"):
        WorldBibleDraft.model_validate(draft)


def test_player_cannot_be_an_existing_character() -> None:
    raw = _raw()
    raw["player_role"]["title"] = "walter  WHITE"
    _rejects(raw, "must be an original role")


def test_starting_location_and_opening_conflict() -> None:
    bible = valid_bible()
    location_ids = {location.id for location in bible.locations}
    character_ids = {character.id for character in bible.characters}
    opening = bible.opening_conflict

    assert bible.starting_location.location_id in location_ids
    assert opening.classification == Classification.GENERATED
    assert opening.location_id == bible.starting_location.location_id
    assert set(opening.involved_character_ids) <= character_ids
    assert 10 <= opening.estimated_minutes <= 20
    assert opening.immediate_goal
    assert 2 <= len(opening.decision_options) <= 4


def test_canon_inferred_and_generated_stay_distinguishable() -> None:
    bible = valid_bible()
    by_id = {character.id: character for character in bible.characters}
    assert by_id["walter_white"].classification is Classification.CANON
    assert by_id["marisol_vega"].classification is Classification.GENERATED
    assert by_id["walter_white"].goals[0].classification is Classification.INFERRED
    assert by_id["walter_white"].known_facts[0].classification is Classification.CANON
    assert bible.provenance.classification_counts == {"canon": 15, "inferred": 6, "generated": 5}


def test_canon_requires_a_source() -> None:
    raw = _raw()
    raw["characters"][0]["source_refs"] = []
    _rejects(raw, "characters\\[0\\]: classification 'canon' requires at least one source_ref")

    raw = _raw()
    raw["characters"][0]["known_facts"][0]["source_refs"] = []
    _rejects(raw, "known_facts\\[0\\]: classification 'canon' requires")


def test_source_refs_must_point_at_recorded_sources() -> None:
    raw = _raw()
    raw["locations"][0]["source_refs"] = ["wikipedia:en:424242"]
    _rejects(raw, "locations\\[0\\].source_refs: unknown source ids")

    raw = _raw()
    raw["player_role"]["source_refs"] = ["made-up"]
    _rejects(raw, "player_role.source_refs: unknown source ids")


@pytest.mark.parametrize(
    ("mutate", "message"),
    [
        (lambda raw: raw["starting_location"].update(location_id="nowhere"), "starting_location"),
        (lambda raw: raw["opening_conflict"].update(location_id="nowhere"), "opening_conflict"),
        (
            lambda raw: raw["opening_conflict"].update(involved_character_ids=["ghost"]),
            "'ghost' is not a defined character id",
        ),
        (lambda raw: raw["relationships"][0].update(to_id="ghost"), "relationships\\[0\\].to_id"),
        (lambda raw: raw["factions"][0].update(member_ids=["ghost"]), "factions\\[0\\].member_ids"),
        (
            lambda raw: raw["important_conflicts"][0].update(party_ids=["ghost"]),
            "important_conflicts\\[0\\].party_ids",
        ),
        (
            lambda raw: raw["player_role"]["connections"][0].update(character_id="ghost"),
            "player_role.connections\\[0\\]",
        ),
        (lambda raw: raw["locations"][1].update(id="a1a_car_wash"), "duplicate ids"),
        (lambda raw: raw["factions"][0].update(id="walter_white"), "duplicate ids"),
        (lambda raw: raw["characters"][3].update(id="player"), "reserved for the player"),
    ],
)
def test_dangling_and_duplicate_ids_are_rejected(mutate, message: str) -> None:
    raw = _raw()
    mutate(raw)
    _rejects(raw, message)


def test_a_bible_needs_at_least_one_canon_character() -> None:
    raw = _raw()
    for character in raw["characters"]:
        character["classification"] = "inferred"
    _rejects(raw, "at least one canon character")


@pytest.mark.parametrize(
    "mutate",
    [
        lambda raw: raw.update(schema_version=2),
        lambda raw: raw.update(unknown_field=1),
        lambda raw: raw.pop("player_role"),
        lambda raw: raw.pop("opening_conflict"),
        lambda raw: raw.pop("starting_location"),
        lambda raw: raw.pop("provenance"),
        lambda raw: raw["opening_conflict"].update(estimated_minutes=45),
        lambda raw: raw["opening_conflict"].update(involved_character_ids=[]),
        lambda raw: raw["opening_conflict"].update(decision_options=["only one"]),
        lambda raw: raw["characters"][0].update(name="   "),
        lambda raw: raw["characters"][0].update(classification="fanon"),
        lambda raw: raw["locations"][0].update(importance="huge"),
        lambda raw: raw["provenance"].update(sources=[]),
    ],
)
def test_malformed_bibles_are_rejected(mutate) -> None:
    raw = _raw()
    mutate(raw)
    with pytest.raises(ValidationError):
        WorldBible.model_validate(raw)


def test_ids_are_normalized_to_protocol_identifiers() -> None:
    draft = valid_draft()
    draft["characters"][0]["id"] = "Walter White"
    draft["relationships"][0]["from_id"] = " Walter-White "
    bible = valid_bible(draft)
    assert bible.characters[0].id == "walter_white"
    assert bible.relationships[0].from_id == "walter_white"
    for item in [*bible.locations, *bible.characters, *bible.factions]:
        assert re.fullmatch(ENTITY_ID_PATTERN, item.id)
        # Usable as a protocol V1 `target` / `current_location` (docs/PROTOCOL.md).
        assert re.fullmatch(r"[A-Za-z0-9_.:-]{1,64}", item.id)


def test_character_relationships_are_derived_from_the_top_level_list() -> None:
    bible = valid_bible()
    walter = bible.characters[0]
    assert [(r.other_id, r.direction, r.kind) for r in walter.relationships] == [
        ("jesse_pinkman", "outgoing", "business partner"),
        ("hank_schrader", "incoming", "brother-in-law"),
        ("player", "incoming", "coworker"),
    ]
    assert walter.relationships[2].classification is Classification.GENERATED
    assert bible.characters[3].relationships == []


def test_provenance_records_every_source() -> None:
    bible = valid_bible()
    packet = saved_packet()
    provenance = bible.provenance
    assert [source.source_id for source in provenance.sources] == packet.source_ids()
    assert provenance.sources[1].source_url == "https://en.wikipedia.org/wiki/Breaking_Bad"
    assert provenance.canon_packet_sha256 == packet.sha256()
    assert provenance.retrieved_at == packet.retrieved_at
    assert provenance.compiler_version
    assert provenance.llm.model == "claude-opus-5-5"


def test_iter_attributed_reaches_nested_claims() -> None:
    paths = [path for path, _ in iter_attributed(WorldBibleDraft.model_validate(valid_draft()))]
    assert "era" in paths
    assert "characters[0]" in paths
    assert "characters[0].known_facts[1]" in paths
    assert "player_role" in paths
    assert "opening_conflict" in paths
    assert len(paths) == 26
