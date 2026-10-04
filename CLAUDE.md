# Rift

Enter any story. Change what happens next.

A 24-hour hackathon project: the player enters a story universe and their
actions change what happens next. Optimize every decision for hackathon speed
and a reliable demo.

## Architecture

```
Unreal  = rendering + deterministic gameplay           (game/, UE5, C++ + Blueprints)
Rust    = realtime authoritative state + orchestration (backend/, Tokio + Axum)
Python  = pre-game canon/data pipeline                 (canon/, uv + Pydantic + httpx)

Future providers: Claude, TiDB, TMDB, ElevenLabs, World Labs
```

Unreal <-> Rust: one persistent WebSocket, JSON protocol V1 (`docs/PROTOCOL.md`).
Python runs before gameplay and writes to `cache/`; it is not on the gameplay path.

## Non-negotiable rules

1. Unreal owns rendering and deterministic gameplay.
2. Rust owns authoritative realtime `GameSession` state.
3. Python handles pre-game canon/data processing.
4. Gameplay never blocks on an LLM or any slow external service.
5. AI work runs asynchronously.
6. LLMs may *propose* actions; they never directly mutate authoritative state.
7. Rust validates every state mutation.
8. Unreal executes only validated, structured events (`world_event`).
9. Unreal and Rust communicate through a persistent WebSocket.
10. Active gameplay state lives in memory.
11. Database operations stay outside the latency-critical gameplay path.
12. External services sit behind provider interfaces.
13. API keys stay server-side (Rust/Python `.env` only).
14. Never expose secrets to Unreal — not in config, assets, Blueprints or messages.
15. Protocol is JSON V1. Unknown versions are rejected.
16. No Redis, Kafka, Kubernetes, protobuf, Docker orchestration or other
    infrastructure without a concrete, demonstrated need.
17. Optimize for a 24-hour hackathon.

## Working rules

- Never block gameplay on AI.
- Never let an LLM directly mutate authoritative state.
- Never expose secrets to Unreal.
- Do not change shared contracts casually: `backend/src/protocol.rs`,
  `backend/src/action.rs`, `shared/schemas/v1/`, `docs/PROTOCOL.md` change together,
  with tests, and Unreal must be updated in the same change.
- Keep providers behind interfaces (`canon/src/rift_canon/providers/`).
- Preserve working code. Prefer small, additive changes.
- Compile/test after meaningful changes (`make lint test smoke`).
- Do not add frameworks or dependencies without a concrete need.
- Prefer hackathon-speed simplicity over abstraction.
- Never claim an integration works unless it was actually tested.
- Never print, log or commit secret values. `.env` is git-ignored.

## Layout

```
backend/   Rust server: protocol.rs, action.rs, session.rs, ws.rs, config.rs
canon/     Python (uv): config.py, doctor.py, providers/
game/      Unreal project (manual setup: game/README.md)
shared/    JSON Schemas for protocol V1 + future contract notes
cache/     generated universes/worlds/audio (git-ignored contents)
scripts/   doctor.sh, smoke.sh, smoke_ws.py
docs/      ARCHITECTURE, PROTOCOL, SETUP, MILESTONES
```

## Commands

```sh
make doctor            # toolchain + Unreal/Xcode + provider status
make canon-doctor      # provider configuration only (no network)
make canon-doctor-live # + free read-only connectivity checks
make backend           # run backend on 127.0.0.1:3000
make test              # cargo test + pytest
make smoke             # start backend, run WebSocket smoke client, stop
make lint              # fmt --check, clippy -D warnings, ruff
make format            # cargo fmt + ruff format
```

Rust is installed via rustup in `~/.cargo/bin`; the Makefile adds it to PATH.

## Current phase

Phase 0 (setup) — see `docs/MILESTONES.md`. Do not start a phase until asked.
