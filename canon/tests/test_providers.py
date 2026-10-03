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
        return httpx.Response(200, json={})

    for cls in (TMDBProvider, GeminiProvider, ElevenLabsProvider):
        result = _run(cls(full_settings), handler)
        assert result == LiveResult(True, "HTTP 200")

    assert seen["api.themoviedb.org"].url.params["api_key"] == FAKE_SECRETS["TMDB_API_KEY"]
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
