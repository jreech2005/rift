# Milestones

## Phase 0 — Setup  ✅ complete (software)  ·  ⏳ Unreal pending manual install

Verified 2026-10-03 on macOS 15.2 / Apple M3.

- [x] Git configured, `.gitignore` correct, `.env.example` exists, no secrets committed
- [x] Repository structure, `CLAUDE.md`, setup docs
- [x] Rust toolchain (1.99) works; backend compiles
- [x] `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `cargo test` pass
- [x] `GET /health` works
- [x] WebSocket `/ws`: hello→hello_ack, ping→pong
- [x] `create_session` creates a real in-memory `GameSession`
- [x] `player_action` produces a deterministic `world_event`
- [x] CLI smoke client proves the full protocol path against a live server
- [x] Python uv project, tests, doctor, provider configuration detection
- [x] Unreal/Xcode status known; exact manual steps in `game/README.md`
- [ ] Unreal project opens, C++ compiles, First Person template runs, WebSockets module enabled
      — **blocked on manual Xcode + UE5 install**

## Phase 1 — Universe Compiler
Python: resolve a title (TMDB), retrieve canon, generate a `WorldBible` (Gemini),
cache it under `cache/universes/`. Optional TiDB persistence.

## Phase 2 — Director Engine
Rust: async Director that proposes `DirectorAction`s; Rust validates and turns
them into `world_event`s. Gameplay never waits on it.

## Phase 3 — Unreal Runtime Integration
Unreal C++ WebSocket client speaking protocol V1; executes `world_event`s;
Blueprint hookups for presentation.

## Phase 4 — NPC Agents & Memory
`CharacterState`, per-NPC memory and async agent proposals, validated by Rust.

## Phase 5 — Adaptive Story / Narrative Causality
`Mission`s, world flags with consequences, timeline divergence.

## Phase 6 — World Generation & Voice
World Labs environments, ElevenLabs speech, cached assets in `cache/worlds/` and `cache/audio/`.

## Phase 7 — Demo Hardening & Prize Polish
Reliability, fallbacks for every external service, demo script, polish.
