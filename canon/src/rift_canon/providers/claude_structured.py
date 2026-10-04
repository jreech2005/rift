"""Claude JSON generation through the Messages API.

Extends ``ClaudeProvider`` (configuration + SDK client) with one generation
call. Pre-game only: never called on the gameplay path.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any

import anthropic

from rift_canon.errors import ConfigurationError, ProviderError
from rift_canon.providers._http import MAX_PROVIDER_MESSAGE, _error_message, _kind, redact
from rift_canon.providers.claude import ClaudeProvider

# Read timeout is per streamed chunk, not for the whole generation.
GENERATION_TIMEOUT = anthropic.Timeout(180.0, connect=5.0)
# Thinking counts against this, so it is well above the size of a WorldBible.
MAX_OUTPUT_TOKENS = 32_000
EFFORT = "medium"
# A request declined by a safety classifier is re-run server-side on
# Anthropic's recommended fallback model instead of failing the compile.
FALLBACK_BETA = "server-side-fallback-2026-07-01"

# JSON Schema keywords sent to Claude. Anything else Pydantic emits (title,
# default, pattern, minLength, ...) is dropped; Pydantic still enforces those
# when the response is validated.
_SUPPORTED_KEYWORDS = frozenset(
    {"additionalProperties", "anyOf", "description", "enum", "format", "items", "required", "type"}
)
# Structured outputs do not support numeric bounds or array size limits. They
# are stated in the description instead, and Pydantic enforces them on the
# response.
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


def to_claude_schema(schema: dict[str, Any]) -> dict[str, Any]:
    """Reduce a Pydantic JSON Schema to the supported subset.

    ``$ref`` is inlined (the schemas used here are not recursive), ``const``
    becomes a one-value ``enum``, size limits move into the description, and
    every object is closed (``additionalProperties: false``), which structured
    outputs require.
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
        if converted.get("type") == "object":
            converted["additionalProperties"] = False
        if hint := _limits_hint(node):
            parts = (converted.get("description"), f"{hint}.")
            converted["description"] = " ".join(part for part in parts if part)
        return converted

    return convert(schema)


_SCALARS = {"string", "integer", "number", "boolean", "null"}


def shape_outline(schema: dict[str, Any]) -> str:
    """A compact, readable outline of a ``to_claude_schema`` result for a prompt.

    Large schemas exceed the grammar size structured outputs can compile, so
    the model is shown the shape as text and the reply is validated locally.
    A description is printed once, the first time its field appears.
    """
    seen: set[str] = set()

    def render(node: dict[str, Any], depth: int) -> str:
        if "enum" in node:
            return " | ".join(json.dumps(value) for value in node["enum"])
        if "anyOf" in node:
            return " | ".join(render(option, depth) for option in node["anyOf"])
        kind = node.get("type")
        if kind == "array":
            return f"[{render(node.get('items', {}), depth)}, ...]"
        if kind != "object":
            return kind if kind in _SCALARS else "any"
        pad = "  " * (depth + 1)
        lines = ["{"]
        for name, prop in node.get("properties", {}).items():
            description = prop.get("description")
            if description and description not in seen:
                seen.add(description)
                lines.append(f"{pad}// {description}")
            lines.append(f"{pad}{json.dumps(name)}: {render(prop, depth + 1)},")
        lines.append("  " * depth + "}")
        return "\n".join(lines)

    return render(schema, 0)


def extract_json(text: str) -> str:
    """The JSON object in ``text``, without any code fence or prose around it."""
    start, end = text.find("{"), text.rfind("}")
    return text[start : end + 1] if 0 <= start < end else text


@dataclass(frozen=True)
class ClaudeResult:
    text: str
    model: str
    usage: dict[str, int]


def parse_message(message: Any) -> ClaudeResult:
    if message.stop_reason == "refusal":
        raise ProviderError("Claude", "refused", "the model declined the request")
    if message.stop_reason == "max_tokens":
        raise ProviderError("Claude", "incomplete", "generation stopped before the output ended")
    # Model text only; thinking blocks are skipped.
    text = "".join(block.text for block in message.content if block.type == "text")
    if not text.strip():
        raise ProviderError("Claude", "malformed", "response contained no text output")
    usage = {
        "input_tokens": message.usage.input_tokens,
        "output_tokens": message.usage.output_tokens,
    }
    usage["total_tokens"] = sum(usage.values())
    return ClaudeResult(text=text, model=message.model, usage=usage)


class ClaudeStructured(ClaudeProvider):
    async def generate_json(
        self,
        *,
        model: str,
        system: str,
        prompt: str,
        schema: dict[str, Any] | None = None,
    ) -> ClaudeResult:
        """One stateless generation.

        With ``schema`` the output is constrained server-side; that only suits
        small schemas. Without it the caller must validate the returned text.
        """
        if self.settings.anthropic_api_key is None:
            raise ConfigurationError("ANTHROPIC_API_KEY is not set")
        key = self.settings.anthropic_api_key.get_secret_value()
        output_config: dict[str, Any] = {"effort": EFFORT}
        if schema is not None:
            output_config["format"] = {"type": "json_schema", "schema": schema}
        sdk = self.sdk(GENERATION_TIMEOUT)
        try:
            # Streamed so a long generation cannot hit an idle HTTP timeout.
            async with sdk.beta.messages.stream(
                model=model,
                max_tokens=MAX_OUTPUT_TOKENS,
                system=system,
                messages=[{"role": "user", "content": prompt}],
                output_config=output_config,
                betas=[FALLBACK_BETA],
                fallbacks="default",
            ) as stream:
                message = await stream.get_final_message()
        except anthropic.APITimeoutError:
            raise ProviderError(self.name, "timeout", "no response within the timeout") from None
        except anthropic.APIConnectionError as exc:
            # Type name only: transport messages can include request details.
            raise ProviderError(self.name, "network", type(exc).__name__) from None
        except anthropic.APIStatusError as exc:
            status = exc.status_code
            text = redact(_error_message(exc.body), (key,))[:MAX_PROVIDER_MESSAGE]
            detail = f"HTTP {status}: {text}" if text else f"HTTP {status}"
            raise ProviderError(self.name, _kind(status), detail, status) from None
        except anthropic.APIError as exc:
            raise ProviderError(self.name, "malformed", type(exc).__name__) from None
        finally:
            await self.close(sdk)
        return parse_message(message)
