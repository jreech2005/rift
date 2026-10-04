"""Gemini — LLM provider. Later phases call it asynchronously, never on the gameplay path."""

from __future__ import annotations

import httpx

from rift_canon.providers.base import LiveResult, Provider

BASE_URL = "https://generativelanguage.googleapis.com/v1beta"


class GeminiProvider(Provider):
    name = "Gemini"
    env_vars = ("GEMINI_API_KEY",)

    def is_configured(self) -> bool:
        return self.settings.gemini_api_key is not None

    async def live_check(self, client: httpx.AsyncClient) -> LiveResult:
        # Listing models is free and generates nothing.
        assert self.settings.gemini_api_key is not None
        response = await client.get(
            f"{BASE_URL}/models",
            headers={"x-goog-api-key": self.settings.gemini_api_key.get_secret_value()},
            params={"pageSize": 1},
        )
        return LiveResult.from_json_list(response, "models")
