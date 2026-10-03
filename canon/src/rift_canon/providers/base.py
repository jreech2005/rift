"""Provider interface shared by all external services."""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass
from typing import ClassVar

import httpx

from rift_canon.config import Settings

# Every outbound request gets a bounded timeout.
DEFAULT_TIMEOUT = httpx.Timeout(10.0, connect=5.0)


def make_client() -> httpx.AsyncClient:
    return httpx.AsyncClient(timeout=DEFAULT_TIMEOUT, follow_redirects=False)


@dataclass(frozen=True)
class LiveResult:
    """Outcome of a harmless connectivity check. ``ok=None`` means skipped."""

    ok: bool | None
    detail: str

    @classmethod
    def skipped(cls, detail: str) -> LiveResult:
        return cls(None, detail)

    @classmethod
    def from_status(cls, response: httpx.Response) -> LiveResult:
        # Report only the status code: URLs or bodies could echo credentials.
        return cls(response.is_success, f"HTTP {response.status_code}")

    @classmethod
    def from_error(cls, exc: Exception) -> LiveResult:
        return cls(False, type(exc).__name__)


class Provider(ABC):
    """An external service. Implementations must never expose secret values."""

    name: ClassVar[str]
    env_vars: ClassVar[tuple[str, ...]]

    def __init__(self, settings: Settings) -> None:
        self.settings = settings

    @abstractmethod
    def is_configured(self) -> bool:
        """True when every required setting is present (no network)."""

    async def live_check(self, client: httpx.AsyncClient) -> LiveResult:
        """Free, read-only connectivity check. Default: not supported."""
        return LiveResult.skipped("no live check")

    async def run_live_check(self, client: httpx.AsyncClient) -> LiveResult:
        if not self.is_configured():
            return LiveResult.skipped("not configured")
        try:
            return await self.live_check(client)
        except (httpx.HTTPError, OSError, TimeoutError) as exc:
            return LiveResult.from_error(exc)
