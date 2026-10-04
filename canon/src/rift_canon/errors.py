"""Errors raised by the universe compiler.

Messages are safe to print: they never contain request URLs, headers or secret
values. The CLI reports these without a traceback.
"""

from __future__ import annotations

from typing import Literal

ProviderErrorKind = Literal[
    "timeout",  # no response within the explicit timeout
    "network",  # connection-level failure
    "auth",  # credentials rejected
    "rate_limited",
    "unavailable",  # 5xx, or the provider reported a failed job
    "http",  # any other non-success status
    "malformed",  # response was not the expected shape
    "incomplete",  # generation stopped before finishing
    "refused",  # the model declined to answer
]


class RiftCanonError(Exception):
    """Base class for expected pipeline failures."""


class ConfigurationError(RiftCanonError):
    """A required setting (for example an API key) is missing."""


class ProviderError(RiftCanonError):
    """An external service failed. ``detail`` is safe to show."""

    def __init__(
        self, provider: str, kind: ProviderErrorKind, detail: str, status: int | None = None
    ) -> None:
        super().__init__(f"{provider} {kind}: {detail}")
        self.provider = provider
        self.kind = kind
        self.detail = detail
        self.status = status


class TitleNotFoundError(RiftCanonError):
    """No movie or TV series matched the requested title."""


class CanonAcquisitionError(RiftCanonError):
    """Not enough source material could be retrieved to compile a universe."""


class CompilationError(RiftCanonError):
    """The LLM output failed validation after the bounded repair attempt."""

    def __init__(self, message: str, errors: list[str], raw_outputs: list[str]) -> None:
        super().__init__(message)
        self.errors = errors
        self.raw_outputs = raw_outputs
