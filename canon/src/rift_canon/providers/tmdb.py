"""TMDB — film/TV metadata. Phase 1 will use it for title resolution."""

from __future__ import annotations

import httpx

from rift_canon.providers.base import LiveResult, Provider

BASE_URL = "https://api.themoviedb.org/3"


class TMDBProvider(Provider):
    name = "TMDB"
    env_vars = ("TMDB_API_KEY",)

    def is_configured(self) -> bool:
        return self.settings.tmdb_api_key is not None

    def _auth(self) -> tuple[dict[str, str], dict[str, str]]:
        """TMDB accepts a v4 read-access token (JWT) or a v3 api_key."""
        assert self.settings.tmdb_api_key is not None
        key = self.settings.tmdb_api_key.get_secret_value()
        if key.startswith("eyJ"):
            return {"Authorization": f"Bearer {key}"}, {}
        return {}, {"api_key": key}

    async def live_check(self, client: httpx.AsyncClient) -> LiveResult:
        headers, params = self._auth()
        # One small, free search request; validates auth and the response shape.
        response = await client.get(
            f"{BASE_URL}/search/movie",
            headers=headers,
            params={**params, "query": "Inception", "page": 1},
        )
        return LiveResult.from_json_list(response, "results")
