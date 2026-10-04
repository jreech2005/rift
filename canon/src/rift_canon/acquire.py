"""Canon acquisition: a ``ResolvedUniverse`` becomes a ``CanonPacket``.

Bounded on purpose: one TMDB details call plus at most ``MAX_WIKI_REQUESTS``
Wikipedia requests, enough canon for one focused playable slice. No LLM is
involved, so retrieval is testable on its own.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Any

import httpx

from rift_canon.canon_packet import (
    CanonDocument,
    CanonFact,
    CanonPacket,
    MediaType,
    PacketProvenance,
    ResolvedUniverse,
    SourceSummary,
    SourceType,
)
from rift_canon.errors import CanonAcquisitionError, ProviderError
from rift_canon.providers import tmdb, wikipedia
from rift_canon.providers.tmdb_catalog import TMDBCatalog
from rift_canon.providers.wikipedia import WikiPage, WikipediaProvider
from rift_canon.text import fold, tokens

PIPELINE_VERSION = "1.0.0"
MAX_CHARACTER_PAGES = 5
MAX_CAST_FACTS = 15
MAIN_DOC_CHARS = 12_000
CHARACTER_DOC_CHARS = 8_000
# Wikidata sitelink + main article (+ search fallback) + characters list +
# character pages. A rate-limited request is retried once on top of this.
MAX_WIKI_REQUESTS = 4 + MAX_CHARACTER_PAGES

TMDB_LICENSE = "TMDB API terms of use (attribution required; not endorsed by TMDB)"

# Sections about the real world (production, reception, references), not the
# story. Matched as substrings of the folded heading; subsections go with them.
_DROPPED_SECTIONS = (
    "accolades",
    "adaptations",
    "awards",
    "bibliography",
    "box office",
    "broadcast",
    "casting",
    "citations",
    "concept and creation",
    "conception",
    "cultural impact",
    "development",
    "distribution",
    "external links",
    "filming",
    "franchise",
    "further reading",
    "home media",
    "lawsuit",
    "legacy",
    "marketing",
    "merchandis",
    "music",
    "notes",
    "other media",
    "popular culture",
    "production",
    "ratings",
    "reception",
    "references",
    "related media",
    "release",
    "see also",
    "sequel",
    "soundtrack",
    "sources",
    "spin off",
    "streaming",
    "viewership",
)
_HEADING = re.compile(r"^(={2,6})\s*(.+?)\s*\1$")
_NOT_A_CHARACTER = frozenset({"", "self", "narrator", "additional voices"})


@dataclass(frozen=True)
class CastMember:
    character: str
    actor: str
    episodes: int | None


def _dict(value: object) -> dict[str, Any]:
    return value if isinstance(value, dict) else {}


def _dicts(value: object) -> list[dict[str, Any]]:
    return [item for item in value if isinstance(item, dict)] if isinstance(value, list) else []


def _text(value: object) -> str:
    return value.strip() if isinstance(value, str) else ""


def _int(value: object, default: int = 0) -> int:
    return value if isinstance(value, int) and not isinstance(value, bool) else default


def normalize_wiki_text(extract: str, max_chars: int) -> tuple[str, dict[str, Any]]:
    """Keep in-universe sections, tidy whitespace, cap the length.

    Returns the text and what was done to it, for the document's metadata.
    """
    kept: list[str] = []
    dropped: list[str] = []
    skip_level: int | None = None
    for raw in extract.splitlines():
        line = re.sub(r"\s+", " ", raw).strip()
        heading = _HEADING.match(line)
        if heading is None:
            if skip_level is None:
                kept.append(line)
            continue
        level, title = len(heading.group(1)), heading.group(2)
        if skip_level is not None and level > skip_level:
            continue  # subsection of a dropped section
        folded = fold(title)
        if any(marker in folded for marker in _DROPPED_SECTIONS):
            skip_level = level
            dropped.append(title)
            continue
        skip_level = None
        kept += ["", f"{'#' * level} {title}"]
    text = re.sub(r"\n{3,}", "\n\n", "\n".join(kept)).strip()

    original_chars = len(text)
    truncated = original_chars > max_chars
    if truncated:
        cut = text.rfind("\n\n", 0, max_chars)
        text = text[: cut if cut > max_chars // 2 else max_chars].rstrip()
    stats = {"original_chars": original_chars, "truncated": truncated, "sections_dropped": dropped}
    return text, stats


def _clean_character(value: object) -> str:
    """``"Thomas Anderson / Neo (voice)"`` -> ``"Thomas Anderson"``."""
    name = re.sub(r"\s*\([^)]*\)", "", _text(value).split(" / ")[0]).strip()
    return "" if fold(name) in _NOT_A_CHARACTER else name


def billed_cast(details: dict[str, Any], media_type: MediaType) -> list[CastMember]:
    """Characters in billing order (TV: by episode count first), deduplicated."""
    rows: list[tuple[int, int, CastMember]] = []
    if media_type is MediaType.TV:
        for index, member in enumerate(_dicts(_dict(details.get("aggregate_credits")).get("cast"))):
            roles = _dicts(member.get("roles"))
            role = max(roles, key=lambda r: _int(r.get("episode_count")), default={})
            episodes = _int(member.get("total_episode_count"))
            cast = CastMember(
                _clean_character(role.get("character")), _text(member.get("name")), episodes or None
            )
            rows.append((-episodes, _int(member.get("order"), index), cast))
    else:
        for index, member in enumerate(_dicts(_dict(details.get("credits")).get("cast"))):
            cast = CastMember(
                _clean_character(member.get("character")), _text(member.get("name")), None
            )
            rows.append((0, _int(member.get("order"), index), cast))

    seen: set[str] = set()
    result: list[CastMember] = []
    for _, _, cast in sorted(rows, key=lambda row: row[:2]):
        key = fold(cast.character)
        if cast.character and cast.actor and key not in seen:
            seen.add(key)
            result.append(cast)
    return result


def _tmdb_document(
    universe: ResolvedUniverse,
    details: dict[str, Any],
    cast: list[CastMember],
    retrieved_at: datetime,
) -> tuple[CanonDocument, list[CanonFact]]:
    """TMDB's structured data as one evidence document plus source-backed facts."""
    source_id = f"tmdb:{universe.media_type.value}:{universe.tmdb_id}"
    title = universe.title
    is_tv = universe.media_type is MediaType.TV
    facts: list[CanonFact] = []

    def add(subject: str, predicate: str, value: object) -> None:
        text = "" if value is None else str(value).strip()
        if text:
            facts.append(
                CanonFact(
                    fact_id=f"fact:{len(facts) + 1:03d}",
                    subject=subject,
                    predicate=predicate,
                    object=text,
                    source_ids=[source_id],
                    confidence=1.0,
                )
            )

    add(title, "media_type", "TV series" if is_tv else "film")
    if is_tv:
        add(title, "first_air_date", details.get("first_air_date"))
        add(title, "last_air_date", details.get("last_air_date"))
        add(title, "number_of_seasons", details.get("number_of_seasons"))
        add(title, "number_of_episodes", details.get("number_of_episodes"))
        for person in _dicts(details.get("created_by")):
            add(title, "created_by", person.get("name"))
        for network in _dicts(details.get("networks")):
            add(title, "network", network.get("name"))
    else:
        add(title, "release_date", details.get("release_date"))
        add(title, "runtime_minutes", details.get("runtime"))
        for member in _dicts(_dict(details.get("credits")).get("crew")):
            if member.get("job") == "Director":
                add(title, "directed_by", member.get("name"))
    add(title, "status", details.get("status"))
    for genre in _dicts(details.get("genres")):
        add(title, "genre", genre.get("name"))
    countries = details.get("origin_country")
    for country in countries if isinstance(countries, list) else []:
        add(title, "origin_country", country)
    add(title, "original_language", details.get("original_language"))
    for member in cast[:MAX_CAST_FACTS]:
        add(member.character, "portrayed_by", member.actor)
        add(member.character, "episode_count", member.episodes)

    year = f", {universe.release_year}" if universe.release_year else ""
    lines = [f"{title} ({'TV series' if is_tv else 'film'}{year})"]
    if tagline := _text(details.get("tagline")):
        lines.append(f"Tagline: {tagline}")
    if overview := _text(details.get("overview")) or universe.overview:
        lines.append(f"Overview: {overview}")
    lines += ["", "Structured facts (subject | predicate | object):"]
    lines += [f"- {fact.subject} | {fact.predicate} | {fact.object}" for fact in facts]

    external = _dict(details.get("external_ids"))
    document = CanonDocument(
        source_id=source_id,
        source_type=SourceType.TMDB,
        title=title,
        source_url=f"https://www.themoviedb.org/{universe.media_type.value}/{universe.tmdb_id}",
        retrieved_at=retrieved_at,
        text="\n".join(lines),
        metadata={
            "role": "metadata",
            "tmdb_id": universe.tmdb_id,
            "wikidata_id": external.get("wikidata_id"),
            "imdb_id": external.get("imdb_id") or details.get("imdb_id"),
            "license": TMDB_LICENSE,
        },
    )
    return document, facts


def _wiki_document(
    page: WikiPage, role: str, max_chars: int, retrieved_at: datetime
) -> CanonDocument | None:
    text, stats = normalize_wiki_text(page.extract, max_chars)
    if not text:
        return None
    return CanonDocument(
        source_id=f"wikipedia:en:{page.pageid}",
        source_type=SourceType.WIKIPEDIA,
        title=page.title,
        source_url=page.url,
        retrieved_at=retrieved_at,
        text=text,
        metadata={
            "role": role,
            "pageid": page.pageid,
            "revid": page.revid,
            "rev_timestamp": page.rev_timestamp,
            "wikibase_item": page.wikibase_item,
            "license": wikipedia.LICENSE,
            **stats,
        },
    )


def _base_title(page_title: str) -> str:
    """``"Walter White (Breaking Bad)"`` -> ``"Walter White"``."""
    return re.sub(r"\s*\([^)]*\)\s*$", "", page_title)


def _mentions(page: WikiPage, universe: ResolvedUniverse) -> bool:
    return fold(universe.title) in fold(page.extract)


def _is_character_page(page: WikiPage, character: str, universe: ResolvedUniverse) -> bool:
    wanted = tokens(character)
    shared = wanted & tokens(_base_title(page.title))
    return bool(wanted) and len(shared) >= min(2, len(wanted)) and _mentions(page, universe)


def _is_characters_list(page: WikiPage, universe: ResolvedUniverse) -> bool:
    title_tokens = tokens(page.title)
    return "characters" in title_tokens and tokens(universe.title) <= title_tokens


async def _main_article(
    client: httpx.AsyncClient,
    wiki: WikipediaProvider,
    universe: ResolvedUniverse,
    wikidata_id: object,
    notes: list[str],
) -> WikiPage | None:
    """The universe's own article: exact via Wikidata, else a checked search hit."""
    if isinstance(wikidata_id, str) and wikidata_id:
        title = await wiki.sitelink_title(client, wikidata_id)
        page = await wiki.fetch_page(client, title) if title else None
        if page is not None and not page.disambiguation:
            return page
        notes.append(f"no English Wikipedia article linked from Wikidata item {wikidata_id}")
    kind = "TV series" if universe.media_type is MediaType.TV else "film"
    year = f" {universe.release_year}" if universe.release_year else ""
    page = await wiki.search_page(client, f"{universe.title}{year} {kind}")
    if page is None or page.disambiguation or not tokens(universe.title) <= tokens(page.title):
        return None
    notes.append(f"main article matched by search, not by Wikidata id: {page.title!r}")
    return page


async def acquire_canon(
    client: httpx.AsyncClient,
    tmdb_catalog: TMDBCatalog,
    wiki: WikipediaProvider,
    universe: ResolvedUniverse,
    *,
    now: datetime | None = None,
) -> CanonPacket:
    retrieved_at = now or datetime.now(UTC)
    notes: list[str] = []

    details = await tmdb_catalog.details(client, universe.media_type, universe.tmdb_id)
    cast = billed_cast(details, universe.media_type)
    tmdb_document, facts = _tmdb_document(universe, details, cast, retrieved_at)
    documents = [tmdb_document]

    wikidata_id = _dict(details.get("external_ids")).get("wikidata_id")
    main = await _main_article(client, wiki, universe, wikidata_id, notes)
    main_document = _wiki_document(main, "main", MAIN_DOC_CHARS, retrieved_at) if main else None
    if main is None or main_document is None:
        raise CanonAcquisitionError(
            f"no Wikipedia article found for {universe.title!r}; not enough canon to compile"
        )
    documents.append(main_document)
    seen = {main.pageid}

    # Supporting pages are best effort: a miss is recorded, never fatal.
    wanted = [("characters", f"List of {universe.title} characters", "", MAIN_DOC_CHARS)]
    wanted += [
        ("character", f"{member.character} {universe.title}", member.character, CHARACTER_DOC_CHARS)
        for member in cast[:MAX_CHARACTER_PAGES]
    ]
    for role, query, character, max_chars in wanted:
        try:
            page = await wiki.search_page(client, query)
        except ProviderError as exc:
            notes.append(f"stopped retrieving supporting pages at {query!r}: {exc}")
            break
        if page is None or page.disambiguation:
            notes.append(f"no page found for {query!r}")
            continue
        if page.pageid in seen:
            notes.append(f"{query!r} led to an already included page: {page.title!r}")
            continue
        relevant = (
            _is_character_page(page, character, universe)
            if character
            else _is_characters_list(page, universe)
        )
        if not relevant:
            notes.append(f"rejected unrelated page {page.title!r} for {query!r}")
        elif document := _wiki_document(page, role, max_chars, retrieved_at):
            seen.add(page.pageid)
            documents.append(document)

    wiki_documents = len(documents) - 1
    return CanonPacket(
        universe=universe,
        documents=documents,
        facts=facts,
        retrieved_at=retrieved_at,
        provenance=PacketProvenance(
            pipeline_version=PIPELINE_VERSION,
            sources=[
                SourceSummary(
                    provider=SourceType.TMDB,
                    endpoint=tmdb.BASE_URL,
                    license=TMDB_LICENSE,
                    documents=1,
                ),
                SourceSummary(
                    provider=SourceType.WIKIPEDIA,
                    endpoint=wikipedia.API_URL,
                    license=wikipedia.LICENSE,
                    documents=wiki_documents,
                ),
            ],
            limits={
                "max_wiki_requests": MAX_WIKI_REQUESTS,
                "max_character_pages": MAX_CHARACTER_PAGES,
                "main_doc_chars": MAIN_DOC_CHARS,
                "character_doc_chars": CHARACTER_DOC_CHARS,
            },
            notes=notes,
        ),
    )
