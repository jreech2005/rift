# Shared contracts

`v1/` — JSON Schemas for protocol V1. They document the wire format; the Rust
types in `backend/src/` are the source of truth and are covered by tests.
Change both together, never casually (see `docs/PROTOCOL.md`).

`universe/v1/` — JSON Schemas for `CanonPacket` V1 and `WorldBible` V1 (Phase 1
universe compiler). Not wire messages. The Pydantic models in
`canon/src/rift_canon/` are the source of truth; regenerate with
`cd canon && uv run python -m rift_canon.schema_export` (a test fails on drift).
See `docs/UNIVERSE_COMPILER.md`.

`future/` — placeholder notes for contracts that later phases will define.
