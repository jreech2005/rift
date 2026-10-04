import asyncio

import httpx

from rift_canon.config import Settings
from rift_canon.providers import (
    ElevenLabsProvider,
    GeminiProvider,
    TiDBProvider,
    TMDBProvider,
    WorldLabsProvider,
    all_providers,
)
from rift_canon.providers.base import LiveResult
from tests.conftest import FAKE_SECRETS, SECRET_VALUES


def test_all_configured(full_settings: Settings) -> None:
    assert all(p.is_configured() for p in all_providers(full_settings))


def test_all_missing(empty_settings: Settings) -> None:
    assert not any(p.is_configured() for p in all_providers(empty_settings))


def test_tidb_requires_every_field() -> None:
    partial = {k: v for k, v in FAKE_SECRETS.items() if k != "TIDB_PASSWORD"}
    assert not TiDBProvider(Settings.from_mapping(partial)).is_configured()


def _run(provider, handler) -> LiveResult:
    async def go() -> LiveResult:
        async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
            return await provider.run_live_check(client)

    return asyncio.run(go())


def test_live_checks_send_keys_in_expected_place(full_settings: Settings) -> None:
    seen: dict[str, httpx.Request] = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen[request.url.host] = request
        return httpx.Response(200, json={"results": [], "models": []})

    for cls in (TMDBProvider, GeminiProvider, ElevenLabsProvider):
        result = _run(cls(full_settings), handler)
        assert result == LiveResult(True, "HTTP 200")

    tmdb = seen["api.themoviedb.org"]
    assert tmdb.method == "GET" and tmdb.url.path == "/3/search/movie"
    assert tmdb.url.params["api_key"] == FAKE_SECRETS["TMDB_API_KEY"]
    assert seen["generativelanguage.googleapis.com"].method == "GET"
    assert seen["generativelanguage.googleapis.com"].url.path == "/v1beta/models"
    assert seen["api.elevenlabs.io"].method == "GET"
    assert seen["api.elevenlabs.io"].url.path == "/v1/user"
    gemini = seen["generativelanguage.googleapis.com"]
    assert gemini.headers["x-goog-api-key"] == FAKE_SECRETS["GEMINI_API_KEY"]
    assert "key" not in gemini.url.params
    assert seen["api.elevenlabs.io"].headers["xi-api-key"] == FAKE_SECRETS["ELEVENLABS_API_KEY"]


def test_tmdb_bearer_token() -> None:
    settings = Settings.from_mapping({"TMDB_API_KEY": "eyJfake.token.value"})
    captured: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        captured.append(request)
        return httpx.Response(200)

    _run(TMDBProvider(settings), handler)
    assert captured[0].headers["authorization"] == "Bearer eyJfake.token.value"
    assert "api_key" not in captured[0].url.params


def test_live_failure_does_not_leak_secrets(full_settings: Settings) -> None:
    def unauthorized(request: httpx.Request) -> httpx.Response:
        return httpx.Response(401, text=f"bad key {request.url}")

    def boom(request: httpx.Request) -> httpx.Response:
        raise httpx.ConnectError(f"cannot reach {request.url}")

    for handler in (unauthorized, boom):
        for cls in (TMDBProvider, GeminiProvider, ElevenLabsProvider):
            result = _run(cls(full_settings), handler)
            assert result.ok is False
            assert all(secret not in result.detail for secret in SECRET_VALUES)


def test_world_labs_never_calls_network(full_settings: Settings) -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        raise AssertionError("World Labs live check must not make requests")

    assert _run(WorldLabsProvider(full_settings), handler).ok is None


def test_unconfigured_live_check_is_skipped(empty_settings: Settings) -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        raise AssertionError("unconfigured provider must not make requests")

    for provider in all_providers(empty_settings):
        assert _run(provider, handler) == LiveResult.skipped("not configured")


def test_unexpected_body_is_a_failure(full_settings: Settings) -> None:
    def wrong_shape(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json={"status_message": "nope"})

    def not_json(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, text="<html>captive portal</html>")

    for cls in (TMDBProvider, GeminiProvider):
        assert _run(cls(full_settings), wrong_shape) == LiveResult(
            False, "HTTP 200, unexpected response shape"
        )
        assert _run(cls(full_settings), not_json) == LiveResult(False, "HTTP 200, invalid JSON")


def test_timeout_is_reported_cleanly(full_settings: Settings) -> None:
    def slow(request: httpx.Request) -> httpx.Response:
        raise httpx.ReadTimeout(f"timed out reading {request.url}")

    for cls in (TMDBProvider, GeminiProvider, ElevenLabsProvider):
        assert _run(cls(full_settings), slow) == LiveResult(False, "timeout")


def test_tidb_unreachable_is_a_clean_failure() -> None:
    # Port 1 on localhost: refused immediately, no external traffic.
    settings = Settings.from_mapping({**FAKE_SECRETS, "TIDB_PORT": "1"})
    result = _run(TiDBProvider(settings), lambda request: httpx.Response(500))
    assert result.ok is False
    assert all(secret not in result.detail for secret in SECRET_VALUES)
