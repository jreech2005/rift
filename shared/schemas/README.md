# Shared contracts

`v1/` — JSON Schemas for protocol V1. They document the wire format; the Rust
types in `backend/src/` are the source of truth and are covered by tests.
Change both together, never casually (see `docs/PROTOCOL.md`).

`future/` — placeholder notes for contracts that later phases will define.
