# Gameplay telemetry (Tiger Data)

Rift has two data layers behind the realtime backend, and they answer different questions.

| | TiDB | Tiger Data |
|---|---|---|
| Question | "What should the world remember?" | "What has the player been doing recently?" |
| Holds | NPC memories and state: persistent, semantic | Gameplay events: high-frequency, time-ordered |
| Read as | Individual memories, recalled by an NPC | Aggregates over the last few minutes |
| Docs | [NPC_MEMORY.md](NPC_MEMORY.md) | this file |

Tiger Data does not store memories and TiDB does not store telemetry.

Code: `backend/src/telemetry/` · SQL: `backend/migrations/tiger/` · wiring: [RUNTIME.md](RUNTIME.md)

```
runtime (sync, deterministic)                follow-up task (async, off the read path)
  TelemetrySink::record(event)                 TelemetryReader::recent(session)   ≤ 300 ms
        │  never blocks, never fails                 │
        ▼                                            ▼
  InMemoryTelemetry        (default)           PlayerTelemetry  (5 numbers)
  TigerTelemetry           (--features tiger)        │
    bounded queue ─► writer task ─► hypertable       ▼
                                               DirectorContext.telemetry ─► Director
```

Raw events never reach the Director or Gemini. Only the summary does.

## Modules

| File | Contents |
|---|---|
| `telemetry/mod.rs` | `TelemetryEvent`, `TelemetryEventKind`, the `TelemetrySink` and `TelemetryReader` traits, `summarize` |
| `telemetry/memory.rs` | `InMemoryTelemetry`: sink and reader over a bounded per-session ring (512 events) |
| `telemetry/tiger.rs` | `TigerConfig`, schema and queries (always compiled); `TigerTelemetry` (`--features tiger`) |

The runtime holds an `Arc<dyn TelemetrySink>` and an `Arc<dyn TelemetryReader>`
(`Runtime::with_telemetry`). It knows nothing about SQL.

## Events

One row per event: `timestamp, session_id, event_type, actor_id, target_id, location,
numeric_value, metadata`.

| `event_type` | Emitted when | Today |
|---|---|---|
| `player_action` | any accepted `player_action` (`metadata.action_type`) | emitted |
| `npc_interaction` | an accepted `speak` or `interact` aimed at an NPC that can perceive it (`metadata.disclosure`) | emitted |
| `location_entered` | an accepted `move` | emitted |
| `director_invoked` | the Director was asked (`metadata.trigger`, `result`, `provider`) | emitted |
| `narrative_replan` | the narrative layer or the Director asked for a replan (`metadata.source`) | emitted |
| `player_damaged`, `player_died`, `enemy_killed` | combat | **accepted, not emitted** |

The combat kinds are stored and aggregated like the others, but nothing produces them: the game
has no combat, damage or death yet, and the protocol has no message for them. They are there so
the scores below are real the day Unreal reports combat. Until then `combat_intensity` and
`recent_deaths` are `0` in a live game.

Telemetry carries structure only. What the player said (`content`) is never recorded. A rejected
action records nothing.

## Schema

`backend/migrations/tiger/001_telemetry_events.sql` (required):

```sql
CREATE TABLE IF NOT EXISTS rift_telemetry_events (
    ts            TIMESTAMPTZ      NOT NULL,
    session_id    UUID             NOT NULL,
    event_type    TEXT             NOT NULL,
    actor_id      TEXT,
    target_id     TEXT,
    location      TEXT,
    numeric_value DOUBLE PRECISION,
    metadata      JSONB            NOT NULL DEFAULT '{}'::jsonb
);
SELECT create_hypertable('rift_telemetry_events', 'ts', if_not_exists => TRUE);
CREATE INDEX IF NOT EXISTS rift_telemetry_events_session_ts
    ON rift_telemetry_events (session_id, ts DESC);
```

A hypertable partitioned on `ts`, with the index the recent-window query uses.

`backend/migrations/tiger/002_telemetry_minute.sql` (optional): a continuous aggregate,
`rift_telemetry_minute`, with per-minute event counts and value sums per session and event type,
refreshed every minute. It is for dashboards and longer-range questions.

The backend applies both files at startup, one statement at a time. If the continuous aggregate
cannot be created, it logs that and carries on.

## Aggregation

The summary covers the last `WINDOW_SECONDS` = 300 seconds of one session.

Both backends reduce the window to the same `RecentCounts` and pass them through the same
`summarize` function, so in-memory and Tiger Data produce identical scores:

- in memory: a scan of the session's ring;
- Tiger Data: one `count(*) FILTER (WHERE event_type = …)` query over the raw hypertable.

The query reads the hypertable, not the continuous aggregate, so the summary is exact up to the
last written event and works where the aggregate is unavailable.

## PlayerTelemetry

`director/context.rs`. The only form in which telemetry reaches the Director.

| Field | Meaning | Score |
|---|---|---|
| `window_seconds` | length of the window | `300` |
| `combat_intensity` | damage taken and enemies killed | 10 per damage event + 15 per kill, capped at 100 |
| `recent_deaths` | player deaths | count, capped at 100 |
| `npc_engagement` | NPC interactions | 20 each, capped at 100 |
| `exploration_activity` | distinct locations entered | 25 each, capped at 100 |

Integers, 0–100. `DirectorContext.telemetry` is optional: absent means "unknown", and the
serialized context is then byte-for-byte what it was before telemetry existed.
`DirectorContext::validate()` rejects out-of-range values; the runtime clamps what a reader
returns before it gets there. The whole summary adds well under 160 bytes to a context.

## How the Director uses it

**Gemini** sees `telemetry` as part of the context and is told to pace to the player: breathing
room when `combat_intensity` or `recent_deaths` is high, conversation when `npc_engagement` is
high.

**`FallbackDirector`** has exactly one telemetry rule, in the divergence response
(`objective_refused`, `player_disclosure`):

> If the player is under pressure (`combat_intensity ≥ 60` or `recent_deaths ≥ 2`) or socially
> engaged (`npc_engagement ≥ 60`), and there is an NPC to speak (the one who was told, else the
> one who gave the objective), the escalating `trigger_world_event` is replaced by a
> `start_dialogue` from that NPC.

Everything else about the decision is unchanged. Without telemetry, below the thresholds, or with
nobody to talk to, the fallback behaves exactly as before.

In a live game today the reachable case is `npc_engagement`: a player who talks to NPCs three or
more times in five minutes and then gives away a secret gets a conversation, not a rumour.

Acceptance test: `backend/tests/telemetry_divergence.rs`. Same world, same story state, same
player action; quiet telemetry yields `trigger_world_event` and a `world_event_triggered` world
event, high pressure yields `start_dialogue` and `dialogue_started`.

## Failure behaviour

Telemetry is never on the gameplay path.

- **Writes.** `TelemetrySink::record` returns immediately and cannot fail. `TigerTelemetry` puts
  the event on a bounded queue (1024) and a background task writes it under a 2 s timeout. Queue
  full or database down: events are dropped. One warning is logged per outage, not per event.
- **Reads.** Done in the follow-up task, after the player's action has been acknowledged, under a
  300 ms timeout (`TELEMETRY_READ_TIMEOUT`). Error or timeout: a warning, no telemetry in the
  context, and the Director decides as it did before Phase 3.
- **Startup.** Tiger Data not configured, unreachable, or without a usable schema: the backend
  logs it and uses in-memory telemetry.
- A telemetry failure never rejects a `player_action`.

With Tiger Data the summary may lag the very latest event by the time it takes the writer task to
insert it (milliseconds). In memory there is no lag.

## Setup

```sh
# .env (repo root, git-ignored)
TIGER_DATABASE_URL=postgres://tsdbadmin:<password>@<host>:<port>/tsdb?sslmode=require
```

That one variable is the whole configuration. The URL contains the password, so it is treated as
a secret as a whole: never logged, redacted from driver errors, hidden in `Debug`.

```sh
cargo test                                             # offline, default: in-memory telemetry
cargo run --features tiger                             # Tiger Data when TIGER_DATABASE_URL is set
make lint-tiger                                        # clippy with the feature
make tiger-live                                        # live round trip (needs the URL)
```

The live test (`backend/tests/tiger_live.rs`) creates the schema if needed, writes events for one
random session id, checks the summary, and deletes them. Without `TIGER_DATABASE_URL` it fails
with `BLOCKED`; it never reports a connection it did not make.

**Status: not yet verified against a live Tiger Data service.** No `TIGER_DATABASE_URL` was
available when this was written, so the SQL and the `TigerTelemetry` client have been compiled
and linted but not run against a database.

## Limits

- Combat events have no source yet (see Events).
- Thresholds and weights are fixed constants, not tuned on real play.
- One connection, one insert per event. Enough for one demo session; batch the writer before
  pointing many sessions at it.
- No retention or compression policy is set on the hypertable.
- The Python canon doctor does not know about Tiger Data.
