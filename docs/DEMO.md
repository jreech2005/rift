# JUDGE DEMO

```sh
make demo-breaking-bad

make demo-matrix

make demo-harry-potter

make demo-stop
```

Each launch command stops the previous Rift backend, starts a new one with the right universe,
waits until it answers, opens Unreal if it is not open yet, and prints `RIFT READY`. Then press
**Play** in Unreal. Switching universe is just the next `make demo-...`; press Play again.

Any other cached title: `make demo TITLE="The Matrix"`.

---

## What one command does

`scripts/rift_demo.py` (standard library only, run by the system `python3`):

1. Loads the environment: `RIFT_ENV_FILE`, else `<repo>/.env`, else
   `~/rift-phase3-integration/.env`. The process environment wins over the file. Values are never
   printed; keys are reported as `PRESENT` / `MISSING`.
2. Resolves the title to a WorldBible and a scenario already on disk.
3. Frees port 3000, but only from a Rift backend (see [Process safety](#process-safety)).
4. Runs `cargo build` (a no-op when the binary is up to date) and starts
   `backend/target/debug/rift-backend` with `RIFT_WORLD_BIBLE` and `RIFT_SCENARIO` set.
   Output goes to `.demo/rift-backend.log`, the PID to `.demo/state.json`.
5. Waits for readiness (45 s at most): `GET /health`, then the real protocol over a WebSocket,
   `hello` → `hello_ack` and `create_session` → `session_created`. The session must be placed at
   the scenario's starting location, which only happens when the world loaded and the story
   started.
6. Opens `~/rift-phase3-integration/game/Rift/Rift.uproject` with `open`, unless an
   `UnrealEditor` process is already running. The same Unreal project serves every universe.

```
============================================
RIFT READY
Universe: The Matrix
Player Role: Assistant Hovercraft Engineer
Location: Nebuchadnezzar
Mission: Power Surge in the Simulation Deck
Backend: READY (ws://127.0.0.1:3000/ws, PID 26977)
Director: Gemini PRESENT
Voice: ElevenLabs PRESENT
Unreal: already open (press Play)
============================================
```

Backend only, for testing: add `NO_UNREAL=1` (`make demo-matrix NO_UNREAL=1`).

## Normal (cached) mode

`make demo TITLE="..."` and the three aliases use only files that already exist. No TMDB,
Wikipedia or Gemini canon request is made to launch. (During play the Director and voice still
call Gemini and ElevenLabs as usual; the launcher does not change the runtime.)

**Title resolution.** WorldBibles are recognised by content, not file name: every `*.json` whose
`universe.title` / `universe.universe_id` matches. Matching ignores case, punctuation,
apostrophes and a leading "The": exact title first, then a title starting with the words given
(`"Harry Potter"`), then one containing all of them. Two different universes matching equally is
an error that lists them.

Directories searched, in order:

| Order | Where | Notes |
|---|---|---|
| 1 | pinned demo files | Breaking Bad: `backend/tests/fixtures/director/world_bible_breaking_bad.json` + `backend/tests/fixtures/runtime/scenario_burner_phone.json`, the authored hospital demo ([UNREAL_HOSPITAL_DEMO.md](UNREAL_HOSPITAL_DEMO.md)) |
| 2 | `cache/universes/` | this checkout's cache (git-ignored) |
| 3 | `~/rift-phase3-integration/cache/universes/` | when it is a different checkout |
| 4 | `backend/tests/fixtures/scenario_builder/` | checked in: The Matrix and Harry Potter work on a fresh clone |

**Scenario.** The WorldBible's own pair (`X.world.json` → `X.scenario.json`) if it exists, else
any `*.scenario.json` in those directories with the same `plan.universe_id` whose characters and
locations exist in the WorldBible. If there is none, the deterministic Scenario Builder
([SCENARIO_BUILDER.md](SCENARIO_BUILDER.md), no model, no network) writes one next to the
WorldBible. If no WorldBible matches, the launch fails and says to use `demo-new`.

## New-title mode

```sh
make demo-new TITLE="Blade Runner"
```

Not the judge path. If the title is not compiled yet this runs the Universe Compiler
(title resolution → TMDB + Wikipedia retrieval → Gemini compilation), then the Scenario Builder,
then the normal launch. It needs `TMDB_API_KEY`, `GEMINI_API_KEY` and a network, spends API
credit and can take minutes. The result lands in `cache/universes/`, so afterwards
`make demo TITLE="Blade Runner"` is the fast cached path. A title that is already compiled is
launched from the cache without any request.

## Status, logs, stop, preflight

| Command | What it shows |
|---|---|
| `make demo-status` | backend running / stopped, PID, loaded universe, WorldBible and scenario paths |
| `make demo-logs` | the last 40 lines of `.demo/rift-backend.log` |
| `make demo-stop` | stops the launcher's backend; safe when nothing is running |
| `make demo-preflight` | offline check: cargo, uv, Unreal project, `.env` keys `PRESENT`/`MISSING`, launchable universes, port 3000. No API call, no credit |

Run `make demo-preflight` and one `make demo-matrix` before the judges arrive, so the backend
binary is already built.

## Process safety

Before starting, the launcher stops:

- the backend it started earlier (PID and process start time recorded in `.demo/state.json`; a
  recycled PID does not match and is never signalled), and
- any process listening on the port whose executable is named `rift-backend` (for example a
  forgotten `make backend`).

Anything else on port 3000 is left running and the launch fails with its PID and name. Stopping is
`SIGTERM`, then `SIGKILL` after 5 seconds.

## Failure recovery

| Symptom | Do |
|---|---|
| `RIFT FAILED TO START` | The last backend log lines are printed with it and the child is already stopped. Fix what they say, run the same command again. `make demo-logs` shows more. |
| `port 3000 is in use by PID n (...), which is not a Rift backend` | `kill n` (it is not ours), then run the command again. |
| `no compiled universe matches` | Check the spelling against the list printed, or `make demo-new TITLE="..."`. |
| Unreal shows the previous universe | Stop Play and press Play again: every Play session creates a new backend session. |
| Unreal did not open | `open ~/rift-phase3-integration/game/Rift/Rift.uproject`. The backend is already up. |
| Anything odd | `make demo-stop`, then the launch command again. Startup takes about a second. |
| No network at the venue | Launching still works. The Director falls back to its deterministic rules and dialogue is text only. |

## Configuration

All optional. Set in the shell or in `.env`.

| Variable | Default | |
|---|---|---|
| `RIFT_ENV_FILE` | `<repo>/.env`, then `~/rift-phase3-integration/.env` | the `.env` to load |
| `RIFT_DEMO_HOME` | `~/rift-phase3-integration` | checkout providing the fallback `.env`, cache and Unreal project |
| `RIFT_DEMO_CACHE` | see the table above | `:`-separated cache directories to search instead |
| `RIFT_UPROJECT` | `$RIFT_DEMO_HOME/game/Rift/Rift.uproject` | Unreal project to open |
| `RIFT_DEMO_TIMEOUT` | `45` | seconds to wait for readiness |
| `RIFT_DEMO_FEATURES` | none | cargo features for the backend build, e.g. `tidb` |
| `BACKEND_HOST`, `BACKEND_PORT` | `127.0.0.1`, `3000` | as for the backend |

## Tests

`canon/tests/test_rift_demo.py`, run by `make test`. Offline: the backend is a local stand-in
(`canon/tests/fake_backend.py`) that speaks `/health` and the `hello` / `create_session`
exchange, and Unreal is never started.
