"""Claude — LLM provider. Later phases call it asynchronously, never on the gameplay path."""

from __future__ import annotations

import anthropic
import httpx
import httpx2

from rift_canon.config import Settings
from rift_canon.providers.base import LiveResult, Provider

LIVE_CHECK_TIMEOUT = anthropic.Timeout(10.0, connect=5.0)


class ClaudeProvider(Provider):
    name = "Claude"
    env_vars = ("ANTHROPIC_API_KEY",)
    # The Anthropic SDK runs on httpx2, so it cannot share the pipeline's httpx
    # client. None means the SDK's own transport; tests substitute a mock.
    http_client: httpx2.AsyncClient | None = None

    def __init__(self, settings: Settings, http_client: httpx2.AsyncClient | None = None) -> None:
        super().__init__(settings)
        if http_client is not None:
            self.http_client = http_client

    def is_configured(self) -> bool:
        return self.settings.anthropic_api_key is not None

    def sdk(self, timeout: anthropic.Timeout) -> anthropic.AsyncAnthropic:
        assert self.settings.anthropic_api_key is not None
        # No SDK retries: failures are reported at once and the command is rerun.
        return anthropic.AsyncAnthropic(
            api_key=self.settings.anthropic_api_key.get_secret_value(),
            timeout=timeout,
            max_retries=0,
            http_client=self.http_client,
        )

    async def close(self, sdk: anthropic.AsyncAnthropic) -> None:
        if self.http_client is None:  # an injected client belongs to the caller
            await sdk.close()

    async def live_check(self, client: httpx.AsyncClient) -> LiveResult:
        # Listing models is free and generates nothing.
        sdk = self.sdk(LIVE_CHECK_TIMEOUT)
        try:
            page = await sdk.models.list(limit=1)
        except anthropic.APITimeoutError:
            return LiveResult(False, "timeout")
        except anthropic.APIStatusError as exc:
            # Report only the status code: bodies could echo credentials.
            return LiveResult(False, f"HTTP {exc.status_code}")
        except anthropic.APIError as exc:
            return LiveResult(False, type(exc).__name__)
        except AttributeError:
            # A non-JSON 200 (e.g. a captive portal page) breaks the SDK's page parsing.
            return LiveResult(False, "HTTP 200, unexpected response shape")
        finally:
            await self.close(sdk)
        if isinstance(getattr(page, "data", None), list):
            return LiveResult(True, "HTTP 200")
        return LiveResult(False, "HTTP 200, unexpected response shape")
