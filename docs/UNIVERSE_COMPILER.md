# Universe Compiler (Phase 1)

Turns a title into a validated, cached `WorldBible` for one playable slice.
Runs before gameplay, in Python (`canon/`). Nothing here is on the gameplay path.

```
title ─► TMDB resolution ─► canon retrieval ─► CanonPacket V1 ─► Gemini (structured) ─► WorldBible V1 ─► validation ─► cache/universes/
         resolve.py         acquire.py         canon_packet.py   compiler.py            world_bible.py                   cache.py
```

```sh
cd canon
uv run python -m rift_canon.compile "Breaking Bad"
```

```
Resolving universe...
Resolved: Breaking Bad (TV, 2008)

Retrieving canon...
Documents: 8
Facts: 42

Compiling WorldBible... (gemini-3.8-flash)
WorldBible validation: PASS (attempts: 1)
Classification: canon 24, inferred 6, generated 3
Provenance note: locations[2]: name '...' not found in the cited sources; reclassified canon -> inferred
Player role: Neighborhood Handyman
Starting location: Jesse's House
Opening conflict: A Knock at the Hallway

Written:
../cache/universes/breaking_bad_tv_1396.json
```

| Flag | Effect |
|---|---|
| `--no-cache` | Ignore cached entries; retrieve and compile again |
| `--from-packet PATH` | Compile a saved CanonPacket: no TMDB or Wikipedia request |
| `--output PATH` | Also write the WorldBible to `PATH` |
| `--json` | WorldBible JSON on stdout, progress on stderr |
| `--model ID` | Gemini model (default `GEMINI_MODEL`, else `gemini-3.8-flash`) |
| `--cache-dir DIR` | Cache directory (default `<repo>/cache/universes`) |

Exit codes: `0` success, `1` pipeline failure, `2` usage error or missing credentials.
Needs `TMDB_API_KEY` and `GEMINI_API_KEY` in `.env`. Wikipedia needs no key.

## The rule: evidence is canon, the LLM is not

```
source evidence ─► CanonPacket ─► Gemini ─► WorldBible
```

Every claim-bearing object carries `classification` and `source_refs`.

| `classification` | Meaning | `source_refs` |
|---|---|---|
| `canon` | Stated by a retrieved source | required, ≥ 1 |
| `inferred` | Deduced from sources, not stated by them | the documents it was deduced from |
| `generated` | Invented for the game | optional (what makes it plausible) |

How this is enforced, not just requested:

1. **Constrained output.** The response schema only allows `source_refs` values that are
   `source_id`s of the packet, and fixes `player_role` / `opening_conflict` to `generated`.
2. **Identity from code.** `universe_id`, title, media type, genres and all provenance come
   from the packet. The draft schema has no field through which the model could set them.
3. **Grounding** (`compiler.ground`). A `canon` character, location or faction whose name
   does not appear in the documents it cites, or any `canon` item citing nothing, is demoted
   to `inferred` and recorded in `provenance.validation_notes`. Nothing is ever promoted.
4. **Integrity** (`world_bible.integrity_errors`). `canon` needs sources; every `source_ref`
   must be in `provenance.sources`; every referenced id must exist; ids are unique; the
   player role is `generated` and is not an existing character.

## Title resolution

`TMDB /search/multi`, movies and TV only. Ranking: a well-known exact title match, else
the most-voted title containing the query, else TMDB's first result. `ResolvedUniverse`
keeps `match` (`exact|partial|fallback`), `ambiguous` and the top five `candidates` for a
future disambiguation UI.

`universe_id = <ascii_slug>_<media_type>_<tmdb_id>`, e.g. `breaking_bad_tv_1396`. It matches
`^[a-z0-9_]+$`, so it is safe as a file name.

## Canon acquisition

Bounded: one TMDB details call and at most 9 Wikipedia/Wikidata requests, sequential and
paced (Wikimedia throttles bursts; a refused request is retried once).

| Document | How it is found |
|---|---|
| `tmdb:<type>:<id>` | TMDB details: overview, tagline, structured facts |
| main article | Wikidata item from TMDB → English Wikipedia sitelink (search as fallback) |
| characters list | search, accepted only if the title is a characters page of this universe |
| ≤ 5 character pages | top-billed characters; accepted only if the page title matches the name and the page mentions the universe |

Wikipedia text is normalized: real-world sections (production, reception, references, …)
are dropped, whitespace is tidied, and each document is capped (12k chars, 8k for
character pages). Misses and rejections go to `provenance.notes`; only a missing main
article is fatal.

`CanonFact`s are deterministic, taken from TMDB structured data (dates, genres, creators,
cast). They are `canon`, cite the TMDB document, and need no LLM.

## Contracts

Pydantic models are the source of truth. JSON Schemas are generated into
`shared/schemas/universe/v1/` by `uv run python -m rift_canon.schema_export`; a test fails
if they drift.

**CanonPacket V1** (`canon_packet.py`): `schema_version`, `universe`, `documents[]`
(`source_id, source_type, title, source_url, retrieved_at, text, metadata`), `facts[]`
(`fact_id, subject, predicate, object, classification, source_ids, confidence`),
`retrieved_at`, `provenance`.

**WorldBible V1** (`world_bible.py`): `schema_version`, `universe` (`universe_id, title,
media_type, release_year, tmdb_id, genres, era, setting`), `world_rules`, `locations`,
`characters` (`id, name, role, personality_traits, goals, known_facts, status,
relationships`), `factions`, `relationships`, `important_conflicts`, `player_role`,
`starting_location`, `opening_conflict`, `timeline_context` (`canon_cutoff`), `provenance`
(`compiler_version, compiled_at, retrieved_at, canon_packet_sha256, sources, llm,
classification_counts, validation_notes`).

Entity ids match `^[a-z0-9_]{1,64}$`, a subset of protocol V1 identifiers, so a location or
character id can later be used as a `target` or `current_location` unchanged. `player` is
reserved.

## Gemini

`POST /v1beta/interactions` (Interactions API) over httpx, key in the `x-goog-api-key`
header, `store: false`, schema-constrained JSON via `response_format`. 180 s read timeout.

- At most **two** generation calls per compile: the first attempt, plus exactly one repair
  call if the output fails JSON, schema or integrity validation. The repair request shows
  the model its rejected output and the error list.
- Provider errors (timeout, HTTP errors, truncated generation) are reported and not retried.
- If both attempts fail, nothing is cached and the rejected output is saved under
  `cache/universes/_failed/` for diagnosis.

Observed live (2026-10-03, free tier): nested arrays with `minItems`/`maxItems` make the
constrained decoder reject the schema with HTTP 400, so size limits are sent in field
descriptions and enforced by Pydantic. `gemini-3.8-flash` sometimes answers 503 "high
demand"; rerun the command — the canon packet is already cached. Free tier allows 5
requests per minute.

## Cache

```
cache/universes/<universe_id>.json         WorldBible V1
cache/universes/<universe_id>.canon.json   CanonPacket V1 it was compiled from
cache/universes/_failed/                   rejected LLM output (never read back)
```

An entry is a hit only if it parses, passes full model validation (including integrity)
and belongs to the requested universe. Invalid or stale (other compiler version) entries
are announced and rebuilt, never used. Writes are atomic and read back through validation
before the run reports success. Cache contents are git-ignored.

## Tests

`cd canon && uv run pytest -q`. All offline: TMDB, MediaWiki and Gemini are replaced by
`tests/support.FakeWeb` (httpx `MockTransport`) and fixtures in `tests/fixtures/`.
No test spends API credits.

## Not in Phase 1

Director, NPC agents and memory, TiDB, missions, ElevenLabs, World Labs, Unreal, any
backend or protocol change. Rust does not load WorldBibles yet.
