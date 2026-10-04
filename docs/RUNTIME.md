# Phase 2 Runtime

How a `player_action` becomes world events once the NPC, Narrative and Director systems are in
play. Code: `backend/src/runtime/`. Tests: `backend/tests/runtime_divergence.rs`,
`backend/tests/runtime_ws.rs`.

`ws.rs` is transport only. It decodes a frame, hands a validated action to `Runtime`, and
serialises what comes back.

## Flow

```
player_action
  │  action::validate                                   (ws.rs)
  ▼
Runtime::apply_player_action                            sync, deterministic, all-or-nothing
  1. action::apply on a draft GameSession               duplicate / invalid actions stop here
  2. NPC perception      disclosure → NpcEvent           only the audience learns or remembers
  3. Narrative           NarrativeEngine::apply_event    objectives, missions, canon policy, ReplanRequest
  4. commit              NpcDirectory, NarrativeState, GameSession
  5. DirectorContext     built from the committed state  only for a meaningful action
  │
  ├─► world_event  (acknowledgement, reply_to set)       first frame, same as before Phase 2
  ├─► world_event* (narrative consequences)
  ▼
Runtime::complete                                       async, off the read path
  6. persist NPC memories     Arc<dyn MemoryStore>
  7. DirectorEngine::decide   once
  8. revalidate               validate_actions against a context built now
  9. apply                    all actions on drafts, then commit
  │
  └─► world_event* (Director consequences, no reply_to)
```

The first half never touches a model or a database. The second half runs in a task spawned per
action; the connection keeps reading while it runs.

## State

The runtime adds no session model. It drives the existing stores and holds only what had no home:

| State | Owner |
|---|---|
| location, world flags, event log, sequence | `SessionStore` / `GameSession` |
| NPC knowledge, relationships, location, life status | `NpcDirectory` |
| NPC memories | `Arc<dyn MemoryStore>` (in-memory, or TiDB with `--features tidb`) |
| missions, objectives, planned beats, world truths | `NarrativeState`, kept per session by `Runtime` |
| NPCs on stage, Director replan requests | `Runtime`, per session |
| gameplay telemetry (recent events) | `Arc<dyn TelemetrySink>` / `Arc<dyn TelemetryReader>` (in-memory, or Tiger Data with `--features tiger`); see [TELEMETRY.md](TELEMETRY.md) |

One mutex per session serialises every runtime write to it. It is held for short synchronous
sections and never across an `.await`.

## Worlds

A session runs a story only if the backend was started with a world:

| Variable | Meaning |
|---|---|
| `RIFT_WORLD_BIBLE` | Path to a compiled WorldBible (`cache/universes/<id>.json`). Unset: no world. |
| `RIFT_SCENARIO` | Optional path to a scenario: `{ plan, world, secrets }`. |

The WorldBible seeds all three systems: `NpcDirectory::seed_from_world_bible`,
`WorldSummary::from_world_bible` (the Director's digest) and `narrative::build_initial` (a minimal
opening plan). A scenario replaces that plan with an authored `NarrativePlan` + `WorldFacts` and
adds `secrets`. Example: `backend/tests/fixtures/runtime/scenario_burner_phone.json`.

A world that is configured but does not load stops the backend at startup. A session without a
world takes the plain protocol V1 path: `SessionStore::apply_action`, one `world_event`, nothing
else. That is the path `make smoke` and Unreal's `Rift.NetSmoke` exercise, and it is unchanged.

## Player action → NPC and Narrative

| Action | NPC layer | Narrative layer |
|---|---|---|
| `speak` to an NPC, content matches a secret | `FactRevealed` (speaker: player), audience `Participants` | `fact_revealed { fact_id, to: npc }` |
| `interact` (first time) | — | `flag_set { interacted:<target> }` |
| `move` to a location the story knows | — | `player_moved` |
| anything else | — | — |

A secret is `{ fact_id, statement, keywords, objective_id? }`. The player gives it away when the
spoken `content` contains one of the keywords, case-insensitively. No model reads player text on
this path. Talking to an NPC that is dead, incapacitated or not in the session discloses nothing.

Flags set by narrative plan effects (`set_flag`) are mirrored into `GameSession::world_flags`.

## When the Director is asked

Only for a meaningful action, and at most once per action:

| What happened | Trigger |
|---|---|
| the player told an NPC a secret | `player_disclosure { npc_id, objective_id? }` |
| an objective failed or was invalidated | `objective_failed` |
| an objective completed | `objective_completed` |
| any other mission, objective or beat change, or a `ReplanRequest` | `player_action` |
| nothing changed in the story | not asked |

The context is built after steps 1–4, so the Director sees what the action did: the failed
objective, the mission that went with it, the flags the plan set, and a one-line summary of which
planned beats can no longer happen and which are now possible.

What a decision produces (objectives, flags, world events, replan requests) is applied and
recorded directly. It never becomes a trigger, so there is no Director → Director loop.

## Applying a decision

`Runtime::apply_decision` rebuilds the context from current state and runs
`director::validate_actions` against it. A decision that no longer fits (the session moved while
the model was thinking, an unknown id, a dead NPC, an `interacted:*` flag) is rejected whole.
Otherwise every action is applied to drafts of the session, the narrative state and the NPC roster,
and the drafts are committed only if all of them applied.

| Action | Applied as | `event_type` |
|---|---|---|
| `set_objective` | `adopt_mission` of a one-objective mission (the narrative layer has no free-standing objectives) | `objective_updated` |
| `complete_objective`, `fail_objective` | `objective_completed`, `objective_failed` | `objective_updated` |
| `invalidate_mission` | `mission_invalidated` (no effects fire) | `mission_updated` |
| `activate_npc` | NPC location + on-stage set, `character_moved` | `npc_activated` |
| `move_npc` | NPC location, `character_moved` | `npc_moved` |
| `set_npc_disposition` | `set_relationship` with a value that reads as that disposition | `npc_disposition_changed` |
| `reveal_information` | to an NPC: `FactRevealed` to that NPC alone; to the player: event only | `information_revealed` |
| `set_world_flag`, `clear_world_flag` | session flag + `flag_set` | `world_flag_changed` |
| `trigger_world_event` | `NpcEventKind::WorldEvent`, perceived by the named NPCs, else by NPCs at the location | `world_event_triggered` |
| `start_dialogue` | presentation only | `dialogue_started` |
| `request_replan` | recorded (`Runtime::replan_notes`), reported as deferred, not executed | — |

`DirectorReport` says what happened: `NotInvoked`, `Applied { decision, deferred }`,
`Rejected { decision, error }` or `Failed(error)`.

## Failure behaviour

- **Rejected player action** (invalid, unknown session, duplicate `action_id`): no session, NPC or
  narrative change, no memory, no Director call.
- **Provider failure** (unavailable/503, rate-limited/429, timeout, output still invalid after the
  one repair): handled by `DirectorEngine` as before. With Gemini configured, `FallbackDirector`
  answers instead. If nothing produces a decision the report is `Failed`; the player action and
  its deterministic consequences stand and the session carries on.
- **Memory store failure**: logged and reported in `FollowUpOutcome::memory_errors`. NPC state is
  authoritative in memory, so recall is lost, not state.
- **Telemetry failure**: recording cannot fail or block (events are dropped). If the recent
  summary cannot be read within 300 ms the Director decides without it. Never rejects an action.

The Director is Gemini with `FallbackDirector` behind it when `GEMINI_API_KEY` is set, and the
deterministic rules alone otherwise.

## NPC privacy

- A disclosure reaches the listener only (`Audience::Participants`). No other NPC learns the fact
  or forms a memory.
- The Director is given `NpcView`: id, location, on stage, alive, disposition toward the player. It
  carries no facts, flags or memories. Seeing global state teaches no NPC anything.
- `reveal_information` teaches the named recipient only. A named source that does not itself know
  the information is not made to know it. The `information_revealed` event sent to the client
  omits the text unless the recipient is the player.
- Flags the Director sets are world flags; no NPC's `known_flags` change.

## Protocol

V1, additive. `world_event` gains nine `event_type`s (see `docs/PROTOCOL.md`) and may arrive
without `reply_to`. The reply to a `player_action` is still exactly one `world_event` with
`reply_to` set, sent first. Unreal reads `event_type` as a string and needed no change.

## Limits

- No Director call at session start; NPCs in the player's starting location are on stage.
- `request_replan` is recorded, not executed. There is no planner yet.
- Relationship changes in the NPC layer are not reported to the narrative layer as
  `relationship_changed`.
- Runtime events share the session's 32-event log, so in a session with a world the
  duplicate-`action_id` window covers fewer player actions.
- A Director task still running when its client disconnects finishes and applies its decision;
  the events are recorded but not delivered.
