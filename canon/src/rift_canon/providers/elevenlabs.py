"""ElevenLabs — speech synthesis (Phase 6)."""

from __future__ import annotations

import httpx

from rift_canon.providers.base import LiveResult, Provider

BASE_URL = "https://api.elevenlabs.io/v1"


class ElevenLabsProvider(Provider):
    name = "ElevenLabs"
    env_vars = ("ELEVENLABS_API_KEY",)

    def is_configured(self) -> bool:
        return self.settings.elevenlabs_api_key is not None

    async def live_check(self, client: httpx.AsyncClient) -> LiveResult:
        # Account lookup: requires a valid key, consumes no characters/credits.
        assert self.settings.elevenlabs_api_key is not None
        response = await client.get(
            f"{BASE_URL}/user",
            headers={"xi-api-key": self.settings.elevenlabs_api_key.get_secret_value()},
        )
        return LiveResult.from_status(response)
