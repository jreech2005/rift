"""Shared HTTP plumbing for provider calls.

One place turns transport and status failures into ``ProviderError`` without
leaking request URLs, headers or keys.
"""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

import httpx

from rift_canon.errors import ProviderError, ProviderErrorKind

MAX_PROVIDER_MESSAGE = 300


def redact(text: str, secrets: Sequence[str]) -> str:
    for secret in secrets:
        if secret:
            text = text.replace(secret, "[redacted]")
    return text


def _kind(status: int) -> ProviderErrorKind:
    if status in (401, 403):
        return "auth"
    if status == 429:
        return "rate_limited"
    if status >= 500:
        return "unavailable"
    return "http"


def _error_message(body: Any) -> str:
    """The provider's own error text: Google ``error.message``, MediaWiki
    ``error.info``, TMDB ``status_message``."""
    if not isinstance(body, dict):
        return ""
    error = body.get("error")
    if isinstance(error, dict):
        for key in ("message", "info"):
            if isinstance(error.get(key), str):
                return error[key]
    message = body.get("status_message")
    return message if isinstance(message, str) else ""


async def request_json(
    client: httpx.AsyncClient,
    provider: str,
    method: str,
    url: str,
    *,
    secrets: Sequence[str] = (),
    **kwargs: Any,
) -> Any:
    """Send a request and return the decoded JSON body, or raise ``ProviderError``."""
    try:
        response = await client.request(method, url, **kwargs)
    except httpx.TimeoutException:
        raise ProviderError(provider, "timeout", "no response within the timeout") from None
    except httpx.HTTPError as exc:
        # Type name only: httpx messages can include the request URL (and so a key).
        raise ProviderError(provider, "network", type(exc).__name__) from None

    try:
        body = response.json()
    except ValueError:
        body = None
    status = response.status_code
    if not response.is_success:
        message = redact(_error_message(body), secrets)[:MAX_PROVIDER_MESSAGE]
        detail = f"HTTP {status}: {message}" if message else f"HTTP {status}"
        raise ProviderError(provider, _kind(status), detail, status)
    if body is None:
        raise ProviderError(provider, "malformed", "response was not JSON", status)
    return body
