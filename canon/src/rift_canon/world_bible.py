"""WorldBible V1 — the compiled, validated description of one playable slice.

``WorldBibleDraft`` is what the LLM proposes. ``WorldBible`` is that draft plus
the identity and provenance that code assigns from the CanonPacket. Every
claim-bearing object carries ``classification`` and ``source_refs`` so canon,
inferred and generated content stay distinguishable.
"""

from __future__ import annotations

import re
from collections.abc import Iterator
from typing import Annotated, Any, Literal, Self

from pydantic import (
    AwareDatetime,
    BaseModel,
    BeforeValidator,
    ConfigDict,
    Field,
    StringConstraints,
    model_validator,
)

from rift_canon.canon_packet import UNIVERSE_ID_PATTERN, Classification, MediaType, SourceType
from rift_canon.text import fold

PLAYER_ID = "player"
# A subset of protocol V1 identifiers, so ids can later be used as a
# `target` or `current_location` without translation.
ENTITY_ID_PATTERN = r"^[a-z0-9_]{1,64}$"


def _normalize_id(value: object) -> object:
    if isinstance(value, str):
        return re.sub(r"[^a-z0-9]+", "_", value.strip().lower()).strip("_")[:64].strip("_")
    return value


EntityId = Annotated[
    str, BeforeValidator(_normalize_id), StringConstraints(pattern=ENTITY_ID_PATTERN)
]
Text = Annotated[str, StringConstraints(strip_whitespace=True, min_length=1)]
ClassificationField = Annotated[
    Classification,
    Field(
        description=(
            "canon: stated in the cited evidence. inferred: your deduction from the "
            "evidence. generated: invented for the game."
        )
    ),
]
SourceRefs = Annotated[
    list[str],
    Field(
        description=(
            "source_id of each evidence document that supports this item. "
            "Must not be empty when classification is canon."
        )
    ),
]


class _Model(BaseModel):
    model_config = ConfigDict(extra="forbid")


class Claim(_Model):
    """One statement with its provenance."""

    text: Text
    classification: ClassificationField
    source_refs: SourceRefs


class Location(_Model):
    id: EntityId
    name: Text
    description: Text
    importance: Literal["primary", "secondary", "background"]
    classification: ClassificationField
    source_refs: SourceRefs


class CharacterDraft(_Model):
    id: EntityId
    name: Text
    role: Annotated[Text, Field(description="Who they are in the story, in one line.")]
    personality_traits: list[Text] = Field(min_length=1, max_length=6)
    goals: list[Claim] = Field(max_length=4)
    known_facts: list[Claim] = Field(min_length=1, max_length=8)
    status: Annotated[Text, Field(description="Their situation at the canon cutoff.")]
    classification: ClassificationField
    source_refs: SourceRefs


class CharacterRelationship(_Model):
    """A relationship seen from one character; derived from ``relationships``."""

    other_id: EntityId
    direction: Literal["outgoing", "incoming"]
    kind: Text
    description: Text
    classification: ClassificationField
    source_refs: SourceRefs


class Character(CharacterDraft):
    relationships: list[CharacterRelationship] = []


class Faction(_Model):
    id: EntityId
    name: Text
    description: Text
    member_ids: list[EntityId] = Field(description="ids of characters in this faction.")
    classification: ClassificationField
    source_refs: SourceRefs


class Relationship(_Model):
    from_id: EntityId = Field(description="Character id, faction id, or 'player'.")
    to_id: EntityId = Field(description="Character id, faction id, or 'player'.")
    kind: Annotated[Text, Field(description="Short label, e.g. 'brother-in-law', 'rival'.")]
    description: Text
    classification: ClassificationField
    source_refs: SourceRefs


class Conflict(_Model):
    id: EntityId
    name: Text
    description: Text
    party_ids: list[EntityId] = Field(
        min_length=1, description="Character ids, faction ids, or 'player'."
    )
    stakes: Text
    classification: ClassificationField
    source_refs: SourceRefs


class PlayerConnection(_Model):
    character_id: EntityId
    description: Annotated[Text, Field(description="How the player role knows this character.")]


class PlayerRole(_Model):
    """An original role for the player. Always ``generated``."""

    title: Annotated[
        Text, Field(description="The role, e.g. 'Forensic accountant'. Not a canon character.")
    ]
    description: Text
    reason_for_presence: Annotated[
        Text, Field(description="Why this person is plausibly in the starting location.")
    ]
    capabilities: list[Text] = Field(min_length=1, max_length=6)
    connections: list[PlayerConnection] = Field(min_length=1, max_length=4)
    classification: Literal["generated"]
    source_refs: Annotated[
        list[str], Field(description="Evidence documents that make this role plausible.")
    ]


class StartingLocation(_Model):
    location_id: EntityId = Field(description="id of one of the locations.")
    rationale: Text


class OpeningConflict(_Model):
    """One conflict sized for a 10-20 minute episode. Always ``generated``."""

    title: Text
    summary: Text
    location_id: EntityId = Field(description="The starting location, or one near it.")
    involved_character_ids: list[EntityId] = Field(min_length=1, max_length=5)
    immediate_goal: Annotated[
        Text, Field(description="What the player must do in the next minutes.")
    ]
    decision_options: list[Text] = Field(
        min_length=2, max_length=4, description="Distinct choices open to the player right away."
    )
    stakes: Text
    estimated_minutes: int = Field(ge=10, le=20)
    classification: Literal["generated"]
    source_refs: Annotated[
        list[str], Field(description="Evidence documents this conflict is consistent with.")
    ]


class TimelineContext(_Model):
    canon_cutoff: str | None = Field(
        description=(
            "Point in the canon timeline where the slice begins, e.g. 'Season 1, after "
            "the pilot'. null if the evidence does not support one."
        )
    )
    summary: Annotated[Text, Field(description="What has already happened by that point.")]
    classification: ClassificationField
    source_refs: SourceRefs


class WorldBibleDraft(_Model):
    """The LLM's proposal. Identity and provenance are added by code."""

    era: Claim
    setting: Claim
    timeline_context: TimelineContext
    world_rules: list[Claim] = Field(min_length=2, max_length=8)
    locations: list[Location] = Field(min_length=2, max_length=6)
    characters: list[CharacterDraft] = Field(min_length=3, max_length=8)
    factions: list[Faction] = Field(max_length=4)
    relationships: list[Relationship] = Field(max_length=12)
    important_conflicts: list[Conflict] = Field(min_length=1, max_length=4)
    player_role: PlayerRole
    starting_location: StartingLocation
    opening_conflict: OpeningConflict


class UniverseInfo(_Model):
    universe_id: str = Field(pattern=UNIVERSE_ID_PATTERN)
    title: str
    media_type: MediaType
    release_year: int | None
    tmdb_id: int
    genres: list[str]
    era: Claim
    setting: Claim


class SourceRecord(_Model):
    """An evidence document a ``source_ref`` can point to."""

    source_id: str
    source_type: SourceType
    title: str
    source_url: str
    retrieved_at: AwareDatetime


class LLMRecord(_Model):
    provider: str
    api: str
    model: str
    attempts: int
    usage: dict[str, int] = {}


class Provenance(_Model):
    compiler_version: str
    compiled_at: AwareDatetime
    retrieved_at: AwareDatetime
    canon_packet_sha256: str
    sources: list[SourceRecord] = Field(min_length=1)
    llm: LLMRecord
    classification_counts: dict[str, int] = {}
    validation_notes: list[str] = []


class WorldBible(_Model):
    schema_version: Literal[1] = 1
    universe: UniverseInfo
    world_rules: list[Claim]
    locations: list[Location]
    characters: list[Character]
    factions: list[Faction]
    relationships: list[Relationship]
    important_conflicts: list[Conflict]
    player_role: PlayerRole
    starting_location: StartingLocation
    opening_conflict: OpeningConflict
    timeline_context: TimelineContext
    provenance: Provenance

    @model_validator(mode="after")
    def _check_integrity(self) -> Self:
        errors = integrity_errors(self)
        if errors:
            raise ValueError("; ".join(errors))
        return self


def iter_attributed(node: object, path: str = "") -> Iterator[tuple[str, Any]]:
    """Every nested object that carries ``classification`` and ``source_refs``."""
    if isinstance(node, BaseModel):
        fields = type(node).model_fields
        if "classification" in fields and "source_refs" in fields:
            yield path, node
        for name in fields:
            yield from iter_attributed(getattr(node, name), f"{path}.{name}" if path else name)
    elif isinstance(node, list):
        for index, item in enumerate(node):
            yield from iter_attributed(item, f"{path}[{index}]")


def integrity_errors(bible: WorldBible) -> list[str]:
    """Everything that makes a WorldBible internally inconsistent.

    Self-contained on purpose: a cached file can be checked without the packet.
    """
    errors: list[str] = []
    location_ids = [item.id for item in bible.locations]
    character_ids = [item.id for item in bible.characters]
    faction_ids = [item.id for item in bible.factions]
    all_ids = location_ids + character_ids + faction_ids
    all_ids += [item.id for item in bible.important_conflicts]
    duplicates = sorted({i for i in all_ids if all_ids.count(i) > 1})
    if duplicates:
        errors.append(f"duplicate ids: {duplicates}")
    if PLAYER_ID in all_ids:
        errors.append(f"the id {PLAYER_ID!r} is reserved for the player")

    def require(path: str, value: str | list[str], allowed: set[str], kind: str) -> None:
        for item in [value] if isinstance(value, str) else value:
            if item not in allowed:
                errors.append(f"{path}: {item!r} is not a defined {kind} id")

    locations, characters = set(location_ids), set(character_ids)
    actors = characters | set(faction_ids) | {PLAYER_ID}
    opening = bible.opening_conflict
    require(
        "starting_location.location_id", bible.starting_location.location_id, locations, "location"
    )
    require("opening_conflict.location_id", opening.location_id, locations, "location")
    require(
        "opening_conflict.involved_character_ids",
        opening.involved_character_ids,
        characters,
        "character",
    )
    for index, connection in enumerate(bible.player_role.connections):
        path = f"player_role.connections[{index}].character_id"
        require(path, connection.character_id, characters, "character")
    for index, faction in enumerate(bible.factions):
        require(f"factions[{index}].member_ids", faction.member_ids, characters, "character")
    for index, relationship in enumerate(bible.relationships):
        for end in ("from_id", "to_id"):
            path = f"relationships[{index}].{end}"
            require(path, getattr(relationship, end), actors, "character or faction")
    for index, conflict in enumerate(bible.important_conflicts):
        path = f"important_conflicts[{index}].party_ids"
        require(path, conflict.party_ids, actors, "character or faction")

    if fold(bible.player_role.title) in {fold(item.name) for item in bible.characters}:
        errors.append("player_role.title: the player must be an original role, not a character")
    if not any(item.classification == Classification.CANON for item in bible.characters):
        errors.append("characters: at least one canon character is required")

    sources = {source.source_id for source in bible.provenance.sources}
    for path, node in iter_attributed(bible):
        unknown = [ref for ref in node.source_refs if ref not in sources]
        if unknown:
            errors.append(f"{path}.source_refs: unknown source ids {unknown}")
        if node.classification == Classification.CANON and not node.source_refs:
            errors.append(f"{path}: classification 'canon' requires at least one source_ref")
    return errors
