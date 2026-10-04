# Rift

Enter any story. Change what happens next.

Pick a story universe, step into it, and act — the world responds and the story
diverges based on what you do.

## Architecture

- **Unreal Engine 5** (`game/`) — rendering and deterministic gameplay
- **Rust** (`backend/`) — authoritative realtime game state over a persistent WebSocket (JSON protocol V1)
- **Python** (`canon/`) — pre-game canon/data pipeline
- Future providers: Claude, TMDB, TiDB, ElevenLabs, World Labs — server-side only

## Current phase

**Phase 0 — Setup.** Backend, protocol, Python pipeline scaffold and tooling are
done and tested. Unreal awaits manual install (`game/README.md`).

## Commands

```sh
make backend      # start backend on 127.0.0.1:3000
make test         # Rust + Python tests
make smoke        # end-to-end WebSocket smoke test
make doctor       # toolchain + provider status
```

## Docs

- [CLAUDE.md](CLAUDE.md) — rules for contributors and agents
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- [docs/PROTOCOL.md](docs/PROTOCOL.md)
- [docs/SETUP.md](docs/SETUP.md)
- [docs/MILESTONES.md](docs/MILESTONES.md)
- [game/README.md](game/README.md) — Unreal setup
