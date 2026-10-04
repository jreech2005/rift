import json
from pathlib import Path

import httpx
import pytest

from rift_canon import cache, compile
from rift_canon.config import Settings
from rift_canon.providers import wikipedia
from rift_canon.world_bible import WorldBible
from tests.conftest import FAKE_SECRETS, SECRET_VALUES
from tests.support import (
    CLAUDE_HOST,
    FIXTURES,
    TMDB_HOST,
    WIKIDATA_HOST,
    WIKIPEDIA_HOST,
    FakeWeb,
    fake_settings,
    valid_draft,
)

UNIVERSE_ID = "breaking_bad_tv_1396"


@pytest.fixture(autouse=True)
def _no_pacing(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(wikipedia, "REQUEST_DELAY_SECONDS", 0)
    monkeypatch.setattr(wikipedia, "RETRY_DELAY_SECONDS", 0)


def _main(web: FakeWeb, tmp_path: Path, *argv: str, settings: Settings | None = None) -> int:
    return compile.main(
        [*argv, "--cache-dir", str(tmp_path)],
        client=web.client(),
        settings=settings or fake_settings(),
        llm=web.llm(settings),
    )


def test_compiles_a_title_end_to_end(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    web = FakeWeb(claude=[valid_draft()])
    assert _main(web, tmp_path, "Breaking Bad") == 0

    out = capsys.readouterr().out
    for line in (
        "Resolving universe...",
        "Resolved: Breaking Bad (TV, 2008)",
        "Retrieving canon...",
        "Documents: 5",
        "Facts: 26",
        "Compiling WorldBible... (claude-opus-5-5)",
        "WorldBible validation: PASS (attempts: 1)",
        "Classification: canon 15, inferred 6, generated 5",
        "Player role: Forensic accountant",
        "Starting location: A1A Car Wash",
        "Opening conflict: Cash That Does Not Add Up",
        "Written:",
        f"{UNIVERSE_ID}.json",
    ):
        assert line in out

    bible_path = cache.world_bible_path(tmp_path, UNIVERSE_ID)
    assert cache.load_world_bible(bible_path, UNIVERSE_ID).status == "hit"
    assert cache.load_canon_packet(cache.canon_packet_path(tmp_path, UNIVERSE_ID)).status == "hit"
    assert len(web.sent_to(CLAUDE_HOST)) == 1


def test_second_run_is_a_cache_hit(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert _main(FakeWeb(claude=[valid_draft()]), tmp_path, "Breaking Bad") == 0
    capsys.readouterr()

    web = FakeWeb()  # any Claude call would fail the test
    assert _main(web, tmp_path, "Breaking Bad") == 0
    out = capsys.readouterr().out
    assert "Cache: HIT (validated)" in out
    assert "Cached at:" in out
    assert [request.url.host for request in web.requests] == [TMDB_HOST]


def test_corrupt_cache_entry_is_reported_and_rebuilt(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    assert _main(FakeWeb(claude=[valid_draft()]), tmp_path, "Breaking Bad") == 0
    bible_path = cache.world_bible_path(tmp_path, UNIVERSE_ID)
    bible_path.write_text('{"schema_version": 1, "universe": "broken"', encoding="utf-8")
    capsys.readouterr()

    web = FakeWeb(claude=[valid_draft()])
    assert _main(web, tmp_path, "Breaking Bad") == 0
    out = capsys.readouterr().out
    assert "Cache: INVALID (" in out
    assert "recompiling" in out
    assert "Cache: HIT" not in out
    assert len(web.sent_to(CLAUDE_HOST)) == 1
    # The evidence was still valid, so it is reused rather than fetched again.
    assert "Using cached canon packet" in out
    assert web.sent_to(WIKIPEDIA_HOST) == []
    assert cache.load_world_bible(bible_path, UNIVERSE_ID).status == "hit"


def test_no_cache_rebuilds_everything(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert _main(FakeWeb(claude=[valid_draft()]), tmp_path, "Breaking Bad") == 0
    web = FakeWeb(claude=[valid_draft()])
    assert _main(web, tmp_path, "Breaking Bad", "--no-cache") == 0
    assert "Cache: HIT" not in capsys.readouterr().out.split("Resolving universe...")[-1]
    assert len(web.sent_to(CLAUDE_HOST)) == 1
    assert len(web.sent_to(WIKIPEDIA_HOST)) == 7


def test_from_packet_makes_no_retrieval_request(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    web = FakeWeb(claude=[valid_draft()])
    packet = FIXTURES / "canon_packet_breaking_bad.json"
    # No TMDB key needed: nothing is resolved or retrieved.
    settings = Settings.from_mapping({"ANTHROPIC_API_KEY": FAKE_SECRETS["ANTHROPIC_API_KEY"]})
    assert _main(web, tmp_path, "--from-packet", str(packet), settings=settings) == 0

    assert [request.url.host for request in web.requests] == [CLAUDE_HOST]
    out = capsys.readouterr().out
    assert "Universe: Breaking Bad (TV, 2008)" in out
    assert "WorldBible validation: PASS" in out
    assert cache.world_bible_path(tmp_path, UNIVERSE_ID).is_file()


def test_from_packet_rejects_an_invalid_packet(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    bad = tmp_path / "bad.canon.json"
    bad.write_text('{"schema_version": 1}', encoding="utf-8")
    web = FakeWeb()
    assert _main(web, tmp_path, "--from-packet", str(bad)) == 1
    assert _main(web, tmp_path, "--from-packet", str(tmp_path / "absent.json")) == 1
    err = capsys.readouterr().err
    assert "cannot use canon packet" in err
    assert "file not found" in err
    assert web.requests == []


def test_json_flag_prints_only_the_world_bible(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    assert _main(FakeWeb(claude=[valid_draft()]), tmp_path, "Breaking Bad", "--json") == 0
    captured = capsys.readouterr()
    bible = WorldBible.model_validate_json(captured.out)
    assert bible.universe.universe_id == UNIVERSE_ID
    assert "Resolving universe..." in captured.err


def test_output_flag_writes_a_copy(tmp_path: Path) -> None:
    target = tmp_path / "out" / "bible.json"
    web = FakeWeb(claude=[valid_draft()])
    assert _main(web, tmp_path, "Breaking Bad", "--output", str(target)) == 0
    assert cache.load_world_bible(target, UNIVERSE_ID).status == "hit"


def test_model_can_be_overridden(tmp_path: Path) -> None:
    web = FakeWeb(claude=[valid_draft()])
    assert _main(web, tmp_path, "Breaking Bad", "--model", "claude-sonnet-5-5") == 0
    assert web.claude_bodies()[0]["model"] == "claude-sonnet-5-5"

    web = FakeWeb(claude=[valid_draft()])
    settings = fake_settings(ANTHROPIC_MODEL="claude-from-env")
    assert _main(web, tmp_path, "Breaking Bad", "--no-cache", settings=settings) == 0
    assert web.claude_bodies()[0]["model"] == "claude-from-env"


def test_missing_tmdb_key_is_blocked(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    web = FakeWeb()
    settings = Settings.from_mapping({"ANTHROPIC_API_KEY": FAKE_SECRETS["ANTHROPIC_API_KEY"]})
    assert _main(web, tmp_path, "Breaking Bad", settings=settings) == 2
    assert "BLOCKED: TMDB_API_KEY is not set" in capsys.readouterr().err
    assert web.requests == []


def test_missing_anthropic_key_is_blocked_before_retrieval(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    web = FakeWeb()
    settings = Settings.from_mapping({"TMDB_API_KEY": FAKE_SECRETS["TMDB_API_KEY"]})
    assert _main(web, tmp_path, "Breaking Bad", settings=settings) == 2
    assert "BLOCKED: ANTHROPIC_API_KEY is not set" in capsys.readouterr().err
    assert web.sent_to(WIKIPEDIA_HOST) == web.sent_to(WIKIDATA_HOST) == []
    assert list(tmp_path.iterdir()) == []


def test_unknown_title(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert _main(FakeWeb(), tmp_path, "zzzqqxx") == 1
    assert "ERROR: no movie or TV series found for 'zzzqqxx'" in capsys.readouterr().err


def test_provider_failure_is_reported(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    web = FakeWeb(tmdb=lambda request: httpx.Response(401, json={"status_message": "Invalid."}))
    assert _main(web, tmp_path, "Breaking Bad") == 1
    assert "ERROR: TMDB auth: HTTP 401: Invalid." in capsys.readouterr().err


def test_transient_claude_failure_can_simply_be_rerun(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    busy = {"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}
    web = FakeWeb(claude=[httpx.Response(529, json=busy)])
    assert _main(web, tmp_path, "Breaking Bad") == 1
    err = capsys.readouterr().err
    assert "ERROR: Claude unavailable: HTTP 529: Overloaded" in err
    assert "run the same command again" in err
    assert len(web.sent_to(CLAUDE_HOST)) == 1  # reported, not retried
    assert not cache.world_bible_path(tmp_path, UNIVERSE_ID).exists()

    # The evidence was cached before compiling, so the rerun retrieves nothing.
    web = FakeWeb(claude=[valid_draft()])
    assert _main(web, tmp_path, "Breaking Bad") == 0
    assert "Using cached canon packet" in capsys.readouterr().out
    assert web.sent_to(WIKIPEDIA_HOST) == []


def test_failed_compilation_writes_no_cache_entry(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    web = FakeWeb(claude=["not json", "still not json"])
    assert _main(web, tmp_path, "Breaking Bad") == 1

    captured = capsys.readouterr()
    assert "WorldBible validation: FAIL" in captured.out
    assert "failed validation after 2 attempts" in captured.err
    assert "Rejected model output saved to:" in captured.err
    assert not cache.world_bible_path(tmp_path, UNIVERSE_ID).exists()
    failed = sorted((tmp_path / cache.FAILED_DIR_NAME).iterdir())
    assert [path.read_text(encoding="utf-8") for path in failed] == ["not json", "still not json"]
    # The evidence is kept, so a retry needs no retrieval.
    assert cache.canon_packet_path(tmp_path, UNIVERSE_ID).is_file()


def test_output_never_contains_secrets(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    def echo(request: httpx.Request) -> httpx.Response:
        return httpx.Response(400, json={"error": {"message": f"rejected {request.headers}"}})

    assert _main(FakeWeb(claude=[valid_draft()]), tmp_path, "Breaking Bad") == 0
    assert _main(FakeWeb(claude_handler=echo), tmp_path, "Breaking Bad", "--no-cache") == 1
    captured = capsys.readouterr()
    written = "".join(
        path.read_text(encoding="utf-8") for path in tmp_path.iterdir() if path.is_file()
    )
    for secret in SECRET_VALUES:
        assert secret not in captured.out + captured.err + written


def test_a_title_or_a_packet_is_required(capsys: pytest.CaptureFixture[str]) -> None:
    for argv in ([], ["   "]):
        with pytest.raises(SystemExit) as caught:
            compile.main(argv, client=FakeWeb().client(), settings=fake_settings())
        assert caught.value.code == 2
    assert "give a title or --from-packet PATH" in capsys.readouterr().err


def test_cached_file_is_plain_world_bible_json(tmp_path: Path) -> None:
    assert _main(FakeWeb(claude=[valid_draft()]), tmp_path, "Breaking Bad") == 0
    raw = json.loads(cache.world_bible_path(tmp_path, UNIVERSE_ID).read_text(encoding="utf-8"))
    assert list(raw)[:2] == ["schema_version", "universe"]
    assert raw["player_role"]["classification"] == "generated"
    assert raw["opening_conflict"]["classification"] == "generated"
