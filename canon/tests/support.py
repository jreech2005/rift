"""Offline doubles for TMDB, Wikidata, Wikipedia and Claude.

Every Phase 1 test runs against ``FakeWeb``; nothing touches the network or
spends API credits. Wikipedia texts are short synthetic stand-ins.
"""

from __future__ import annotations

import asyncio
import copy
import json
from collections.abc import Callable, Coroutine
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import httpx
import httpx2

from rift_canon.canon_packet import CanonPacket
from rift_canon.compiler import assemble, ground
from rift_canon.config import Settings
from rift_canon.providers.claude_structured import ClaudeStructured
from rift_canon.world_bible import WorldBible, WorldBibleDraft
from tests.conftest import FAKE_SECRETS

FIXTURES = Path(__file__).parent / "fixtures"
NOW = datetime(2026, 10, 3, 12, 0, tzinfo=UTC)

TMDB_HOST = "api.themoviedb.org"
WIKIDATA_HOST = "www.wikidata.org"
WIKIPEDIA_HOST = "en.wikipedia.org"
CLAUDE_HOST = "api.anthropic.com"

Handler = Callable[[httpx.Request], httpx.Response]


def fixture(name: str) -> Any:
    return json.loads((FIXTURES / name).read_text(encoding="utf-8"))


def run[T](coro: Coroutine[Any, Any, T]) -> T:
    return asyncio.run(coro)


def fake_settings(**overrides: str) -> Settings:
    return Settings.from_mapping({**FAKE_SECRETS, **overrides})


def saved_packet() -> CanonPacket:
    return CanonPacket.model_validate(fixture("canon_packet_breaking_bad.json"))


def valid_draft() -> dict[str, Any]:
    """A fresh copy of the draft a well-behaved model returns for the saved packet."""
    return copy.deepcopy(fixture("world_bible_draft_breaking_bad.json"))


def message_stream(text: str, stop_reason: str = "end_turn") -> httpx.Response:
    """A streamed Messages API response whose model output is ``text``.

    Same event sequence as a live response: a thinking block before the text.
    """
    message = {
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": "claude-opus-5-5",
        "content": [],
        "stop_reason": None,
        "stop_sequence": None,
        "usage": {"input_tokens": 1200, "output_tokens": 1},
    }
    thinking = {"type": "thinking", "thinking": "", "signature": ""}
    events: list[dict[str, Any]] = [
        {"type": "message_start", "message": message},
        {"type": "content_block_start", "index": 0, "content_block": thinking},
        {
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "signature_delta", "signature": "opaque"},
        },
        {"type": "content_block_stop", "index": 0},
        {"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}},
        {"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": text}},
        {"type": "content_block_stop", "index": 1},
        {
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_sequence": None},
            "usage": {"output_tokens": 800},
        },
        {"type": "message_stop"},
    ]
    body = "".join(f"event: {event['type']}\ndata: {json.dumps(event)}\n\n" for event in events)
    return httpx.Response(200, headers={"content-type": "text/event-stream"}, text=body)


def sdk_client(handler: Handler) -> httpx2.AsyncClient:
    """An httpx2 client for the Anthropic SDK, answered by an httpx ``handler``."""

    def bridge(request: httpx2.Request) -> httpx2.Response:
        try:
            response = handler(
                httpx.Request(
                    request.method,
                    str(request.url),
                    headers=list(request.headers.items()),
                    content=request.content,
                    extensions=request.extensions,
                )
            )
        except httpx.TimeoutException as exc:
            raise httpx2.ReadTimeout(str(exc)) from None
        except httpx.HTTPError as exc:
            raise httpx2.ConnectError(str(exc)) from None
        return httpx2.Response(
            response.status_code,
            headers={"content-type": response.headers.get("content-type", "")},
            content=response.content,
        )

    return httpx2.AsyncClient(transport=httpx2.MockTransport(bridge))


SEARCHES = {
    "breaking bad": "tmdb_search_breaking_bad.json",
    "the matrix": "tmdb_search_the_matrix.json",
    "harry potter": "tmdb_search_harry_potter.json",
}
DETAILS = {"/3/tv/1396": "tmdb_tv_1396.json", "/3/movie/603": "tmdb_movie_603.json"}
SITELINKS = {"Q1079": "Breaking Bad", "Q83495": "The Matrix"}

MAIN_EXTRACT = """\
Breaking Bad is an American crime drama television series created by Vince Gilligan. Set in \
Albuquerque, New Mexico, it follows Walter White, a high school chemistry teacher who is \
diagnosed with lung cancer and starts producing methamphetamine with his former student \
Jesse Pinkman.


== Premise ==
Walter White teaches chemistry at J. P. Wynne High School and works a second job at the A1A \
Car Wash. After his diagnosis he partners with Jesse Pinkman to cook methamphetamine in a \
recreational vehicle in the desert outside Albuquerque. His brother-in-law Hank Schrader is an \
agent of the Drug Enforcement Administration who investigates the local drug trade.


== Cast and characters ==
Bryan Cranston as Walter White, a chemistry teacher turned drug manufacturer.
Aaron Paul as Jesse Pinkman,   a former student and small-time dealer.
Anna Gunn as Skyler White, Walter's wife.
Dean Norris as Hank Schrader, a DEA agent and Walter's brother-in-law.


== Production ==
The series was filmed in Albuquerque.


=== Casting ===
Casting took several months.


== Reception ==
Critics praised the series.


== References ==
"""

CHARACTERS_EXTRACT = """\
This is a list of characters in the Breaking Bad franchise.


== Main characters ==


=== Walter White ===
Walter White is a chemistry teacher who becomes a methamphetamine manufacturer.


=== Jesse Pinkman ===
Jesse Pinkman is Walter's former student and business partner.


=== Hank Schrader ===
Hank Schrader is a DEA agent married to Marie Schrader, the sister of Skyler White.


== See also ==
Better Call Saul
"""

WALTER_EXTRACT = """\
Walter Hartwell White Sr. is the protagonist of Breaking Bad, portrayed by Bryan Cranston.


== Concept and creation ==
The character was created by Vince Gilligan.


== Character biography ==


=== Background ===
Walter White co-founded Gray Matter Technologies before leaving the company and becoming a \
teacher in Albuquerque.


=== Season 1 ===
After his cancer diagnosis, Walter rides along on a DEA raid with Hank Schrader and sees \
Jesse Pinkman escaping. He pressures Jesse into becoming his partner.


== Critical reception ==
The character was widely praised.
"""

JESSE_EXTRACT = """\
Jesse Bruce Pinkman is a character in Breaking Bad, portrayed by Aaron Paul. He is a small-time \
methamphetamine cook and dealer who becomes the partner of his former teacher Walter White.
"""

# title -> (pageid, wikidata item, extract)
PAGES: dict[str, tuple[int, str, str]] = {
    "Breaking Bad": (1001, "Q1079", MAIN_EXTRACT),
    "List of characters in the Breaking Bad franchise": (1002, "Q900002", CHARACTERS_EXTRACT),
    "Walter White (Breaking Bad)": (1003, "Q23554", WALTER_EXTRACT),
    "Jesse Pinkman": (1004, "Q900004", JESSE_EXTRACT),
    "Hank Williams": (1005, "Q900005", "Hank Williams was an American singer and songwriter."),
    "The Matrix": (2001, "Q83495", "The Matrix is a 1999 science fiction film about Neo."),
}
# Wikipedia search query -> top hit. Anything else finds nothing.
WIKI_SEARCH = {
    "List of Breaking Bad characters": "List of characters in the Breaking Bad franchise",
    "Walter White Breaking Bad": "Walter White (Breaking Bad)",
    "Jesse Pinkman Breaking Bad": "Jesse Pinkman",
    "Walter White Jr. Breaking Bad": "Walter White (Breaking Bad)",
    "Hank Schrader Breaking Bad": "Hank Williams",
    "Breaking Bad 2008 TV series": "Breaking Bad",
}


def wiki_page(title: str | None) -> dict[str, Any]:
    """A MediaWiki ``action=query`` response for one page (or for no result)."""
    if title is None:
        return {"batchcomplete": True}
    if title not in PAGES:
        return {"batchcomplete": True, "query": {"pages": [{"title": title, "missing": True}]}}
    pageid, item, extract = PAGES[title]
    page = {
        "pageid": pageid,
        "ns": 0,
        "title": title,
        "extract": extract,
        "canonicalurl": f"https://en.wikipedia.org/wiki/{title.replace(' ', '_')}",
        "pageprops": {"wikibase_item": item},
        "revisions": [{"revid": 500000 + pageid, "timestamp": "2026-09-27T20:29:13Z"}],
    }
    return {"batchcomplete": True, "query": {"pages": [page]}}


class FakeWeb:
    """Routes requests by host and records them. ``claude`` is a queue of replies."""

    def __init__(
        self,
        claude: list[httpx.Response | dict[str, Any] | str] | None = None,
        **overrides: Handler,
    ) -> None:
        self.requests: list[httpx.Request] = []
        self.claude = list(claude or [])
        self.routes: dict[str, Handler] = {
            TMDB_HOST: overrides.get("tmdb", self._tmdb),
            WIKIDATA_HOST: overrides.get("wikidata", self._wikidata),
            WIKIPEDIA_HOST: overrides.get("wikipedia", self._wikipedia),
            CLAUDE_HOST: overrides.get("claude_handler", self._claude),
        }

    def __call__(self, request: httpx.Request) -> httpx.Response:
        self.requests.append(request)
        return self.routes[request.url.host](request)

    def client(self) -> httpx.AsyncClient:
        return httpx.AsyncClient(transport=httpx.MockTransport(self))

    def sent_to(self, host: str) -> list[httpx.Request]:
        return [request for request in self.requests if request.url.host == host]

    def llm(self, settings: Settings | None = None) -> ClaudeStructured:
        """The Claude provider, with its SDK answered by this fake."""
        return ClaudeStructured(settings or fake_settings(), http_client=sdk_client(self))

    def claude_bodies(self) -> list[dict[str, Any]]:
        return [json.loads(request.content) for request in self.sent_to(CLAUDE_HOST)]

    @staticmethod
    def _tmdb(request: httpx.Request) -> httpx.Response:
        path = request.url.path
        if path == "/3/search/multi":
            name = SEARCHES.get(request.url.params["query"].lower())
            empty = {"page": 1, "results": [], "total_pages": 0, "total_results": 0}
            return httpx.Response(200, json=fixture(name) if name else empty)
        if path in DETAILS:
            return httpx.Response(200, json=fixture(DETAILS[path]))
        missing = {"success": False, "status_code": 34, "status_message": "Not found."}
        return httpx.Response(404, json=missing)

    @staticmethod
    def _wikidata(request: httpx.Request) -> httpx.Response:
        item = request.url.params["ids"]
        sitelinks = (
            {"enwiki": {"site": "enwiki", "title": SITELINKS[item]}} if item in SITELINKS else {}
        )
        return httpx.Response(200, json={"entities": {item: {"id": item, "sitelinks": sitelinks}}})

    @staticmethod
    def _wikipedia(request: httpx.Request) -> httpx.Response:
        params = request.url.params
        if "gsrsearch" in params:
            return httpx.Response(200, json=wiki_page(WIKI_SEARCH.get(params["gsrsearch"])))
        return httpx.Response(200, json=wiki_page(params["titles"]))

    def _claude(self, request: httpx.Request) -> httpx.Response:
        if not self.claude:
            raise AssertionError("unexpected Claude call")
        reply = self.claude.pop(0)
        if isinstance(reply, httpx.Response):
            return reply
        return message_stream(reply if isinstance(reply, str) else json.dumps(reply))


def valid_bible(draft: dict[str, Any] | None = None) -> WorldBible:
    """The WorldBible assembled from ``draft`` (default: the valid one) and the saved packet."""
    model = WorldBibleDraft.model_validate(draft or valid_draft())
    packet = saved_packet()
    notes = ground(model, packet)
    return assemble(
        model,
        packet,
        model="claude-opus-5-5",
        attempts=1,
        usage={"total_tokens": 2000},
        notes=notes,
        compiled_at=NOW,
    )
