import json
from pathlib import Path

from rift_canon import cache
from rift_canon.acquire import PIPELINE_VERSION
from rift_canon.compiler import COMPILER_VERSION
from tests.support import saved_packet, valid_bible

UNIVERSE_ID = "breaking_bad_tv_1396"


def _write_bible(tmp_path: Path) -> Path:
    return cache.write_json(cache.world_bible_path(tmp_path, UNIVERSE_ID), valid_bible())


def _tamper(path: Path, mutate) -> None:
    raw = json.loads(path.read_text(encoding="utf-8"))
    mutate(raw)
    path.write_text(json.dumps(raw), encoding="utf-8")


def test_paths_are_stable_and_inside_the_cache_dir(tmp_path: Path) -> None:
    assert cache.world_bible_path(tmp_path, UNIVERSE_ID) == tmp_path / f"{UNIVERSE_ID}.json"
    assert cache.canon_packet_path(tmp_path, UNIVERSE_ID) == tmp_path / f"{UNIVERSE_ID}.canon.json"
    assert cache.DEFAULT_CACHE_DIR.parts[-2:] == ("cache", "universes")


def test_write_then_read_is_a_hit(tmp_path: Path) -> None:
    path = _write_bible(tmp_path / "nested")
    lookup = cache.load_world_bible(path, UNIVERSE_ID)
    assert lookup.status == "hit"
    assert lookup.value == valid_bible()
    # Atomic write: no temporary file is left behind.
    assert [entry.name for entry in path.parent.iterdir()] == [f"{UNIVERSE_ID}.json"]


def test_stored_metadata(tmp_path: Path) -> None:
    raw = json.loads(_write_bible(tmp_path).read_text(encoding="utf-8"))
    assert raw["schema_version"] == 1
    assert raw["universe"]["universe_id"] == UNIVERSE_ID
    provenance = raw["provenance"]
    assert provenance["compiler_version"] == COMPILER_VERSION
    assert provenance["retrieved_at"] == "2026-10-03T12:00:00Z"
    assert provenance["llm"]["provider"] == "gemini"
    assert provenance["sources"][0] == {
        "source_id": "tmdb:tv:1396",
        "source_type": "tmdb",
        "title": "Breaking Bad",
        "source_url": "https://www.themoviedb.org/tv/1396",
        "retrieved_at": "2026-10-03T12:00:00Z",
    }


def test_missing_entry_is_a_miss(tmp_path: Path) -> None:
    lookup = cache.load_world_bible(cache.world_bible_path(tmp_path, UNIVERSE_ID), UNIVERSE_ID)
    assert (lookup.status, lookup.value) == ("miss", None)


def test_corrupt_json_is_invalid(tmp_path: Path) -> None:
    path = _write_bible(tmp_path)
    path.write_text(path.read_text(encoding="utf-8")[:200], encoding="utf-8")
    lookup = cache.load_world_bible(path, UNIVERSE_ID)
    assert (lookup.status, lookup.value) == ("invalid", None)
    assert "validation error" in lookup.reason


def test_non_utf8_bytes_are_invalid(tmp_path: Path) -> None:
    path = _write_bible(tmp_path)
    path.write_bytes(b"\xff\xfe\x00garbage")
    assert cache.load_world_bible(path, UNIVERSE_ID).status == "invalid"


def test_schema_invalid_entry_is_invalid(tmp_path: Path) -> None:
    path = _write_bible(tmp_path)
    _tamper(path, lambda raw: raw.pop("player_role"))
    lookup = cache.load_world_bible(path, UNIVERSE_ID)
    assert (lookup.status, lookup.value) == ("invalid", None)
    assert "player_role" in lookup.reason


def test_entry_that_lost_its_provenance_is_invalid(tmp_path: Path) -> None:
    # Valid JSON, right shape, but a canon claim no longer cites any evidence.
    path = _write_bible(tmp_path)
    _tamper(path, lambda raw: raw["characters"][0].update(source_refs=[]))
    lookup = cache.load_world_bible(path, UNIVERSE_ID)
    assert (lookup.status, lookup.value) == ("invalid", None)
    assert "requires at least one source_ref" in lookup.reason


def test_entry_for_another_universe_is_invalid(tmp_path: Path) -> None:
    path = _write_bible(tmp_path)
    lookup = cache.load_world_bible(path, "the_matrix_movie_603")
    assert (lookup.status, lookup.value) == ("invalid", None)
    assert "breaking_bad_tv_1396" in lookup.reason


def test_entry_from_another_compiler_version_is_stale(tmp_path: Path) -> None:
    path = _write_bible(tmp_path)
    _tamper(path, lambda raw: raw["provenance"].update(compiler_version="0.0.1"))
    lookup = cache.load_world_bible(path, UNIVERSE_ID)
    assert lookup.status == "stale"
    assert "0.0.1" in lookup.reason


def test_canon_packet_round_trip(tmp_path: Path) -> None:
    path = cache.write_json(cache.canon_packet_path(tmp_path, UNIVERSE_ID), saved_packet())
    lookup = cache.load_canon_packet(path, UNIVERSE_ID)
    assert lookup.status == "hit"
    assert lookup.value == saved_packet()
    assert cache.load_canon_packet(path, "the_matrix_movie_603").status == "invalid"
    assert cache.load_canon_packet(tmp_path / "absent.json").status == "miss"

    _tamper(path, lambda raw: raw["facts"][0].update(source_ids=["wikipedia:en:424242"]))
    assert cache.load_canon_packet(path, UNIVERSE_ID).status == "invalid"


def test_canon_packet_from_another_pipeline_version_is_stale(tmp_path: Path) -> None:
    path = cache.write_json(cache.canon_packet_path(tmp_path, UNIVERSE_ID), saved_packet())
    _tamper(path, lambda raw: raw["provenance"].update(pipeline_version="0.0.1"))
    lookup = cache.load_canon_packet(path, UNIVERSE_ID)
    assert lookup.status == "stale"
    assert PIPELINE_VERSION in lookup.reason


def test_failed_outputs_are_kept_apart_from_entries(tmp_path: Path) -> None:
    paths = cache.save_failed_outputs(tmp_path, UNIVERSE_ID, ["first", "second"])
    assert [path.read_text(encoding="utf-8") for path in paths] == ["first", "second"]
    assert all(path.parent.name == cache.FAILED_DIR_NAME for path in paths)
    assert (
        cache.load_world_bible(cache.world_bible_path(tmp_path, UNIVERSE_ID), UNIVERSE_ID).status
        == "miss"
    )
