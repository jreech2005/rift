import copy
from typing import Any

import pytest
from pydantic import ValidationError

from rift_canon.canon_packet import CanonFact, CanonPacket, Classification
from tests.support import fixture, saved_packet


def _raw() -> dict[str, Any]:
    return copy.deepcopy(fixture("canon_packet_breaking_bad.json"))


def test_saved_fixture_is_a_valid_packet() -> None:
    packet = saved_packet()
    assert packet.schema_version == 1
    assert packet.universe.universe_id == "breaking_bad_tv_1396"
    assert len(packet.documents) == 5
    assert len(packet.facts) == 26
    assert packet.source_ids()[0] == "tmdb:tv:1396"


def test_json_round_trip_is_lossless() -> None:
    packet = saved_packet()
    again = CanonPacket.model_validate_json(packet.model_dump_json())
    assert again == packet
    assert again.sha256() == packet.sha256()
    assert len(packet.sha256()) == 64


def test_digest_changes_with_the_evidence() -> None:
    raw = _raw()
    raw["documents"][1]["text"] += " One more sentence."
    assert CanonPacket.model_validate(raw).sha256() != saved_packet().sha256()


def test_classification_values() -> None:
    assert [c.value for c in Classification] == ["canon", "inferred", "generated"]
    fact = {"fact_id": "f", "subject": "s", "predicate": "p", "object": "o"}
    sourced = {**fact, "source_ids": ["tmdb:tv:1396"], "confidence": 1.0}
    # Source-backed facts are canon unless stated otherwise.
    assert CanonFact.model_validate(sourced).classification is Classification.CANON
    for value in Classification:
        assert CanonFact.model_validate({**sourced, "classification": value.value})
    with pytest.raises(ValidationError):
        CanonFact.model_validate({**sourced, "classification": "fanon"})


def test_fact_needs_a_source_and_a_sane_confidence() -> None:
    fact = {"fact_id": "f", "subject": "s", "predicate": "p", "object": "o"}
    with pytest.raises(ValidationError):
        CanonFact.model_validate({**fact, "source_ids": [], "confidence": 1.0})
    with pytest.raises(ValidationError):
        CanonFact.model_validate({**fact, "source_ids": ["tmdb:tv:1396"], "confidence": 1.5})


def test_duplicate_source_ids_are_rejected() -> None:
    raw = _raw()
    raw["documents"][2]["source_id"] = raw["documents"][1]["source_id"]
    with pytest.raises(ValidationError, match="duplicate document source_id"):
        CanonPacket.model_validate(raw)


def test_fact_citing_an_unknown_source_is_rejected() -> None:
    raw = _raw()
    raw["facts"][0]["source_ids"] = ["wikipedia:en:424242"]
    with pytest.raises(ValidationError, match="cites unknown sources"):
        CanonPacket.model_validate(raw)


@pytest.mark.parametrize(
    "mutate",
    [
        lambda raw: raw.update(schema_version=2),
        lambda raw: raw.update(documents=[]),
        lambda raw: raw.update(surprise=True),
        lambda raw: raw["documents"][0].update(text=""),
        lambda raw: raw["documents"][0].update(retrieved_at="2026-10-03T12:00:00"),
        lambda raw: raw["documents"][0].update(source_type="blog"),
        lambda raw: raw["universe"].update(universe_id="../escape"),
        lambda raw: raw["facts"].append(raw["facts"][0]),
    ],
)
def test_invalid_packets_are_rejected(mutate) -> None:
    raw = _raw()
    mutate(raw)
    with pytest.raises(ValidationError):
        CanonPacket.model_validate(raw)
