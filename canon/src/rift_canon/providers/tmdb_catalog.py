"""TMDB read API used by the universe compiler: title search and details.

Extends the Phase 0 ``TMDBProvider`` (configuration + auth) with data calls.
"""

from __future__ import annotations

from typing import Any

import httpx

from rift_canon.canon_packet import MediaType
from rift_canon.errors import ConfigurationError, ProviderError
from rift_canon.providers._http import request_json
from rift_canon.providers.tmdb import BASE_URL, TMDBProvider


class TMDBCatalog(TMDBProvider):
    async def _get(self, client: httpx.AsyncClient, path: str, **params: Any) -> Any:
        if self.settings.tmdb_api_key is None:
            raise ConfigurationError("TMDB_API_KEY is not set")
        headers, auth = self._auth()
        return await request_json(
            client,
            self.name,
            "GET",
            f"{BASE_URL}{path}",
            secrets=(self.settings.tmdb_api_key.get_secret_value(),),
            headers=headers,
            params={**auth, **params},
        )

    async def search(self, client: httpx.AsyncClient, query: str) -> list[dict[str, Any]]:
        """Movies, TV series (and people) matching ``query``, in TMDB relevance order."""
        data = await self._get(
            client, "/search/multi", query=query, include_adult="false", language="en-US", page=1
        )
        results = data.get("results") if isinstance(data, dict) else None
        if not isinstance(results, list):
            raise ProviderError(self.name, "malformed", "search response has no results list")
        return [item for item in results if isinstance(item, dict)]

    async def details(
        self, client: httpx.AsyncClient, media_type: MediaType, tmdb_id: int
    ) -> dict[str, Any]:
        """Title details with external ids and billed cast in one request."""
        cast = "aggregate_credits" if media_type is MediaType.TV else "credits"
        data = await self._get(
            client,
            f"/{media_type.value}/{tmdb_id}",
            append_to_response=f"external_ids,{cast}",
            language="en-US",
        )
        if not isinstance(data, dict) or data.get("id") != tmdb_id:
            raise ProviderError(self.name, "malformed", "details response is not the title asked")
        return data
