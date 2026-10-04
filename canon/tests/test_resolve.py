import re

import httpx
import pytest

from rift_canon.canon_packet import (
    UNIVERSE_ID_PATTERN,
    MediaType,
    ResolvedUniverse,
    make_universe_id,
)
from rift_canon.config import Settings
from rift_canon.errors import ConfigurationError, ProviderError, TitleNotFoundError
from rift_canon.providers.tmdb_catalog import TMDBCatalog
from rift_canon.resolve import MAX_CANDIDATES, resolve_title
from tests.conftest import FAKE_SECRETS, SECRET_VALUES
from tests.support import TMDB_HOST, FakeWeb, fake_settings, run


def _resolve(title: str, web: FakeWeb | None = None) -> ResolvedUniverse:
    web = web or FakeWeb()

    async def go() -> ResolvedUniverse:
        async with web.client() as client:
            return await resolve_title(client, TMDBCatalog(fake_settings()), title)

    return run(go())


def _results(*items: dict) -> FakeWeb:
    body = {"page": 1, "results": list(items), "total_pages": 1, "total_results": len(items)}
    return FakeWeb(tmdb=lambda request: httpx.Response(200, json=body))


def test_resolves_tv_series() -> None:
    universe = _resolve("Breaking Bad")
    assert universe.title == "Breaking Bad"
    assert universe.media_type is MediaType.TV
    assert universe.tmdb_id == 1396
    assert universe.release_year == 2008
    assert universe.original_language == "en"
    assert universe.overview.startswith("A chemistry teacher")
    assert universe.universe_id == "breaking_bad_tv_1396"
    assert universe.match == "exact"
    assert not universe.ambiguous
    assert universe.provider_metadata["provider"] == "tmdb"


def test_resolves_movie() -> None:
    universe = _resolve("the matrix")
    assert (universe.title, universe.media_type, universe.tmdb_id) == ("The Matrix", "movie", 603)
    assert universe.release_year == 1999
    assert universe.universe_id == "the_matrix_movie_603"
    assert universe.match == "exact"
    assert not universe.ambiguous


def test_candidates_are_kept_for_disambiguation() -> None:
    universe = _resolve("Breaking Bad")
    assert [c.tmdb_id for c in universe.candidates] == [1396, 559969, 635602]
    assert len(universe.candidates) <= MAX_CANDIDATES
    # People are not universes; an empty release date is not a year.
    assert all(c.media_type in ("movie", "tv") for c in universe.candidates)
    assert universe.candidates[2].release_year is None


def test_ambiguous_title_prefers_the_established_work() -> None:
    # TMDB ranks an unreleased, zero-vote series named exactly "Harry Potter" first.
    universe = _resolve("Harry Potter")
    assert universe.tmdb_id == 671
    assert universe.title == "Harry Potter and the Philosopher's Stone"
    assert universe.match == "partial"
    assert universe.ambiguous
    assert 224377 in [c.tmdb_id for c in universe.candidates]


def test_two_exact_matches_are_ambiguous() -> None:
    old = {"id": 841, "media_type": "movie", "title": "Dune", "release_date": "1984-12-14"}
    new = {"id": 438631, "media_type": "movie", "title": "Dune", "release_date": "2021-09-15"}
    universe = _resolve("Dune", _results({**old, "vote_count": 3000}, {**new, "vote_count": 12000}))
    assert universe.tmdb_id == 438631
    assert universe.match == "exact"
    assert universe.ambiguous


def test_unrelated_results_fall_back_to_tmdb_order() -> None:
    first = {"id": 1, "media_type": "tv", "name": "Something Else", "vote_count": 3}
    second = {"id": 2, "media_type": "movie", "title": "Another Thing", "vote_count": 900}
    universe = _resolve("Zorp", _results(first, second))
    assert universe.tmdb_id == 1
    assert universe.match == "fallback"
    assert universe.ambiguous


def test_no_results() -> None:
    with pytest.raises(TitleNotFoundError, match="zzzqqxx"):
        _resolve("zzzqqxx")


def test_only_people_is_not_a_universe() -> None:
    web = _results({"id": 17419, "media_type": "person", "name": "Bryan Cranston"})
    with pytest.raises(TitleNotFoundError):
        _resolve("Bryan Cranston", web)


def test_blank_title_makes_no_request() -> None:
    web = FakeWeb()
    with pytest.raises(TitleNotFoundError):
        _resolve("   ", web)
    assert web.requests == []


def test_timeout() -> None:
    def slow(request: httpx.Request) -> httpx.Response:
        raise httpx.ReadTimeout(f"timed out reading {request.url}")

    with pytest.raises(ProviderError) as caught:
        _resolve("Breaking Bad", FakeWeb(tmdb=slow))
    assert caught.value.kind == "timeout"
    assert caught.value.provider == "TMDB"


def test_bad_credentials() -> None:
    body = {"status_code": 7, "status_message": "Invalid API key.", "success": False}
    web = FakeWeb(tmdb=lambda request: httpx.Response(401, json=body))
    with pytest.raises(ProviderError) as caught:
        _resolve("Breaking Bad", web)
    assert caught.value.kind == "auth"
    assert caught.value.status == 401
    assert "Invalid API key" in caught.value.detail


def test_server_failure() -> None:
    web = FakeWeb(tmdb=lambda request: httpx.Response(503, text="upstream down"))
    with pytest.raises(ProviderError) as caught:
        _resolve("Breaking Bad", web)
    assert caught.value.kind == "unavailable"
    assert caught.value.detail == "HTTP 503"


@pytest.mark.parametrize(
    "response",
    [
        httpx.Response(200, text="<html>not json</html>"),
        httpx.Response(200, json={"results": "nope"}),
        httpx.Response(200, json=["not", "an", "object"]),
        httpx.Response(200, json={"results": [{"media_type": "tv", "name": "No Id"}]}),
    ],
)
def test_malformed_response(response: httpx.Response) -> None:
    with pytest.raises(ProviderError) as caught:
        _resolve("Breaking Bad", FakeWeb(tmdb=lambda request: response))
    assert caught.value.kind == "malformed"


def test_failures_never_leak_the_key() -> None:
    def unreachable(request: httpx.Request) -> httpx.Response:
        raise httpx.ConnectError(f"cannot reach {request.url}")

    def echo(request: httpx.Request) -> httpx.Response:
        return httpx.Response(400, json={"status_message": f"bad request {request.url}"})

    for handler, kind in ((unreachable, "network"), (echo, "http")):
        with pytest.raises(ProviderError) as caught:
            _resolve("Breaking Bad", FakeWeb(tmdb=handler))
        assert caught.value.kind == kind
        assert all(secret not in str(caught.value) for secret in SECRET_VALUES)
        assert caught.value.__cause__ is None


def test_key_is_sent_to_tmdb_only_as_expected() -> None:
    web = FakeWeb()
    _resolve("Breaking Bad", web)
    (request,) = web.sent_to(TMDB_HOST)
    assert request.url.path == "/3/search/multi"
    assert request.url.params["api_key"] == FAKE_SECRETS["TMDB_API_KEY"]
    assert request.url.params["query"] == "Breaking Bad"


def test_missing_key_is_a_configuration_error() -> None:
    async def go() -> None:
        async with FakeWeb().client() as client:
            await resolve_title(client, TMDBCatalog(Settings.from_mapping({})), "Breaking Bad")

    with pytest.raises(ConfigurationError, match="TMDB_API_KEY"):
        run(go())


@pytest.mark.parametrize(
    ("title", "expected"),
    [
        ("Amélie: Le Fabuleux/Destin!", "amelie_le_fabuleux_destin_movie_194"),
        ("../../etc/passwd", "etc_passwd_movie_194"),
        ("進撃の巨人", "universe_movie_194"),
        ("x" * 200, "x" * 48 + "_movie_194"),
    ],
)
def test_universe_id_is_stable_and_filesystem_safe(title: str, expected: str) -> None:
    universe_id = make_universe_id(title, MediaType.MOVIE, 194)
    assert universe_id == expected
    assert re.fullmatch(UNIVERSE_ID_PATTERN, universe_id)
