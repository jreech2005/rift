import json
from typing import Any

import httpx
import pytest

from rift_canon.config import Settings
from rift_canon.errors import ConfigurationError, ProviderError
from rift_canon.providers.claude_structured import (
    FALLBACK_BETA,
    GENERATION_TIMEOUT,
    ClaudeResult,
    extract_json,
    shape_outline,
    to_claude_schema,
)
from rift_canon.world_bible import WorldBibleDraft
from tests.conftest import FAKE_SECRETS
from tests.support import CLAUDE_HOST, FakeWeb, message_stream, run

SCHEMA = {
    "type": "object",
    "properties": {"ok": {"type": "boolean"}},
    "required": ["ok"],
    "additionalProperties": False,
}


def _generate(web: FakeWeb, settings: Settings | None = None) -> ClaudeResult:
    return run(
        web.llm(settings).generate_json(
            model="claude-opus-5-5", system="Be exact.", prompt="Return ok.", schema=SCHEMA
        )
    )


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


def _objects(node: Any) -> list[dict[str, Any]]:
    if isinstance(node, list):
        return [found for item in node for found in _objects(item)]
    if not isinstance(node, dict):
        return []
    nested = [found for value in node.values() for found in _objects(value)]
    return [node, *nested] if node.get("type") == "object" else nested


def test_schema_is_reduced_to_supported_keywords() -> None:
    raw = WorldBibleDraft.model_json_schema()
    assert "$defs" in raw  # Pydantic emits references; Claude gets them inlined.
    schema = to_claude_schema(raw)
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


def test_every_object_is_closed() -> None:
    # Structured outputs reject an object without additionalProperties: false.
    objects = _objects(to_claude_schema(WorldBibleDraft.model_json_schema()))
    assert len(objects) > 10
    assert all(node["additionalProperties"] is False for node in objects)


def test_size_limits_move_into_descriptions() -> None:
    # Structured outputs do not support size limits, so they are told to the model
    # in prose and enforced by Pydantic afterwards.
    properties = to_claude_schema(WorldBibleDraft.model_json_schema())["properties"]
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


def test_shape_outline_is_compact_and_complete() -> None:
    outline = shape_outline(to_claude_schema(WorldBibleDraft.model_json_schema()))
    for field in WorldBibleDraft.model_fields:
        assert f'  "{field}": ' in outline
    assert '"classification": "canon" | "inferred" | "generated",' in outline
    assert '"classification": "generated",' in outline
    assert '"canon_cutoff": string | null,' in outline
    assert "// 3 to 8 item(s)." in outline
    assert '"estimated_minutes": integer,' in outline
    # Repeated field descriptions are printed once.
    assert outline.count("canon: stated in the cited evidence.") == 1
    assert len(outline) < 6000


def test_extract_json_drops_fences_and_prose() -> None:
    assert extract_json('{"a": {"b": 1}}') == '{"a": {"b": 1}}'
    assert extract_json('```json\n{"a": 1}\n```') == '{"a": 1}'
    assert extract_json('Here it is:\n{"a": 1}\nDone.') == '{"a": 1}'
    assert extract_json("not json") == "not json"


def test_schema_is_optional() -> None:
    web = FakeWeb(claude=['{"ok": true}'])
    result = run(web.llm().generate_json(model="claude-opus-5-5", system="s", prompt="p"))
    assert result.text == '{"ok": true}'
    (body,) = web.claude_bodies()
    assert "format" not in body["output_config"]
    assert "tools" not in body


def test_request_shape() -> None:
    web = FakeWeb(claude=['{"ok": true}'])
    result = _generate(web)
    assert result.text == '{"ok": true}'  # the thinking block is not output
    assert result.model == "claude-opus-5-5"
    assert result.usage == {"input_tokens": 1200, "output_tokens": 800, "total_tokens": 2000}

    (request,) = web.sent_to(CLAUDE_HOST)
    assert request.method == "POST"
    assert request.url.path == "/v1/messages"
    # The key travels in a header, never in the URL.
    assert request.headers["x-api-key"] == FAKE_SECRETS["ANTHROPIC_API_KEY"]
    assert FAKE_SECRETS["ANTHROPIC_API_KEY"] not in str(request.url)
    assert request.extensions["timeout"]["read"] == GENERATION_TIMEOUT.read
    assert FALLBACK_BETA in request.headers["anthropic-beta"]

    body = json.loads(request.content)
    assert body["model"] == "claude-opus-5-5"
    assert body["system"] == "Be exact."
    assert body["messages"] == [{"role": "user", "content": "Return ok."}]
    assert body["output_config"]["format"] == {"type": "json_schema", "schema": SCHEMA}
    assert body["fallbacks"] == "default"
    assert body["stream"] is True
    assert body["max_tokens"] > 0


@pytest.mark.parametrize(
    ("reply", "kind"),
    [
        (message_stream("{", stop_reason="max_tokens"), "incomplete"),
        (message_stream("", stop_reason="refusal"), "refused"),
        (message_stream("  "), "malformed"),
    ],
)
def test_unusable_messages(reply: httpx.Response, kind: str) -> None:
    with pytest.raises(ProviderError) as caught:
        _generate(FakeWeb(claude=[reply]))
    assert caught.value.kind == kind


def _error(kind: str, message: str) -> dict[str, Any]:
    return {"type": "error", "error": {"type": kind, "message": message}}


def test_provider_errors_are_useful_and_safe() -> None:
    key = FAKE_SECRETS["ANTHROPIC_API_KEY"]
    bad = _error("invalid_request_error", f"Bad schema for key {key}")
    cases = [
        (httpx.Response(400, json=bad), "http", "HTTP 400: Bad schema for key [redacted]"),
        (
            httpx.Response(401, json=_error("authentication_error", "invalid x-api-key")),
            "auth",
            None,
        ),
        (httpx.Response(429, json=_error("rate_limit_error", "Slow down.")), "rate_limited", None),
        (httpx.Response(529, text="overloaded"), "unavailable", "HTTP 529"),
    ]
    for response, kind, detail in cases:
        web = FakeWeb(claude=[response])
        with pytest.raises(ProviderError) as caught:
            _generate(web)
        assert caught.value.kind == kind
        assert caught.value.provider == "Claude"
        assert key not in str(caught.value)
        if detail:
            assert caught.value.detail == detail
        assert len(web.sent_to(CLAUDE_HOST)) == 1  # the SDK does not retry


def test_timeout() -> None:
    def slow(request: httpx.Request) -> httpx.Response:
        raise httpx.ReadTimeout("timed out")

    with pytest.raises(ProviderError) as caught:
        _generate(FakeWeb(claude_handler=slow))
    assert caught.value.kind == "timeout"


def test_missing_key_makes_no_request() -> None:
    web = FakeWeb()
    with pytest.raises(ConfigurationError, match="ANTHROPIC_API_KEY"):
        _generate(web, Settings.from_mapping({}))
    assert web.requests == []
