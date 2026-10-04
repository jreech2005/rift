import json
from typing import Any

import httpx
import pytest

from rift_canon.compiler import draft_schema
from rift_canon.config import Settings
from rift_canon.errors import ConfigurationError, ProviderError
from rift_canon.providers.gemini_structured import (
    GENERATION_TIMEOUT,
    GeminiResult,
    GeminiStructured,
    parse_interaction,
    to_gemini_schema,
)
from rift_canon.world_bible import WorldBibleDraft
from tests.conftest import FAKE_SECRETS
from tests.support import GEMINI_HOST, FakeWeb, fake_settings, interaction, run

SCHEMA = {"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"]}


def _generate(web: FakeWeb, settings: Settings | None = None) -> GeminiResult:
    async def go() -> GeminiResult:
        async with web.client() as client:
            return await GeminiStructured(settings or fake_settings()).generate_json(
                client,
                model="gemini-3.8-flash",
                system_instruction="Be exact.",
                prompt="Return ok.",
                schema=SCHEMA,
            )

    return run(go())


def _keywords(node: Any, found: set[str]) -> set[str]:
    if isinstance(node, dict):
        for key, value in node.items():
            if key == "properties":
                for prop in value.values():
                    _keywords(prop, found)
            else:
                found.add(key)
                _keywords(value, found)
    elif isinstance(node, list):
        for item in node:
            _keywords(item, found)
    return found


def test_schema_is_reduced_to_supported_keywords() -> None:
    raw = WorldBibleDraft.model_json_schema()
    assert "$defs" in raw  # Pydantic emits references; Gemini gets them inlined.
    schema = to_gemini_schema(raw)
    assert _keywords(schema, set()) <= {
        "additionalProperties",
        "anyOf",
        "description",
        "enum",
        "items",
        "required",
        "type",
    }
    properties = schema["properties"]
    assert properties["player_role"]["properties"]["classification"]["enum"] == ["generated"]
    assert properties["era"]["properties"]["classification"]["enum"] == [
        "canon",
        "inferred",
        "generated",
    ]
    assert set(schema["required"]) == set(WorldBibleDraft.model_fields)


def test_size_limits_move_into_descriptions() -> None:
    # Gemini rejects this schema when nested arrays carry minItems/maxItems, so the
    # limits are told to the model in prose and enforced by Pydantic afterwards.
    properties = to_gemini_schema(WorldBibleDraft.model_json_schema())["properties"]
    opening = properties["opening_conflict"]["properties"]
    assert properties["characters"]["description"] == "3 to 8 item(s)."
    assert properties["factions"]["description"] == "At most 4 item(s)."
    assert opening["estimated_minutes"] == {"type": "integer", "description": "10 to 20."}
    assert opening["decision_options"]["description"] == (
        "Distinct choices open to the player right away. 2 to 4 item(s)."
    )
    party_ids = properties["important_conflicts"]["items"]["properties"]["party_ids"]
    assert (
        party_ids["description"] == "Character ids, faction ids, or 'player'. At least 1 item(s)."
    )


def test_source_refs_are_constrained_to_real_sources() -> None:
    ids = ["tmdb:tv:1396", "wikipedia:en:1001"]
    schema = draft_schema(ids)
    properties = schema["properties"]
    refs = [
        properties["era"]["properties"]["source_refs"],
        properties["characters"]["items"]["properties"]["source_refs"],
        properties["characters"]["items"]["properties"]["known_facts"]["items"]["properties"][
            "source_refs"
        ],
        properties["player_role"]["properties"]["source_refs"],
        properties["opening_conflict"]["properties"]["source_refs"],
    ]
    assert all(ref["items"]["enum"] == ids for ref in refs)


def test_request_shape() -> None:
    web = FakeWeb(gemini=['{"ok": true}'])
    result = _generate(web)
    assert result.text == '{"ok": true}'
    assert result.model == "gemini-3.8-flash"
    assert result.usage == {
        "total_input_tokens": 1200,
        "total_output_tokens": 800,
        "total_thought_tokens": 100,
        "total_cached_tokens": 0,
        "total_tool_use_tokens": 0,
        "total_tokens": 2100,
    }

    (request,) = web.sent_to(GEMINI_HOST)
    assert request.method == "POST"
    assert request.url.path == "/v1beta/interactions"
    # The key travels in a header, never in the URL.
    assert request.headers["x-goog-api-key"] == FAKE_SECRETS["GEMINI_API_KEY"]
    assert FAKE_SECRETS["GEMINI_API_KEY"] not in str(request.url)
    assert request.extensions["timeout"]["read"] == GENERATION_TIMEOUT.read

    body = json.loads(request.content)
    assert body["model"] == "gemini-3.8-flash"
    assert body["input"] == "Return ok."
    assert body["system_instruction"] == "Be exact."
    assert body["response_format"] == {
        "type": "text",
        "mime_type": "application/json",
        "schema": SCHEMA,
    }
    assert body["store"] is False
    assert body["generation_config"]["max_output_tokens"] > 0


def test_thoughts_are_not_output() -> None:
    result = parse_interaction(interaction('{"ok": true}'), "m")
    assert result.text == '{"ok": true}'
    assert "thinking" not in result.text


def test_flat_outputs_shape_is_accepted() -> None:
    data = {
        "status": "completed",
        "outputs": [{"type": "thought", "summary": "x"}, {"type": "text", "text": "{}"}],
    }
    assert parse_interaction(data, "requested-model") == GeminiResult("{}", "requested-model", {})


@pytest.mark.parametrize(
    ("data", "kind"),
    [
        (interaction("{", status="incomplete"), "incomplete"),
        (interaction("{}", status="failed"), "unavailable"),
        ({"status": "completed", "steps": []}, "malformed"),
        ({"status": "completed", "steps": [{"type": "model_output", "content": []}]}, "malformed"),
        (["not", "an", "object"], "malformed"),
    ],
)
def test_unusable_interactions(data: Any, kind: str) -> None:
    with pytest.raises(ProviderError) as caught:
        parse_interaction(data, "m")
    assert caught.value.kind == kind


def test_provider_errors_are_useful_and_safe() -> None:
    key = FAKE_SECRETS["GEMINI_API_KEY"]
    error = {"error": {"code": 400, "message": f"Bad schema for key {key}", "status": "INVALID"}}
    cases = [
        (httpx.Response(400, json=error), "http", "HTTP 400: Bad schema for key [redacted]"),
        (httpx.Response(401, json={"error": {"message": "API key not valid."}}), "auth", None),
        (httpx.Response(429, json={"error": {"message": "Quota exceeded."}}), "rate_limited", None),
        (httpx.Response(503, text="overloaded"), "unavailable", "HTTP 503"),
        (httpx.Response(200, text="not json"), "malformed", None),
    ]
    for response, kind, detail in cases:
        with pytest.raises(ProviderError) as caught:
            _generate(FakeWeb(gemini=[response]))
        assert caught.value.kind == kind
        assert caught.value.provider == "Gemini"
        assert key not in str(caught.value)
        if detail:
            assert caught.value.detail == detail


def test_timeout() -> None:
    def slow(request: httpx.Request) -> httpx.Response:
        raise httpx.ReadTimeout("timed out")

    with pytest.raises(ProviderError) as caught:
        _generate(FakeWeb(gemini_handler=slow))
    assert caught.value.kind == "timeout"


def test_missing_key_makes_no_request() -> None:
    web = FakeWeb()
    with pytest.raises(ConfigurationError, match="GEMINI_API_KEY"):
        _generate(web, Settings.from_mapping({}))
    assert web.requests == []
