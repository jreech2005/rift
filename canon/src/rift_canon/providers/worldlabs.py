"""World Labs — 3D world generation (Phase 6).

No live check: there is no confirmed free endpoint, and generation consumes
credits. Only configuration is reported.
"""

from __future__ import annotations

import httpx

from rift_canon.providers.base import LiveResult, Provider


class WorldLabsProvider(Provider):
    name = "World Labs"
    env_vars = ("WORLD_LABS_API_KEY",)

    def is_configured(self) -> bool:
        return self.settings.world_labs_api_key is not None

    async def live_check(self, client: httpx.AsyncClient) -> LiveResult:
        return LiveResult.skipped("no live check (avoids paid generation)")
