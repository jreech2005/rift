"""Rift universe compiler: a title becomes a validated, cached WorldBible.

    uv run python -m rift_canon.compile "Breaking Bad"
    uv run python -m rift_canon.compile "Breaking Bad" --no-cache
    uv run python -m rift_canon.compile --from-packet ../cache/universes/<id>.canon.json

Title -> TMDB resolution -> canon retrieval (CanonPacket) -> Gemini structured
compilation -> validation -> cache/universes/. Never prints secret values.
"""

from __future__ import annotations

import argparse
import asyncio
import logging
import os
import sys
from collections.abc import Callable, Sequence
from pathlib import Path

import httpx

from rift_canon import cache
from rift_canon.acquire import acquire_canon
from rift_canon.canon_packet import CanonPacket, MediaType, ResolvedUniverse
from rift_canon.compiler import compile_world_bible
from rift_canon.config import Settings, load_settings
from rift_canon.errors import (
    CompilationError,
    ConfigurationError,
    ProviderError,
    RiftCanonError,
)
from rift_canon.providers.base import make_client
from rift_canon.providers.gemini_structured import GeminiStructured
from rift_canon.providers.tmdb_catalog import TMDBCatalog
from rift_canon.providers.wikipedia import WikipediaProvider
from rift_canon.resolve import resolve_title
from rift_canon.world_bible import WorldBible

MAX_SHOWN_ERRORS = 10
# Provider failures that usually clear on their own (seen live: Gemini 503 "high demand").
TRANSIENT_KINDS = frozenset({"timeout", "rate_limited", "unavailable"})
Say = Callable[[str], None]


def _shown(path: Path) -> str:
    try:
        return os.path.relpath(path)
    except ValueError:  # different drive on Windows
        return str(path)


def _describe(universe: ResolvedUniverse) -> str:
    kind = "TV" if universe.media_type is MediaType.TV else "Movie"
    return f"{universe.title} ({kind}, {universe.release_year or 'year unknown'})"


def _summary(bible: WorldBible, say: Say) -> None:
    counts = bible.provenance.classification_counts
    start = next(
        (loc.name for loc in bible.locations if loc.id == bible.starting_location.location_id), "?"
    )
    say("Classification: " + ", ".join(f"{kind} {count}" for kind, count in counts.items()))
    for note in bible.provenance.validation_notes:
        say(f"Provenance note: {note}")
    say(f"Player role: {bible.player_role.title}")
    say(f"Starting location: {start}")
    say(f"Opening conflict: {bible.opening_conflict.title}")


async def _canon_packet(
    args: argparse.Namespace,
    settings: Settings,
    client: httpx.AsyncClient,
    universe: ResolvedUniverse,
    say: Say,
) -> CanonPacket:
    path = cache.canon_packet_path(args.cache_dir, universe.universe_id)
    if not args.no_cache:
        cached = cache.load_canon_packet(path, universe.universe_id)
        if cached.value is not None and cached.status == "hit":
            say(f"Using cached canon packet: {_shown(path)}")
            return cached.value
        if cached.status != "miss":
            say(f"Cached canon packet {cached.status.upper()} ({cached.reason}) - retrieving again")
    packet = await acquire_canon(
        client, TMDBCatalog(settings), WikipediaProvider(settings), universe
    )
    cache.write_json(path, packet)
    return packet


async def _pipeline(
    args: argparse.Namespace, settings: Settings, client: httpx.AsyncClient, say: Say
) -> WorldBible:
    gemini = GeminiStructured(settings)
    model = args.model or settings.gemini_model
    packet: CanonPacket | None = None

    if args.from_packet is not None:
        loaded = cache.load_canon_packet(args.from_packet)
        if loaded.value is None or loaded.status not in ("hit", "stale"):
            reason = loaded.reason or "file not found"
            raise RiftCanonError(f"cannot use canon packet {args.from_packet}: {reason}")
        packet = loaded.value
        universe = packet.universe
        say(f"Canon packet: {_shown(args.from_packet)}")
        say(f"Universe: {_describe(universe)}")
    else:
        if settings.tmdb_api_key is None:
            raise ConfigurationError("TMDB_API_KEY is not set")
        say("Resolving universe...")
        universe = await resolve_title(client, TMDBCatalog(settings), args.title)
        say(f"Resolved: {_describe(universe)}")
        if universe.ambiguous:
            others = ", ".join(
                f"{c.title} ({c.release_year or '?'})" for c in universe.candidates[1:4]
            )
            say(f"Note: {universe.match} match, other candidates: {others}")

    bible_path = cache.world_bible_path(args.cache_dir, universe.universe_id)
    if args.from_packet is None and not args.no_cache:
        cached = cache.load_world_bible(bible_path, universe.universe_id)
        if cached.value is not None and cached.status == "hit":
            say("")
            say("Cache: HIT (validated)")
            _summary(cached.value, say)
            _written(args, cached.value, bible_path, say, label="Cached at:")
            return cached.value
        if cached.status != "miss":
            say(f"Cache: {cached.status.upper()} ({cached.reason}) - recompiling")

    # Fail before any retrieval work if the compile step cannot run.
    if settings.gemini_api_key is None:
        raise ConfigurationError("GEMINI_API_KEY is not set")

    say("")
    if packet is None:
        say("Retrieving canon...")
        packet = await _canon_packet(args, settings, client, universe, say)
    say(f"Documents: {len(packet.documents)}")
    say(f"Facts: {len(packet.facts)}")
    for note in packet.provenance.notes:
        say(f"Retrieval note: {note}")

    say("")
    say(f"Compiling WorldBible... ({model})")
    try:
        bible = await compile_world_bible(client, gemini, packet, model=model)
    except CompilationError as exc:
        say("WorldBible validation: FAIL")
        saved = cache.save_failed_outputs(args.cache_dir, universe.universe_id, exc.raw_outputs)
        exc.add_note("Rejected model output saved to: " + ", ".join(_shown(p) for p in saved))
        raise

    cache.write_json(bible_path, bible)
    # Trust the cache entry only after reading it back through validation.
    verified = cache.load_world_bible(bible_path, universe.universe_id)
    if verified.value is None or verified.status != "hit":
        raise RiftCanonError(f"cache entry failed verification: {verified.reason}")
    say(f"WorldBible validation: PASS (attempts: {bible.provenance.llm.attempts})")
    _summary(verified.value, say)
    _written(args, verified.value, bible_path, say)
    return verified.value


def _written(
    args: argparse.Namespace, bible: WorldBible, path: Path, say: Say, label: str = "Written:"
) -> None:
    say("")
    say(label)
    say(_shown(path))
    if args.output is not None:
        say(_shown(cache.write_json(args.output, bible)))


async def _run(
    args: argparse.Namespace, settings: Settings, client: httpx.AsyncClient | None
) -> int:
    def say(line: str) -> None:
        # With --json, stdout carries only the WorldBible.
        print(line, file=sys.stderr if args.json else sys.stdout)

    try:
        if client is None:
            async with make_client() as owned:
                bible = await _pipeline(args, settings, owned, say)
        else:
            bible = await _pipeline(args, settings, client, say)
    except ConfigurationError as exc:
        print(f"BLOCKED: {exc}. Add it to .env (see .env.example).", file=sys.stderr)
        return 2
    except CompilationError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        for error in exc.errors[:MAX_SHOWN_ERRORS]:
            print(f"  - {error}", file=sys.stderr)
        for note in getattr(exc, "__notes__", []):
            print(note, file=sys.stderr)
        return 1
    except ProviderError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        if exc.kind in TRANSIENT_KINDS:
            print("Usually temporary: run the same command again.", file=sys.stderr)
        return 1
    except RiftCanonError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1
    if args.json:
        print(bible.model_dump_json(indent=2))
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="rift_canon.compile", description="Compile a story universe into a WorldBible."
    )
    parser.add_argument("title", nargs="?", help='universe title, e.g. "Breaking Bad"')
    parser.add_argument(
        "--from-packet",
        type=Path,
        metavar="PATH",
        help="compile a saved CanonPacket instead of resolving and retrieving",
    )
    parser.add_argument(
        "--no-cache", action="store_true", help="ignore cached entries and rebuild them"
    )
    parser.add_argument(
        "--output", type=Path, metavar="PATH", help="also write the WorldBible to this file"
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="print the WorldBible JSON on stdout (progress on stderr)",
    )
    parser.add_argument("--model", help="Gemini model id (default: GEMINI_MODEL or built-in)")
    parser.add_argument(
        "--cache-dir",
        type=Path,
        default=cache.DEFAULT_CACHE_DIR,
        metavar="DIR",
        help="cache directory (default: <repo>/cache/universes)",
    )
    return parser


def main(
    argv: Sequence[str] | None = None,
    *,
    client: httpx.AsyncClient | None = None,
    settings: Settings | None = None,
) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.from_packet is None and not (args.title or "").strip():
        parser.error("give a title or --from-packet PATH")
    # httpx logs request URLs at INFO, and a TMDB v3 key travels in the query string.
    logging.getLogger("httpx").setLevel(logging.WARNING)
    return asyncio.run(_run(args, settings or load_settings(), client))


if __name__ == "__main__":
    sys.exit(main())
