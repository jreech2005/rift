# Narrative Causality (Mission Engine V1)

The deterministic story layer. Rust, `backend/src/narrative/`. No LLM, no network, no clock,
no randomness: the same state and event always give the same result.

It answers three questions:

1. What is supposed to happen now? → active missions and objectives
2. What does that depend on? → prerequisites
3. What happens when the player breaks those dependencies? → invalidation and a typed `ReplanRequest`

**Status: not wired into the live path.** Nothing in `ws.rs` or `session.rs` calls it and no
`player_action` reaches it yet. That is a later integration pass (see [Integration](#integration)).

```
WorldBible ─► build_initial ─► NarrativePlan + WorldFacts ─► NarrativeEngine::start ─► NarrativeState
                                                                                          │
                              NarrativeEvent ─► NarrativeEngine::apply_event ─────────────┤
                                                                                          ▼
                                             NarrativeTransition { state, changes, effects, replan? }
                                                                                          │
                                                                    ReplanRequest ─► Director (later)
                                                                                          │
                                        NarrativeEngine::adopt_mission ◄── proposed mission (validated)
```

## Two timelines

| | Type | Holds |
|---|---|---|
| Reference timeline | `NarrativePlan` | What is *supposed* to happen: missions, and planned beats (`NarrativeCheckpoint`), some of them canon |
| Actual timeline | `WorldFacts` | What is *true* in the played world |

Canon is the reference. Once the player changes the world, future canon is re-evaluated against
it and is never forced.

`NarrativeState { plan, world, revision, actual_timeline }` holds both. `actual_timeline` is the
last 64 applied events.

## World truth vs canon event

These are different types in different places, on purpose.

- A **world truth** (`WorldTruth`, in `state.world.truths`) is an underlying fact: *"the creature
  exists"*. It changes only through an explicit `truth_ended` or `truth_established`.
- A **canon event** (`NarrativeCheckpoint`, in `state.plan.checkpoints`) is a planned beat: *"the
  protagonists discover the creature in the school"*. It can be skipped, adapted, replaced or
  deleted.

Skipping or deleting a beat never touches a truth. A beat that needs the truth
(`truth_holds`) survives a skipped event; a beat that needs the *event* (`checkpoint_reached`) does not.

## Model

**Mission** — `mission_id, title, description, status, objectives, prerequisites, success_effects,
failure_effects, related_characters, related_locations, canon_relation, importance, metadata`
Status: `inactive → active → completed | failed | invalidated`.

**Objective** — `objective_id` (unique across the plan), `description, status, optional,
prerequisites, completes_when, fails_when, success_effects, failure_effects`
Status: `pending → active → completed | failed | invalidated`.

**NarrativeCheckpoint** — `checkpoint_id, title, description, canon_relation, importance,
prerequisites, reached_when, status, policy`
Status: `pending → reached | skipped`. Policy: `preserve | adapt | replace | delete`.

`failed` means the player or the world did what the objective forbade. `invalidated` means the
premise collapsed: something it depended on is permanently gone.

`canon_relation` is `canon | inferred | generated`, the same vocabulary as WorldBible
`classification`. `importance` is `minor | major | critical`.

Ids use the protocol V1 identifier charset `[A-Za-z0-9_.:-]`: 1–64 chars for entities, 1–128 for
flag keys, fact ids and plan ids. A WorldBible entity id or a protocol `target` is always valid.

## Prerequisites

A `Prerequisite` is `{ condition, necessity }`. Conditions are data, never code, and there is no
negation.

Evaluation is three-valued:

| Verdict | Meaning |
|---|---|
| `holds` | True now |
| `unmet` | False now, can still become true |
| `broken` | Can never become true |

| Condition | `broken` when |
|---|---|
| `character_alive`, `character_available`, `character_at` | the character is dead (`character_at`: or the location is lost) |
| `character_dead` | never |
| `player_at`, `location_available` | the location is lost |
| `object_intact` | the object is destroyed |
| `objective_is`, `mission_is` | the target can no longer reach that status (statuses only move forward) |
| `checkpoint_reached` | the beat was skipped, or is classified `replace` / `delete` |
| `fact_known { fact_id, by }` | `by` is a dead character who never learned it |
| `fact_secret { fact_id, from }` | `from` already knows it — exposure cannot be undone |
| `relationship_at_least`, `relationship_at_most` | either party is a dead character |
| `truth_holds` | the truth has ended |
| `flag_is` | never — flags are reversible |
| `all` | any part is broken |
| `any` | every part is broken |

Every `broken` verdict is permanent and names what was lost as a `LostDependency`:
`character_dead`, `object_destroyed`, `location_lost`, `fact_exposed`, `objective_unreachable`,
`mission_unreachable`, `checkpoint_unreachable`, `truth_ended`.

Use a flag for anything that can be undone. Use a fact, object, location, truth or character for
anything that cannot.

`necessity` is `essential` (default) or `flexible`. An essential prerequisite that breaks makes
its owner impossible. A flexible one that breaks is waived: the owner goes ahead in adapted form.

## Engine

```rust
NarrativeEngine::start(plan, world)        -> Result<NarrativeTransition, NarrativeError>
NarrativeEngine::apply_event(&state, &ev)  -> Result<NarrativeTransition, NarrativeError>
NarrativeEngine::adopt_mission(&state, m)  -> Result<NarrativeTransition, NarrativeError>
```

All three are pure. The input state is never modified, and an `Err` means nothing changed.
`NarrativeTransition` carries the next `state`, `mission_changes`, `objective_changes`,
`checkpoint_changes`, the `effects` that fired, and an optional `replan`.

After an event is applied, consequences are derived until nothing changes:

| | Rule |
|---|---|
| Inactive mission | Essential prerequisite broken → `invalidated`. All prerequisites hold → `active`. |
| Active mission | Essential prerequisite broken → `invalidated`. A required objective `failed` → `failed`. A required objective `invalidated` → `invalidated`. All required objectives `completed` → `completed`. |
| Objective (in an active mission) | `fails_when` holds → `failed`. Else essential prerequisite broken, or `completes_when` broken → `invalidated`. Else pending with prerequisites met → `active`; active with `completes_when` holding → `completed`. |
| Pending beat | Re-classified (below). `delete` → `skipped`. `reached_when` holding with prerequisites met → `reached`. |

When a mission ends, its open objectives become `invalidated`. `success_effects` fire on
`completed`, `failure_effects` on `failed`; an `invalidated` mission fires nothing.

Effects are `set_flag`, `reveal_fact`, `adjust_relationship`, `establish_truth`, `end_truth`.
They cannot kill, create or revive anything.

Events (`NarrativeEvent`): `flag_set`, `character_died`, `character_availability_changed`,
`character_moved`, `player_moved`, `location_lost`, `object_destroyed`, `fact_revealed`,
`relationship_changed`, `truth_ended`, `truth_established`, `objective_completed`,
`objective_failed`, `checkpoint_reached`, `checkpoint_skipped`.

An event that contradicts the world is rejected: a dead character cannot move, become available
or learn anything; a beat cannot be declared reached unless its prerequisites hold.

Before anything runs, `validate` checks the plan against the world: schema version, id shape,
uniqueness, that every reference resolves, and that no mission, objective or beat waits on itself
(dependency cycles, through any chain of `objective_is` / `mission_is` / `checkpoint_reached`).

## Canon adaptation

Each pending beat is classified against the current world.

| The beat's situation | Policy |
|---|---|
| Nothing it needs is lost (prerequisites hold, or are merely unmet) | `preserve` |
| Only flexible prerequisites are lost | `adapt` — it can still happen, in altered form |
| Impossible, and gravity ≥ 3, and no truth it needs has ended | `replace` — it stays pending as an open slot for the Director |
| Impossible, and gravity < 3 or a truth it needs has ended | `delete` — it becomes `skipped` |

*Impossible* means an essential prerequisite, or `reached_when`, is broken.

A beat is preserved because its prerequisites are still valid, never because the source material
says it happened. A generated beat with valid prerequisites is preserved exactly like a canon one.

## Canon gravity

```
gravity = importance (minor 0, major 2, critical 4) + canon_relation (generated 0, inferred 1, canon 2)
```

A number from 0 to 6. It does exactly two things:

1. When a beat is impossible, gravity ≥ 3 (`REPLACE_MIN_GRAVITY`) keeps its *place* in the story
   (`replace`) instead of dropping it (`delete`).
2. `NarrativeState::ready_checkpoints()` lists the beats whose prerequisites hold right now,
   highest gravity first, so high-importance canon events come first when they can still
   naturally happen.

It never turns a broken prerequisite into `preserve`, and `checkpoint_reached` on a contradicted
beat is an error whatever its gravity.

## ReplanRequest

Emitted when a mission or objective is `failed` / `invalidated`, or a beat's policy changes.
Objectives voided only because their mission *completed* do not count.

| Field | Contents |
|---|---|
| `reason` | Most severe of `mission_invalidated`, `mission_failed`, `objective_invalidated`, `objective_failed`, `canon_divergence` |
| `invalidated_missions`, `invalidated_objectives` | Each with `from`, `to` and a `cause` |
| `lost_prerequisites` | Every `LostDependency` from this step, deduplicated |
| `world_changes` | The `trigger` event and the plan `effects` that fired |
| `beats` | Every beat still ahead, re-evaluated, plus those dropped in this step |
| `remaining_context` | Available / unavailable / dead characters, available / lost locations, intact / destroyed objects, holding / ended truths, flags, player location, active and completed missions |

`cause` is one of `prerequisite_broken { lost }`, `completion_impossible { lost }`,
`fail_condition_met { condition }`, `reported`, `objective_failed`, `objective_invalidated`,
`mission_ended`.

Abbreviated example — companion Maya dies during "stop the experiment":

```json
{
  "schema_version": 1,
  "revision": 2,
  "reason": "mission_invalidated",
  "invalidated_missions": [{
    "mission_id": "stop_experiment", "from": "active", "to": "invalidated",
    "cause": { "type": "objective_invalidated", "objective_id": "maya_bypasses_door" }
  }],
  "invalidated_objectives": [{
    "mission_id": "stop_experiment", "objective_id": "maya_bypasses_door",
    "from": "active", "to": "invalidated",
    "cause": { "type": "prerequisite_broken",
               "lost": [{ "type": "character_dead", "character_id": "maya" }] }
  }],
  "lost_prerequisites": [{ "type": "character_dead", "character_id": "maya" }],
  "world_changes": { "trigger": { "type": "character_died", "character_id": "maya" }, "effects": [] },
  "beats": [{ "checkpoint_id": "maya_sacrifice", "policy": "replace", "gravity": 6, "ready": false,
              "lost": [{ "type": "character_dead", "character_id": "maya" }] }],
  "remaining_context": {
    "available_characters": ["dr_voss"], "dead_characters": ["maya"],
    "holding_truths": ["experiment_running"]
  }
}
```

The engine never invents a replacement. The cast is fixed when the state is created, and no event
or effect adds or revives a character. A Director's answer comes back through `adopt_mission`,
which validates it like any plan and rejects it (`Impossible`) if it depends on something already
lost — a mission that needs Maya alive cannot be adopted.

## Initial plan

`build_initial(&WorldBibleSeed)` derives one opening mission from a WorldBible. `WorldBibleSeed`
reads the fields it needs and ignores the rest, so a cached `cache/universes/<id>.json` loads
unchanged.

| WorldBible | Becomes |
|---|---|
| `characters`, `locations` | The cast and locations; characters in the opening conflict are placed at its location |
| `starting_location` | The player's location |
| `world_rules` | Truths `world_rule_1…n` |
| `important_conflicts` | Truths, by conflict id |
| `opening_conflict` | Mission `opening_conflict` (generated, critical), which needs its location and every involved character alive |
| `opening_conflict.immediate_goal` | Objective `opening_conflict.resolve`, completed when any flag `opening_conflict.choice_<n>` is set |
| start ≠ conflict location | An extra first objective `opening_conflict.reach_scene` |
| — | Beat `opening_conflict_resolved`, reached when the mission completes |

It does not generate a campaign. WorldBible V1 has no list of future canon events, so the builder
creates no canon beats; those come later from the Director or a richer WorldBible.

## Integration

Not done here. For the later pass:

- Keep a `NarrativeState` per session next to `GameSession`. Translate each validated `WorldEvent`,
  and NPC state changes, into `NarrativeEvent`s and call `apply_event` synchronously — it is
  in-memory and never waits on anything.
- NPC `LifeStatus` maps to `CharacterStatus` (`alive` → `available`, `incapacitated` →
  `unavailable`, `dead` → `dead`); relationships use the same `trust / fear / affinity` axes.
- Hand `ReplanRequest`s to the Director asynchronously. Its proposals come back through
  `adopt_mission`; gameplay never waits for them.
- Turn `transition.effects` and mission / objective changes into `world_event`s for Unreal. That
  is a protocol change and must follow the shared-contract rule in `CLAUDE.md`.

## Limits of V1

- Missions activate as soon as their prerequisites hold. Gate one on a flag to hold it back.
- A lost location is lost for good. There is no "temporarily closed".
- A beat that depends on a `replace` beat is treated as broken, even though a replacement may
  later fill that slot.
- Cycle detection covers explicit references. It does not catch an objective that waits for its
  own mission to complete.
- There is no JSON Schema under `shared/schemas/`: nothing crosses a process boundary yet, and the
  Rust types are the contract.

## Tests

`cd backend && cargo test --test narrative` — offline and deterministic. Fixtures for the Maya,
ledger and creature stories live in `backend/tests/narrative/fixtures.rs`; none of them appear in
`src/`. The builder runs against `fixtures/world_bible_breaking_bad.json`, a full WorldBible V1
generated from the canon test fixtures.
