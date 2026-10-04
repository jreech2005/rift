# rift-canon

Pre-game canon/data pipeline for Rift.

```sh
uv run python -m rift_canon.doctor          # config check, no network
uv run python -m rift_canon.doctor --live   # + free read-only connectivity checks
uv run python -m rift_canon.compile "Breaking Bad"   # title -> cached WorldBible
uv run pytest -q
```

## Universe compiler (Phase 1)

`rift_canon.compile` resolves a title with TMDB, retrieves a bounded set of canon documents
(TMDB + Wikipedia), builds a `CanonPacket`, has Gemini compile it into a schema-constrained
`WorldBible`, validates it and caches it under `../cache/universes/`.

Needs `TMDB_API_KEY` and `GEMINI_API_KEY` in the repo-root `.env`. `GEMINI_MODEL` is optional.
Flags: `--no-cache`, `--from-packet PATH`, `--output PATH`, `--json`, `--model ID`, `--cache-dir DIR`.

| Module | Role |
|---|---|
| `resolve.py` | title -> `ResolvedUniverse` (TMDB) |
| `acquire.py` | `ResolvedUniverse` -> `CanonPacket` (TMDB details + Wikipedia) |
| `canon_packet.py` | CanonPacket V1 models |
| `compiler.py` | `CanonPacket` -> `WorldBible` (Gemini, grounding, one repair retry) |
| `world_bible.py` | WorldBible V1 models and integrity rules |
| `cache.py` | validated cache read/write |
| `compile.py` | CLI |
| `schema_export.py` | regenerate `shared/schemas/universe/v1/*.schema.json` |
| `providers/` | `tmdb_catalog`, `wikipedia`, `gemini_structured` behind the `Provider` interface |

Design, contracts and provenance rules: [`docs/UNIVERSE_COMPILER.md`](../docs/UNIVERSE_COMPILER.md).

Tests are fully offline (`tests/support.py` fakes every external service).
