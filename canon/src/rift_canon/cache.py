"""On-disk cache under ``cache/universes/``.

    <universe_id>.json         WorldBible V1
    <universe_id>.canon.json   CanonPacket V1 it was compiled from
    _failed/                   rejected LLM output, for diagnosis only

An entry is a hit only if it parses, passes full model validation and belongs
to the universe asked for. Anything else is reported, never used.
"""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

from pydantic import BaseModel, ValidationError

from rift_canon.acquire import PIPELINE_VERSION
from rift_canon.canon_packet import CanonPacket
from rift_canon.compiler import COMPILER_VERSION
from rift_canon.config import REPO_ROOT
from rift_canon.world_bible import WorldBible

DEFAULT_CACHE_DIR = REPO_ROOT / "cache" / "universes"
FAILED_DIR_NAME = "_failed"


@dataclass(frozen=True)
class CacheLookup[T]:
    status: Literal["hit", "miss", "invalid", "stale"]
    value: T | None = None
    reason: str = ""


def world_bible_path(cache_dir: Path, universe_id: str) -> Path:
    return cache_dir / f"{universe_id}.json"


def canon_packet_path(cache_dir: Path, universe_id: str) -> Path:
    return cache_dir / f"{universe_id}.canon.json"


def write_json(path: Path, model: BaseModel) -> Path:
    """Write atomically, so a crash never leaves a half-written entry."""
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp")
    temporary.write_text(model.model_dump_json(indent=2) + "\n", encoding="utf-8")
    os.replace(temporary, path)
    return path


def _load[M: BaseModel](path: Path, model: type[M]) -> CacheLookup[M]:
    if not path.is_file():
        return CacheLookup("miss")
    try:
        return CacheLookup("hit", model.model_validate_json(path.read_bytes()))
    except OSError as exc:
        return CacheLookup("invalid", reason=f"unreadable ({type(exc).__name__})")
    except ValidationError as exc:
        first = exc.errors()[0]
        where = ".".join(str(part) for part in first["loc"]) or model.__name__
        return CacheLookup(
            "invalid", reason=f"{exc.error_count()} validation error(s); {where}: {first['msg']}"
        )


def load_world_bible(path: Path, universe_id: str) -> CacheLookup[WorldBible]:
    lookup = _load(path, WorldBible)
    bible = lookup.value
    if bible is None:
        return lookup
    if bible.universe.universe_id != universe_id:
        found = bible.universe.universe_id
        return CacheLookup("invalid", reason=f"entry is for {found!r}, not {universe_id!r}")
    if bible.provenance.compiler_version != COMPILER_VERSION:
        version = bible.provenance.compiler_version
        reason = f"compiled by version {version}, now {COMPILER_VERSION}"
        return CacheLookup("stale", bible, reason)
    return lookup


def load_canon_packet(path: Path, universe_id: str | None = None) -> CacheLookup[CanonPacket]:
    lookup = _load(path, CanonPacket)
    packet = lookup.value
    if packet is None:
        return lookup
    if universe_id is not None and packet.universe.universe_id != universe_id:
        found = packet.universe.universe_id
        return CacheLookup("invalid", reason=f"entry is for {found!r}, not {universe_id!r}")
    if packet.provenance.pipeline_version != PIPELINE_VERSION:
        version = packet.provenance.pipeline_version
        reason = f"retrieved by version {version}, now {PIPELINE_VERSION}"
        return CacheLookup("stale", packet, reason)
    return lookup


def save_failed_outputs(cache_dir: Path, universe_id: str, raw_outputs: list[str]) -> list[Path]:
    """Keep rejected LLM output so a failure can be diagnosed without another call."""
    directory = cache_dir / FAILED_DIR_NAME
    directory.mkdir(parents=True, exist_ok=True)
    paths = []
    for attempt, text in enumerate(raw_outputs, start=1):
        path = directory / f"{universe_id}.attempt{attempt}.txt"
        path.write_text(text, encoding="utf-8")
        paths.append(path)
    return paths
