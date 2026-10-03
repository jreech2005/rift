# Local Setup (macOS, Apple Silicon)

## Prerequisites

| Tool | Version used | Install |
|---|---|---|
| Git | 2.53 | Xcode CLT or Homebrew |
| Rust (rustup, rustfmt, clippy) | 1.99 stable | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh -s -- -y --profile default` |
| uv | 0.11 | `curl -LsSf https://astral.sh/uv/install.sh \| sh` |
| Python | 3.12 (uv-managed, pinned in `canon/.python-version`) | `uv python install 3.12` |
| Xcode + Unreal Engine 5 | — | manual, see `game/README.md` |

Rust was installed without editing shell profiles. To use `cargo` directly in
your shell, add this to `~/.zshrc` (the Makefile does not need it):

```sh
. "$HOME/.cargo/env"
```

## First run

```sh
git clone <repo> rift && cd rift
cp .env.example .env          # fill in keys you have; never commit .env
make doctor                   # toolchain + provider status
make test                     # Rust + Python tests
make smoke                    # end-to-end WebSocket protocol check
```

`make smoke` builds the backend, starts it on `127.0.0.1:3000`, runs
`scripts/smoke_ws.py` and stops the server. It refuses to run if the port is
already in use.

## Day-to-day

```sh
make backend                  # run server (Ctrl-C to stop)
curl http://127.0.0.1:3000/health
uv run scripts/smoke_ws.py    # against an already-running server
make lint                     # fmt --check, clippy -D warnings, ruff
make format
```

Python (from `canon/`):

```sh
uv sync
uv run pytest
uv run python -m rift_canon.doctor          # no network
uv run python -m rift_canon.doctor --live   # free read-only checks
```

## Environment variables

See `.env.example`. Provider keys are read only by server-side code.

| Variable | Used by | Notes |
|---|---|---|
| `GEMINI_API_KEY` | canon | live check: list models (free) |
| `TMDB_API_KEY` | canon | v3 key or v4 read token; live check: `/3/configuration` |
| `ELEVENLABS_API_KEY` | canon | live check: list models (no credits) |
| `WORLD_LABS_API_KEY` | canon | configuration only — no live check (avoids credits) |
| `TIDB_*` | canon | live check: TCP reachability only, no auth |
| `BACKEND_HOST`, `BACKEND_PORT` | backend | default `127.0.0.1:3000` |
| `RUST_LOG` | backend | e.g. `info`, `rift_backend=debug` |

Process environment overrides `.env`. `RIFT_ENV_FILE` points canon at another file.

## Troubleshooting

- `cargo: command not found` → `. "$HOME/.cargo/env"` or use `make`.
- `make smoke` says port in use → stop the other backend (`lsof -iTCP:3000`).
- Doctor `Environment WARN` → no `.env`; copy `.env.example`.
