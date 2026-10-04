# Architecture

```
        Unreal Engine 5                 rendering, input, deterministic gameplay
             │                          no secrets, no direct AI calls
          WebSocket  (JSON protocol V1, persistent)
             │
             ▼
      Rust Orchestrator (Axum/Tokio)    authoritative GameSession state
             │                          validates every mutation
       in-memory state                  emits structured world_events
             ┆
             ┆  (future) async tasks: Director AI, NPC agents → proposals only
             ┆  async, optional: TiDB (NPC memory) · Tiger Data (recent gameplay telemetry)
             ┆
      Python Pipeline (uv)              pre-game: canon retrieval, WorldBible
             │                          writes cache/universes, cache/worlds, cache/audio
       future providers                 TMDB · Gemini · TiDB · ElevenLabs · World Labs
```

## Ownership

| Component | Owns | Never does |
|---|---|---|
| Unreal (`game/`) | Rendering, movement, physics, animation, presentation; executes validated `world_event`s | Holds API keys; calls LLMs; decides authoritative story state |
| Rust (`backend/`) | `GameSession` (in memory), protocol validation, action validation, event generation, future orchestration of async AI | Blocks the WebSocket loop on slow I/O; lets AI output mutate state unvalidated |
| Python (`canon/`) | Pre-game data: title resolution, canon retrieval, WorldBible compilation (Phase 1+), provider access | Runs in the gameplay loop |
| `shared/schemas/` | Wire contract documentation | — |

## Flow (Phase 0)

1. Unreal connects to `/ws`, sends `hello`, receives `hello_ack`.
2. `create_session` → Rust inserts a `GameSession` into `SessionStore`
   (`Arc<RwLock<HashMap<Uuid, GameSession>>>`) → `session_created`.
3. Player does something → `player_action` → Rust validates (version, session,
   action type, identifiers) → applies deterministic rule → `world_event`.
4. Unreal executes the event.

## Flow (Phase 2)

In a session that runs a world, step 3 continues: the accepted action goes
through NPC perception and the narrative layer, and a meaningful one is put to
the Director once, off the read path. Its validated decision comes back as
further `world_event`s. See [RUNTIME.md](RUNTIME.md).

## Flow (Phase 3 voice)

A `start_dialogue` decision becomes a `dialogue_started` event. When ElevenLabs
is configured the line is synthesized, cached in memory and offered as
`audio_url` (`GET /audio/<id>`); otherwise, and on any failure, the event
carries the text alone. See [VOICE.md](VOICE.md).

## Phase 3: who does what

```
Unreal player action
      |
      v
authoritative Runtime (Rust)
      +--> NPC perception / memory ......... TiDB behind MemoryStore
      +--> narrative causality
      +--> telemetry event (never blocks) .. Tiger Data behind TelemetrySink
      v
DirectorContext, built after the consequences
      +--> recent PlayerTelemetry (300 ms bounded read)
      v
Director failover, inside DIRECTOR_BUDGET_MS
      Gemini primary -> one bounded retry -> Gemini fallback model
      -> optional Anthropic -> deterministic FallbackDirector
      v
validated DirectorDecision (all of it or none of it)
      +--> world state
      +--> optional ElevenLabs synthesis -> audio_url
      v
world_event(s) --> Unreal presentation (HUD, NPCs, subtitles, voice)
```

| Service | Role | When it is missing or fails |
|---|---|---|
| TiDB | persistent semantic memory of NPCs and the world, behind `MemoryStore` ([NPC_MEMORY.md](NPC_MEMORY.md)) | in-memory store; recall is lost, state is not |
| Tiger Data | recent behavioral telemetry: what the player has been doing in the last minutes, behind `TelemetrySink` / `TelemetryReader` ([TELEMETRY.md](TELEMETRY.md)) | in-memory telemetry; a slow read means the Director decides without it |
| Gemini / Anthropic / `FallbackDirector` | adaptive reasoning with resilient failover ([DIRECTOR.md](DIRECTOR.md)) | the next leg, then the deterministic rules |
| ElevenLabs | generated NPC speech ([VOICE.md](VOICE.md)) | the line goes out as text |
| Unreal | physical presentation of validated events ([UNREAL_HOSPITAL_DEMO.md](UNREAL_HOSPITAL_DEMO.md)) | — |

TiDB and Tiger Data are deliberately separate: TiDB remembers what was said and learned, for
as long as the world exists; Tiger Data summarises recent behavior for the next decision. Neither
is on the path of a player action, and none of these services can block gameplay.

## AI path

```
player_action ─► Rust validates ─► world_event (immediate, deterministic)
                      │
                      └─► spawn async task ─► LLM proposes DirectorAction
                                                   │
                           Rust validates proposal ◄┘
                                   │
                                   └─► pushes world_event via connection writer
```

The gameplay response never waits for the LLM. Each WebSocket connection already
has a reader loop and a separate writer task joined by a channel, so
server-originated events can be pushed later without changing the read path.

## Key decisions

- **In-memory state, no DB on the hot path.** Sessions vanish on restart; fine for
  a hackathon demo. TiDB is for pre-game canon data, accessed by Python.
- **Two optional data layers, both off the hot path.** TiDB is persistent semantic memory
  ("what should the world remember?", [NPC_MEMORY.md](NPC_MEMORY.md)). Tiger Data is
  high-frequency recent behavioural telemetry ("what has the player been doing recently?",
  [TELEMETRY.md](TELEMETRY.md)). Each sits behind a trait with an in-memory default; the
  backend builds, tests and runs without either.
- **Std `RwLock`, short critical sections.** No `.await` while locked.
- **Deterministic events.** `event_id = UUIDv5(action_id)`; same state + action →
  same event. Duplicate `action_id`s are rejected.
- **Secrets.** Read only by Python (and later Rust) from the git-ignored `.env`.
  Python holds them as `SecretStr`; doctor and errors report status codes only.
- **No session ownership/auth yet.** Any connection may act on any session id it
  knows. Acceptable for a local demo; revisit if exposed beyond localhost.
- **Bind to 127.0.0.1 by default.**
