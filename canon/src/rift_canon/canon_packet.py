"""CanonPacket V1 — retrieved evidence, independent of any LLM.

Retrieval produces a CanonPacket; the compiler consumes one. A saved packet can
be compiled again without making a single retrieval request.
"""

from __future__ import annotations

import hashlib
from enum import StrEnum
from typing import Any, Literal, Self

from pydantic import AwareDatetime, BaseModel, ConfigDict, Field, model_validator

from rift_canon.text import slugify

UNIVERSE_ID_PATTERN = r"^[a-z0-9_]{1,96}$"
SOURCE_ID_PATTERN = r"^[a-z0-9][a-z0-9:_.-]{0,95}$"


class Classification(StrEnum):
    """Where a statement comes from. Only retrieved evidence is ``canon``."""

    CANON = "canon"  # stated by a retrieved source
    INFERRED = "inferred"  # deduced from sources, not stated by them
    GENERATED = "generated"  # invented for the game


class MediaType(StrEnum):
    MOVIE = "movie"
    TV = "tv"


class SourceType(StrEnum):
    TMDB = "tmdb"
    WIKIPEDIA = "wikipedia"


class _Model(BaseModel):
    model_config = ConfigDict(extra="forbid")


class TitleCandidate(_Model):
    """One TMDB match, kept so a future UI can offer disambiguation."""

    tmdb_id: int
    media_type: MediaType
    title: str
    release_year: int | None = None
    overview: str = ""
    original_language: str | None = None
    popularity: float = 0.0
    vote_count: int = 0


class ResolvedUniverse(_Model):
    universe_id: str = Field(pattern=UNIVERSE_ID_PATTERN)
    title: str
    media_type: MediaType
    release_year: int | None
    tmdb_id: int
    overview: str
    original_language: str | None
    provider_metadata: dict[str, Any] = {}
    query: str
    match: Literal["exact", "partial", "fallback"]
    ambiguous: bool
    candidates: list[TitleCandidate] = []


def make_universe_id(title: str, media_type: MediaType, tmdb_id: int) -> str:
    """Stable, filesystem-safe id. TMDB ids are only unique per media type."""
    return f"{slugify(title) or 'universe'}_{media_type.value}_{tmdb_id}"


class CanonDocument(_Model):
    source_id: str = Field(pattern=SOURCE_ID_PATTERN)
    source_type: SourceType
    title: str
    source_url: str
    retrieved_at: AwareDatetime
    text: str = Field(min_length=1)
    metadata: dict[str, Any] = {}


class CanonFact(_Model):
    fact_id: str
    subject: str
    predicate: str
    object: str
    classification: Classification = Classification.CANON
    source_ids: list[str] = Field(min_length=1)
    confidence: float = Field(ge=0.0, le=1.0)


class SourceSummary(_Model):
    provider: SourceType
    endpoint: str
    license: str
    documents: int


class PacketProvenance(_Model):
    pipeline_version: str
    sources: list[SourceSummary]
    limits: dict[str, int] = {}
    notes: list[str] = []


class CanonPacket(_Model):
    schema_version: Literal[1] = 1
    universe: ResolvedUniverse
    documents: list[CanonDocument] = Field(min_length=1)
    facts: list[CanonFact] = []
    retrieved_at: AwareDatetime
    provenance: PacketProvenance

    @model_validator(mode="after")
    def _check_references(self) -> Self:
        source_ids = [doc.source_id for doc in self.documents]
        duplicates = sorted({s for s in source_ids if source_ids.count(s) > 1})
        if duplicates:
            raise ValueError(f"duplicate document source_id: {duplicates}")
        fact_ids = [fact.fact_id for fact in self.facts]
        if len(set(fact_ids)) != len(fact_ids):
            raise ValueError("duplicate fact_id")
        known = set(source_ids)
        for fact in self.facts:
            unknown = [s for s in fact.source_ids if s not in known]
            if unknown:
                raise ValueError(f"fact {fact.fact_id} cites unknown sources: {unknown}")
        return self

    def source_ids(self) -> list[str]:
        return [doc.source_id for doc in self.documents]

    def sha256(self) -> str:
        """Digest of the evidence a WorldBible was compiled from."""
        return hashlib.sha256(self.model_dump_json().encode("utf-8")).hexdigest()
