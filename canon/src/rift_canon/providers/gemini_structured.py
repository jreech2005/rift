"""Gemini schema-constrained generation through the Interactions API.

Extends the Phase 0 ``GeminiProvider`` (configuration + base URL) with one
generation call. Pre-game only: never called on the gameplay path.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

import httpx

from rift_canon.errors import ConfigurationError, ProviderError
from rift_canon.providers._http import request_json
from rift_canon.providers.gemini import BASE_URL, GeminiProvider

INTERACTIONS_URL = f"{BASE_URL}/interactions"
# Generating a whole WorldBible takes far longer than a metadata lookup.
GENERATION_TIMEOUT = httpx.Timeout(180.0, connect=5.0)
MAX_OUTPUT_TOKENS = 32_768
THINKING_LEVEL = "low"

# JSON Schema keywords sent to Gemini. Anything else Pydantic emits (title,
# default, pattern, minLength, ...) is dropped; Pydantic still enforces those
# when the response is validated.
_SUPPORTED_KEYWORDS = frozenset(
    {"additionalProperties", "anyOf", "description", "enum", "format", "items", "required", "type"}
)
# Size limits are documented as supported, but nested bounded arrays make the
# constrained decoder reject the whole schema (HTTP 400 after ~35 s, seen live
# with gemini-3.8-flash). They are stated in the description instead, and
# Pydantic enforces them on the response.
_LIMITS = (("minItems", "maxItems", " item(s)"), ("minimum", "maximum", ""))


def _limits_hint(node: dict[str, Any]) -> str:
    for low_key, high_key, unit in _LIMITS:
        low, high = node.get(low_key), node.get(high_key)
        if low is not None and high is not None:
            return f"{low} to {high}{unit}"
        if low is not None:
            return f"At least {low}{unit}"
        if high is not None:
            return f"At most {high}{unit}"
    return ""


def to_gemini_schema(schema: dict[str, Any]) -> dict[str, Any]:
    """Reduce a Pydantic JSON Schema to the supported subset.

    ``$ref`` is inlined (the schemas used here are not recursive), ``const``
    becomes a one-value ``enum``, and size limits move into the description.
    """
    definitions = schema.get("$defs", {})

    def convert(node: Any) -> Any:
        if isinstance(node, list):
            return [convert(item) for item in node]
        if not isinstance(node, dict):
            return node
        if len(node.get("allOf", ())) == 1:
            node = {**node["allOf"][0], **{k: v for k, v in node.items() if k != "allOf"}}
        if "$ref" in node:
            target = definitions[node["$ref"].rsplit("/", 1)[-1]]
            return convert({**target, **{k: v for k, v in node.items() if k != "$ref"}})
        converted: dict[str, Any] = {}
        for key, value in node.items():
            if key == "const":
                converted["enum"] = [value]
            elif key == "properties":
                converted[key] = {name: convert(prop) for name, prop in value.items()}
            elif key in _SUPPORTED_KEYWORDS:
                converted[key] = convert(value)
        if hint := _limits_hint(node):
            parts = (converted.get("description"), f"{hint}.")
            converted["description"] = " ".join(part for part in parts if part)
        return converted

    return convert(schema)


@dataclass(frozen=True)
class GeminiResult:
    text: str
    model: str
    usage: dict[str, int]


def _dicts(value: object) -> list[dict[str, Any]]:
    return [item for item in value if isinstance(item, dict)] if isinstance(value, list) else []


def _texts(parts: object) -> list[str]:
    return [
        part["text"]
        for part in _dicts(parts)
        if part.get("type") == "text" and isinstance(part.get("text"), str)
    ]


def _output_text(data: dict[str, Any]) -> str:
    """Model text only; thought steps are skipped."""
    parts: list[str] = []
    for step in _dicts(data.get("steps")):
        if step.get("type") == "model_output":
            parts += _texts(step.get("content"))
    if not parts:  # flat `outputs` list used by earlier Interactions responses
        parts = _texts(data.get("outputs"))
    return "".join(parts)


def parse_interaction(data: Any, requested_model: str) -> GeminiResult:
    if not isinstance(data, dict):
        raise ProviderError("Gemini", "malformed", "response is not a JSON object")
    status = data.get("status")
    if status == "incomplete":
        raise ProviderError("Gemini", "incomplete", "generation stopped before the output ended")
    if status not in (None, "completed"):
        raise ProviderError("Gemini", "unavailable", f"interaction status {status!r}")
    text = _output_text(data)
    if not text.strip():
        raise ProviderError("Gemini", "malformed", "response contained no text output")
    usage = data.get("usage") if isinstance(data.get("usage"), dict) else {}
    model = data.get("model")
    return GeminiResult(
        text=text,
        model=model if isinstance(model, str) and model else requested_model,
        usage={
            key: value
            for key, value in usage.items()
            if key.startswith("total_") and isinstance(value, int)
        },
    )


class GeminiStructured(GeminiProvider):
    async def generate_json(
        self,
        client: httpx.AsyncClient,
        *,
        model: str,
        system_instruction: str,
        prompt: str,
        schema: dict[str, Any],
    ) -> GeminiResult:
        """One stateless generation whose output is constrained to ``schema``."""
        if self.settings.gemini_api_key is None:
            raise ConfigurationError("GEMINI_API_KEY is not set")
        key = self.settings.gemini_api_key.get_secret_value()
        data = await request_json(
            client,
            self.name,
            "POST",
            INTERACTIONS_URL,
            secrets=(key,),
            headers={"x-goog-api-key": key},
            json={
                "model": model,
                "system_instruction": system_instruction,
                "input": prompt,
                "response_format": {
                    "type": "text",
                    "mime_type": "application/json",
                    "schema": schema,
                },
                "generation_config": {
                    "max_output_tokens": MAX_OUTPUT_TOKENS,
                    "thinking_level": THINKING_LEVEL,
                },
                "store": False,
            },
            timeout=GENERATION_TIMEOUT,
        )
        return parse_interaction(data, model)
