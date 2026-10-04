"""Title resolution: a user-typed title becomes a ``ResolvedUniverse`` via TMDB.

Selection is automatic for Phase 1, but the ranked candidates are kept so a
future UI can offer disambiguation.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

import httpx
from pydantic import BaseModel, ConfigDict, ValidationError

from rift_canon.canon_packet import MediaType, ResolvedUniverse, TitleCandidate, make_universe_id
from rift_canon.errors import ProviderError, TitleNotFoundError
from rift_canon.providers.tmdb_catalog import TMDBCatalog
from rift_canon.text import fold

# An exact title match needs this many votes to beat a far better-known partial
# match (e.g. an unreleased series named exactly like a famous franchise).
MIN_EXACT_VOTES = 50
MAX_CANDIDATES = 5


class _SearchItem(BaseModel):
    """The subset of a TMDB ``search/multi`` result we rely on."""

    model_config = ConfigDict(extra="ignore")

    id: int
    media_type: MediaType
    title: str | None = None
    name: str | None = None
    original_title: str | None = None
    original_name: str | None = None
    release_date: str | None = None
    first_air_date: str | None = None
    overview: str | None = None
    original_language: str | None = None
    popularity: float = 0.0
    vote_count: int = 0


@dataclass(frozen=True)
class _Entry:
    candidate: TitleCandidate
    names: frozenset[str]


def _normalize(title: str) -> str:
    return re.sub(r"^(the|a|an) ", "", fold(title))


def _year(date: str | None) -> int | None:
    return int(date[:4]) if date and date[:4].isdigit() else None


def _entry(item: _SearchItem) -> _Entry | None:
    title = item.title or item.name
    if not title:
        return None
    candidate = TitleCandidate(
        tmdb_id=item.id,
        media_type=item.media_type,
        title=title,
        release_year=_year(item.release_date or item.first_air_date),
        overview=item.overview or "",
        original_language=item.original_language,
        popularity=item.popularity,
        vote_count=item.vote_count,
    )
    originals = (title, item.original_title, item.original_name)
    return _Entry(candidate, frozenset(_normalize(name) for name in originals if name))


def _parse(results: list[dict]) -> list[_Entry]:
    entries: list[_Entry] = []
    rejected = 0
    for raw in results:
        if raw.get("media_type") not in (MediaType.MOVIE, MediaType.TV):
            continue  # people and anything else TMDB may add
        try:
            entry = _entry(_SearchItem.model_validate(raw))
        except ValidationError:
            entry = None
        if entry is None:
            rejected += 1
        else:
            entries.append(entry)
    if rejected and not entries:
        raise ProviderError("TMDB", "malformed", "search results did not have the expected shape")
    return entries


def _tier(entry: _Entry, query: str) -> int:
    if query in entry.names and entry.candidate.vote_count >= MIN_EXACT_VOTES:
        return 0
    if query and any(query in name for name in entry.names):
        return 1
    return 2


def _rank(entries: list[_Entry], query: str) -> list[_Entry]:
    """Well-known exact matches, then titles containing the query by votes,
    then whatever TMDB considered relevant, in its own order."""

    def key(indexed: tuple[int, _Entry]) -> tuple[int, int, int]:
        index, entry = indexed
        tier = _tier(entry, query)
        return tier, -entry.candidate.vote_count if tier < 2 else 0, index

    return [entry for _, entry in sorted(enumerate(entries), key=key)]


async def resolve_title(
    client: httpx.AsyncClient, tmdb: TMDBCatalog, title: str
) -> ResolvedUniverse:
    if not title.strip():
        raise TitleNotFoundError("the title is empty")
    entries = _parse(await tmdb.search(client, title.strip()))
    if not entries:
        raise TitleNotFoundError(f"no movie or TV series found for {title.strip()!r}")

    query = _normalize(title)
    ranked = _rank(entries, query)
    best = ranked[0]
    exact = query in best.names
    exact_count = sum(1 for entry in entries if query in entry.names)
    chosen = best.candidate
    return ResolvedUniverse(
        universe_id=make_universe_id(chosen.title, chosen.media_type, chosen.tmdb_id),
        title=chosen.title,
        media_type=chosen.media_type,
        release_year=chosen.release_year,
        tmdb_id=chosen.tmdb_id,
        overview=chosen.overview,
        original_language=chosen.original_language,
        provider_metadata={
            "provider": "tmdb",
            "popularity": chosen.popularity,
            "vote_count": chosen.vote_count,
            "candidates_considered": len(entries),
        },
        query=title.strip(),
        match="exact" if exact else ("partial" if _tier(best, query) == 1 else "fallback"),
        ambiguous=len(entries) > 1 and (not exact or exact_count > 1),
        candidates=[entry.candidate for entry in ranked[:MAX_CANDIDATES]],
    )
