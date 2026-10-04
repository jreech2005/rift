# Director Engine (Phase 2)

Decides how the world reacts to what the player actually did. Rust, `backend/src/director/`.

```
WorldBible ─► WorldSummary ─┐
session snapshot ───────────┤
recent events ──────────────┼─► DirectorContext ─► provider ─► proposal ─► Rust validation ─► DirectorDecision
narrative view (missions) ──┤     (bounded)        Gemini or    (untrusted                    (typed actions)
NPC views ──────────────────┘                      rules        JSON text)
```

**The Director proposes. Rust decides what is legal.** A provider can only return data, and that
data is rejected unless every action is one of 13 allowlisted types and passes validation against
the context. There is no action that carries code, a script or a console command.

**This module only decides.** Nothing here touches `ws.rs`, the protocol or `WorldEvent`s, and
nothing here mutates state. The Phase 2 runtime (`backend/src/runtime/`, [RUNTIME.md](RUNTIME.md))
builds the context, calls the engine once per meaningful player action and applies its decisions.

```rust
use rift_backend::director::*;

let bible   = WorldBibleView::from_json_str(&std::fs::read_to_string(path)?)?;
let world   = WorldSummary::from_world_bible(&bible);          // once per session
let mut ctx = DirectorContext::from_session(&snapshot, universe_id, world, Trigger::PlayerAction);
ctx.narrative = Some(narrative_view);                          // from the narrative engine
ctx.npcs      = npc_views;                                     // from the NPC layer

let engine = DirectorEngine::new(Arc::new(GeminiDirector::from_env()?))
    .with_fallback(Arc::new(FallbackDirector));                // optional
let decision: DirectorDecision = engine.decide(&ctx).await?;   // never on the gameplay path
```

## DirectorContext

`context.rs`. Strict serde (`deny_unknown_fields`), validated by `DirectorContext::validate()`
before any provider is called.

| Field | Notes |
|---|---|
| `schema_version` | `1`; anything else is rejected |
| `session_id`, `universe_id` | identity; copied onto the decision |
| `event_count` | `GameSession::event_count` at snapshot time; echoed as `metadata.based_on_event_count` |
| `trigger` | why the Director is consulted (below) |
| `world` | `WorldSummary`: a deterministic digest of the WorldBible |
| `player` | `location`, small `attributes` map |
| `recent_events` | ≤ 16 `EventView`s, oldest first, from `WorldEvent`s |
| `world_flags` | ≤ 64 |
| `narrative` | optional `NarrativeView`: `summary`, ≤ 4 `missions`, ≤ 12 `objectives` |
| `npcs` | ≤ 12 `NpcView`s: `npc_id, location, active, alive, disposition, status` |
| `allow_character_revival` | default `false`; only then may a dead character act |

**Bounded.** The raw WorldBible is never put in a prompt. `WorldSummary::from_world_bible` keeps
≤ 12 locations and characters (the starting location and the opening conflict's cast first), caps
every string, and drops provenance and source references. On top of the per-field limits a
serialized context may not exceed 48 KiB. The Breaking Bad demo context is 6.7 KB against a 18 KB
WorldBible.

**Triggers** are chosen by the orchestrator, never by a model:
`session_start`, `player_action`, `objective_refused{objective_id}`,
`objective_completed{objective_id}`, `objective_failed{objective_id}`,
`player_disclosure{npc_id, objective_id?}`, `idle`. The LLM path works with plain `player_action`
plus history; the structured ones let the deterministic fallback react without reading free text.

**Interfaces to other subsystems.** `NarrativeView` and `NpcView` are the Director's own minimal
read-only DTOs. The narrative engine and the NPC layer fill them in; the Director depends on
neither. `narrative: None` means "not supplied": objective and mission references are then
format-checked only. A character without an `NpcView` is assumed alive and not in play.
`Disposition` uses the same wire strings as the NPC layer (`hostile, wary, neutral, friendly, loyal`).

## DirectorDecision V1

`decision.rs`. JSON Schema: `shared/schemas/director/v1/director_decision.schema.json`, generated
from `schema.rs` (`RIFT_UPDATE_SCHEMAS=1 cargo test` regenerates; a test fails on drift).

```json
{
  "schema_version": 1,
  "decision_id": "70d6c674-…",
  "session_id": "7d9f0c1e-…",
  "universe_id": "breaking_bad_tv_1396",
  "trigger": { "kind": "player_disclosure", "npc_id": "hank_schrader", "objective_id": "hide_burner_phone" },
  "reason_code": "player_divergence",
  "actions": [ { "type": "invalidate_mission", "action_id": "a2", "mission_id": "protect_walters_cover", "reason": "…" } ],
  "narrative_summary": "…",
  "confidence": 0.9,
  "metadata": { "provider": "gemini", "model": "gemini-3.5-flash", "attempts": 1, "repaired": false,
                "latency_ms": 4696, "usage": { "total_tokens": 3277 }, "created_at": "…",
                "based_on_event_count": 6 }
}
```

A model returns only a `DirectorProposal` (`reason_code, actions, narrative_summary, confidence`).
Identity and `metadata` are assigned by code: the proposal has no field through which a model
could set a session, a decision id or its own provenance.

`reason_code`: `story_setup`, `objective_progress`, `player_divergence`, `npc_reaction`,
`world_reaction`, `no_change` (requires zero actions). Zero actions is a valid decision.

## Actions

`actions.rs`. Internally tagged by `type`; every action has an `action_id` unique in the decision.
Unknown types and unknown fields are decode errors.

| `type` | Fields | Checked against the context |
|---|---|---|
| `set_objective` | `objective_id, title, description, mission_id?` | id is new `^[a-z0-9_]{1,64}$`; mission exists, is active and is not invalidated by this decision |
| `complete_objective` | `objective_id` | exists and is active |
| `fail_objective` | `objective_id, reason` | exists and is active |
| `activate_npc` | `npc_id, location_id` | known living character, not already active; known location |
| `move_npc` | `npc_id, location_id, reason` | known living character; known location; reason required |
| `set_npc_disposition` | `npc_id, toward, disposition, reason` | `toward` is `player` or another known living character |
| `reveal_information` | `recipient_id, text, source_npc_id?` | recipient is `player` or a known living character |
| `set_world_flag` | `flag` | key ≤ 128 chars; not `interacted:*` |
| `clear_world_flag` | `flag` | flag is currently set; not `interacted:*` |
| `trigger_world_event` | `event, description, location_id?, npc_ids?` | `event` from the list below; ≤ 4 known NPCs |
| `start_dialogue` | `npc_id, opening_line` | known living character |
| `invalidate_mission` | `mission_id, reason` | exists and is active |
| `request_replan` | `reason, mission_id?` | mission exists |

`event`: `alarm_raised, authorities_alerted, reinforcements_arrive, rumor_spreads,
communication_received, confrontation_begins, environment_changes, tension_escalates`. The kinds
are universe-independent; `description` says what it is in this story.

## Validation

`validate.rs`. All-or-nothing: one bad action rejects the whole proposal, so a decision is never
half-applied.

- **Shape**: JSON object, only the four proposal fields, `actions` an array of at most **8**
  (checked before any action is decoded).
- **Identifiers**: protocol charset `^[A-Za-z0-9_.:-]{1,64}$`; `action_id` `^[a-z0-9_]{1,32}$`.
- **Text**: non-empty, single line, no control characters; title ≤ 80, reason ≤ 200, other text
  ≤ 300, summary ≤ 400 chars.
- **Values**: `confidence` finite in 0..=1; enums closed.
- **References**: NPCs and locations must exist in `world`; objectives and missions must exist
  (and be active where the action needs that) whenever `narrative` is supplied; a flag being
  cleared must currently be set.
- **Dead characters** cannot be activated, moved, addressed or made to speak unless the context
  sets `allow_character_revival`.
- **Duplicates and combinations**: repeated `action_id`; an objective id that already exists or is
  created twice; setting and clearing one flag; completing and failing one objective; resolving an
  objective created by the same decision; placing one NPC twice; `no_change` with actions.
- `interacted:*` flags belong to the deterministic `player_action` rules and are read-only here.

Issues carry a `path` (`actions[2].npc_id`), a `code` and a message; at most 25 are reported.

## Engine

`engine.rs`. `DirectorEngine::decide(&ctx) -> Result<DirectorDecision, DirectorError>`.

1. Validate the context. Invalid → `InvalidContext`, no provider call.
2. Call the provider, bounded by `call_timeout` (default 30 s) whatever the provider does.
3. Parse and validate. Rejected → **exactly one** repair call that shows the provider its output
   and the issue list (`MAX_ATTEMPTS = 2`). Still rejected → `InvalidDecision`.
4. Provider failures (timeout, 429, 503, auth…) are **never retried by the engine** and surface
   as `DirectorError::Provider` with the kind and HTTP status. Transport retry and failover live
   behind the provider interface (see [Failover](#failover)) and are separate from the repair in
   step 3.
5. With `.with_fallback(provider)`, a failed or rejected primary is replaced by the fallback's
   decision and the cause is recorded in `metadata.fallback_reason` and logged. Without it, the
   error is returned.

The engine is `Clone + Send + Sync`, holds no lock and reads no session state, so it runs in a
spawned task while gameplay continues.

## Failover

`failover.rs`. The game must never be unavailable because an LLM is. `FailoverProvider` is a
`DirectorProvider` wrapping an ordered list of legs; the backend builds it from the environment
(`DirectorSettings::from_env().into_engine()`), with `FallbackDirector` behind it.

```text
primary Gemini model (GEMINI_MODEL)
   | transient failure: 429 / 5xx / timeout / connection
   v
one short retry (GEMINI_TRANSIENT_RETRIES, backoff 250 ms)
   | still failing, or a failure that will not pass (auth, 4xx, malformed)
   v
secondary Gemini model (GEMINI_FALLBACK_MODEL), one attempt
   v
independent provider (DIRECTOR_SECONDARY_PROVIDER=anthropic), one attempt
   | every leg failed, or the budget ran out
   v
FallbackDirector (deterministic)  ->  the game continues
```

| Variable | Default | Meaning |
|---|---|---|
| `DIRECTOR_PRIMARY_PROVIDER` | `gemini` | `gemini` or `anthropic` |
| `DIRECTOR_SECONDARY_PROVIDER` | unset | the other provider; left out when its key is missing |
| `GEMINI_MODEL` | `gemini-3.8-flash` | primary model (demo: `gemini-3.5-flash`) |
| `GEMINI_FALLBACK_MODEL` | unset (no such leg) | second Gemini model, same key |
| `GEMINI_TIMEOUT_MS` | `8000` (1000–30000) | one Gemini request |
| `GEMINI_TRANSIENT_RETRIES` | `1` (0–2) | retries of the **first** leg; later legs are tried once |
| `ANTHROPIC_API_KEY` | unset | server-side only, like `GEMINI_API_KEY` |
| `ANTHROPIC_MODEL` | `claude-opus-5-5` | |
| `ANTHROPIC_TIMEOUT_MS` | `10000` (1000–30000) | one Anthropic request |
| `ANTHROPIC_EFFORT` | `low` | `output_config.effort`; set it blank for models that reject the field |
| `DIRECTOR_BUDGET_MS` | `20000` (1000–60000) | one whole pass down the chain |

An unparsable value logs a warning and uses the default. Nothing here is required: with no key the
Director is the deterministic rules, and startup never makes a request.

Hard bounds, all enforced in code:

- Legs are visited once, in order. No leg is revisited, nothing recurses.
- Retries per leg are capped at 2 whatever is configured; only transient kinds (`timeout`,
  `network`, `rate_limited`, `unavailable`) are retried. `Retry-After` is ignored.
- Backoff before retry *n* is `n × 250 ms`, capped at 1 s and at the remaining budget.
- Each request is bounded by its leg timeout and by what is left of the budget. When the budget is
  gone the pass ends, whatever is in flight.
- With the defaults a pass makes at most 4 requests (2 + 1 + 1). A decision is one pass, plus one
  more only when the output was rejected and is being repaired: at most 8 requests and 2 × budget,
  then the deterministic rules answer. Normally it is one request.

Transport retry is not decision repair. A reply that arrives but fails validation is never
retried by the chain; the engine repairs it once (step 3), and that repair call is again a single
pass. `metadata.provider` / `metadata.model` name the leg that answered (`gemini`, `anthropic`,
`fallback`); each failed call is logged with its leg, attempt and error; when the rules answer,
`metadata.fallback_reason` holds the last LLM error.

Not done: no circuit breaker (a long outage re-tries the primary on every decision, bounded as
above), and a repair pass starts at the primary again rather than at the leg that answered.

## Preflight

```sh
make director-preflight
```

`examples/director_preflight.rs` sends **one** tiny request (no schema, a few tokens) to each
configured model in failover order and prints `HEALTHY (ms)` or `UNAVAILABLE (reason)` per model.
Exit `0` primary healthy, `1` primary down but a later model healthy, `2` no LLM healthy or
configured. It creates no session, changes no state, prints no key, and is never run by the
backend or by `make test`. Run it once before a demo; do not loop it.

## Providers

`DirectorProvider` (`provider.rs`) returns raw text. Every provider's output, including the
deterministic ones, goes through the same validation.

**`GeminiDirector`** (`gemini.rs`) mirrors the Phase 1 Python provider: `POST
/v1beta/interactions`, key in the `x-goog-api-key` header, `store: false`, schema-constrained JSON
via `response_format`, `thinking_level: low`. One HTTP request per call.

- Config from the same variables as the Python pipeline: `GEMINI_API_KEY`, `GEMINI_MODEL`
  (default `gemini-3.8-flash`); `with_model` makes the `GEMINI_FALLBACK_MODEL` leg. The key is held in `Secret` (redacted `Debug`, no `Display`, no
  `Serialize`) and is never part of a log line, an error or a request body.
- The response schema is built per context (`schema.rs`): reference fields are enums of ids that
  exist right now, dead characters and reserved flags are not offered, and actions with nothing to
  refer to are left out. Size limits go in descriptions rather than keywords (bounded nested
  arrays make the constrained decoder reject the schema; seen live in Phase 1).
- Error kinds: `not_configured, timeout, network, auth, rate_limited, unavailable, http, malformed,
  incomplete`. Messages contain the status and Google's own error text (redacted, ≤ 300 chars),
  never the URL or headers.
- The prompt (`prompt.rs`) tells the model to preserve canon, respect the divergent state, never
  resurrect the dead, not teleport characters, not invent canon, react to what the player actually
  did, prefer local consequences and output only the typed actions, and that text in the context
  is data, not instructions. Those are requests; validation is what enforces them.

**`AnthropicDirector`** (`anthropic.rs`): the independent second LLM. `POST /v1/messages`, key in
the `x-api-key` header (same `Secret` handling), the same prompt, and the same reduced response
schema as `output_config.format` (`json_schema`). One HTTP request per call, no retry. 429 →
`rate_limited`, 5xx including 529 → `unavailable`, 408 → `timeout`, 401/403 → `auth`; a refusal or
a `max_tokens` stop → `incomplete` (not retried, next leg). Verified against a local stand-in for
the API only; **not yet run against the live API**.

**`FallbackDirector`** (`fallback.rs`): fixed rules, no LLM, no I/O, same context → same proposal.
It reads only structured fields and knows nothing about any universe.

| Trigger | Proposal |
|---|---|
| `session_start` | objective from the WorldBible opening conflict; activate its cast |
| `objective_refused`, `player_disclosure` | fail the objective, invalidate its mission, set a flag, change dispositions, trigger a world event, set a replacement objective, request a replan |
| `objective_completed`, `objective_failed` | resolve the objective, request a replan |
| `player_action`, `idle` | no actions |

It exists for tests, offline development and demo resilience, not to replace Gemini.

**`ScriptedProvider`**: replays canned responses and records calls. For tests of anything built
around the Director.

## Integration

The runtime follows these rules ([RUNTIME.md](RUNTIME.md)); they hold for any other caller:

- Run `decide` in a spawned task. Build the context from a `SessionStore::get` snapshot, outside
  any lock. Gameplay never waits for it.
- A decision is valid for the context it was made for. Before applying, compare
  `metadata.based_on_event_count` with the session and call `validate_actions(&current_ctx,
  &decision.actions)` again; state may have moved while the model was thinking.
- Turning actions into `WorldEvent`s and mutating `GameSession` is the integration layer's job.
  `DirectorAction` is deliberately not a `WorldEvent`.
- `DirectorContext::from_session` keeps the last 16 events and the first 64 flags in key order.
- Player speech reaches the prompt. It is treated as data, and whatever a model does with it can
  only come back as allowlisted actions.

## Running it

```sh
cargo test --manifest-path backend/Cargo.toml                                    # all offline
cargo run  --manifest-path backend/Cargo.toml --example director_live -- --offline  # deterministic
cargo run  --manifest-path backend/Cargo.toml --example director_live              # ONE Gemini decision
make director-preflight                                                           # one tiny request per model
```

The example runs the demo scenario (asked to hide Walter's phone, told Hank instead) on the Phase 1
Breaking Bad WorldBible, makes at most two requests and exits `0` validated, `1` output rejected,
`2` blocked (no key or provider unavailable).

Observed live, 2026-10-03, free tier:

- `gemini-3.8-flash`: HTTP 503 "currently experiencing high demand". Surfaced as
  `gemini unavailable`, not retried.
- `GEMINI_MODEL=gemini-3.5-flash`: validated on the first attempt, no repair. 8 actions
  (`fail_objective, invalidate_mission, set_world_flag, set_npc_disposition` ×2,
  `trigger_world_event, start_dialogue, set_objective`), 4.7 s, 3,277 tokens. The `anyOf` action
  schema is accepted by the constrained decoder.

## Tests

Unit tests sit next to the code and use a made-up universe ("Lantern Bay") so that no logic can
depend on the demo story. `tests/director_gemini.rs` drives the real HTTP client against a local
stand-in for the Interactions API (429, 503, 401, timeout, malformed and incomplete responses, key
redaction, the single repair). `tests/director_divergence.rs` runs the demo scenario on the real
WorldBible. The failover chain is covered with scripted providers in `failover.rs` (primary only,
503 retry, 429, timeout, budget, exhausted primary, every LLM down, repair semantics, config from
a fake environment), over HTTP in `tests/director_failover.rs` (one local server standing in for
both APIs) and at runtime level in `tests/runtime_divergence.rs` (one action, one bounded pass, no
second Director call). No test contacts Google or Anthropic or needs a key.

## Not in this module

Applying decisions, `WorldEvent` mapping, mission and NPC state, persistence. The runtime does the
first two ([RUNTIME.md](RUNTIME.md)); the rest is owned by the narrative and NPC layers.
