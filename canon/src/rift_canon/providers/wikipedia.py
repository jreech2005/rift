"""Wikipedia (and Wikidata) through the public MediaWiki API. No key required.

Used for canon acquisition only. Requests are sequential and paced: Wikimedia
throttles bursts from anonymous clients.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from typing import Any

import httpx

from rift_canon import __version__
from rift_canon.config import Settings
from rift_canon.errors import ProviderError
from rift_canon.providers._http import request_json
from rift_canon.providers.base import Provider

API_URL = "https://en.wikipedia.org/w/api.php"
WIKIDATA_API_URL = "https://www.wikidata.org/w/api.php"
USER_AGENT = f"rift-canon/{__version__} (Rift universe compiler; python-httpx)"
LICENSE = "CC BY-SA 4.0"

REQUEST_DELAY_SECONDS = 0.5
RETRY_DELAY_SECONDS = 3.0

# Plain-text extract of a whole page, with its URL, Wikidata item and revision.
_PAGE_PARAMS = {
    "action": "query",
    "formatversion": 2,
    "redirects": 1,
    "prop": "extracts|info|pageprops|revisions",
    "explaintext": 1,
    "exsectionformat": "wiki",
    "inprop": "url",
    "ppprop": "wikibase_item|disambiguation",
    "rvprop": "ids|timestamp",
}


@dataclass(frozen=True)
class WikiPage:
    pageid: int
    title: str
    url: str
    extract: str
    revid: int | None
    rev_timestamp: str | None
    wikibase_item: str | None
    disambiguation: bool


def _first_page(data: dict[str, Any]) -> WikiPage | None:
    query = data.get("query")
    pages = query.get("pages") if isinstance(query, dict) else None
    if not isinstance(pages, list) or not pages or not isinstance(pages[0], dict):
        return None
    page = pages[0]
    pageid, title, extract = page.get("pageid"), page.get("title"), page.get("extract")
    if page.get("missing") or not isinstance(pageid, int) or not isinstance(title, str):
        return None
    if not isinstance(extract, str) or not extract.strip():
        return None
    props = page.get("pageprops") if isinstance(page.get("pageprops"), dict) else {}
    revisions = page.get("revisions")
    revision = revisions[0] if isinstance(revisions, list) and revisions else {}
    url = page.get("canonicalurl") or page.get("fullurl")
    return WikiPage(
        pageid=pageid,
        title=title,
        url=url if isinstance(url, str) else f"https://en.wikipedia.org/?curid={pageid}",
        extract=extract,
        revid=revision.get("revid") if isinstance(revision, dict) else None,
        rev_timestamp=revision.get("timestamp") if isinstance(revision, dict) else None,
        wikibase_item=props.get("wikibase_item"),
        disambiguation="disambiguation" in props,
    )


class WikipediaProvider(Provider):
    name = "Wikipedia"
    env_vars = ()

    def __init__(
        self, settings: Settings, *, delay: float | None = None, retry_delay: float | None = None
    ) -> None:
        super().__init__(settings)
        self.delay = REQUEST_DELAY_SECONDS if delay is None else delay
        self.retry_delay = RETRY_DELAY_SECONDS if retry_delay is None else retry_delay
        self._sent = 0

    def is_configured(self) -> bool:
        return True

    async def _request(self, client: httpx.AsyncClient, url: str, params: dict[str, Any]) -> Any:
        return await request_json(
            client,
            self.name,
            "GET",
            url,
            headers={"User-Agent": USER_AGENT},
            params={"format": "json", **params},
        )

    async def _get(self, client: httpx.AsyncClient, url: str, **params: Any) -> dict[str, Any]:
        if self._sent:
            await asyncio.sleep(self.delay)
        self._sent += 1
        try:
            data = await self._request(client, url, params)
        except ProviderError as exc:
            if exc.kind != "rate_limited":
                raise
            # Exactly one retry after a pause; a second refusal is reported.
            await asyncio.sleep(self.retry_delay)
            data = await self._request(client, url, params)
        if not isinstance(data, dict):
            raise ProviderError(self.name, "malformed", "response is not a JSON object")
        error = data.get("error")
        if isinstance(error, dict):
            raise ProviderError(self.name, "http", f"API error: {error.get('code', 'unknown')}")
        return data

    async def sitelink_title(self, client: httpx.AsyncClient, wikidata_id: str) -> str | None:
        """English Wikipedia article title for a Wikidata item, if it has one."""
        data = await self._get(
            client,
            WIKIDATA_API_URL,
            action="wbgetentities",
            ids=wikidata_id,
            props="sitelinks",
            sitefilter="enwiki",
        )
        try:
            title = data["entities"][wikidata_id]["sitelinks"]["enwiki"]["title"]
        except (KeyError, TypeError):
            return None
        return title if isinstance(title, str) and title else None

    async def fetch_page(self, client: httpx.AsyncClient, title: str) -> WikiPage | None:
        return _first_page(await self._get(client, API_URL, **_PAGE_PARAMS, titles=title))

    async def search_page(self, client: httpx.AsyncClient, query: str) -> WikiPage | None:
        """Search and fetch the top article hit in a single request."""
        data = await self._get(
            client,
            API_URL,
            **_PAGE_PARAMS,
            generator="search",
            gsrsearch=query,
            gsrlimit=1,
            gsrnamespace=0,
        )
        return _first_page(data)
