# Milestones

## Phase 0 — Setup  ✅ complete (software)  ·  ⏳ Unreal project verification pending

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
- [x] Xcode 27.0 and Unreal Engine 5.8.3 installed (`/Users/Shared/UE_5.8`); `game/Rift/Rift.uproject`
      exists — detected by `make doctor` (2026-10-03, filesystem check only)
- [ ] Unreal project opens, C++ compiles, First Person template runs, WebSockets module enabled
      — engine is installed; not yet verified by QA (owner: Unreal engineer)

## Phase 1 — Universe Compiler
Python: resolve a title (TMDB), retrieve canon, generate a `WorldBible` (Gemini),
cache it under `cache/universes/`. Optional TiDB persistence.

## Phase 2 — Director Engine
Rust: async Director that proposes `DirectorAction`s; Rust validates and turns
them into `world_event`s. Gameplay never waits on it.

## Phase 3 — Unreal Runtime Integration
Unreal C++ WebSocket client speaking protocol V1; executes `world_event`s;
Blueprint hookups for presentation.

### Phase 3 — Tiger Data behavioural telemetry (branch `phase3-tiger`)
Gameplay events go to a `TelemetrySink` (in-memory, or a Tiger Data hypertable with
`--features tiger`); a bounded `PlayerTelemetry` summary of the last five minutes enters
`DirectorContext`, and one `FallbackDirector` rule acts on it. Offline acceptance test:
`backend/tests/telemetry_divergence.rs`. Live Tiger Data check: not yet run (no credentials).
See [TELEMETRY.md](TELEMETRY.md).

## Phase 4 — NPC Agents & Memory
`CharacterState`, per-NPC memory and async agent proposals, validated by Rust.

## Phase 5 — Adaptive Story / Narrative Causality
`Mission`s, world flags with consequences, timeline divergence.

## Phase 6 — World Generation & Voice
World Labs environments, ElevenLabs speech, cached assets in `cache/worlds/` and `cache/audio/`.

## Phase 7 — Demo Hardening & Prize Polish
Reliability, fallbacks for every external service, demo script, polish.
