# Scenario Builder

Turns a compiled `WorldBible` into a runtime scenario: the JSON the Rust backend loads through
`RIFT_SCENARIO`. Runs before gameplay, in Python (`canon/src/rift_canon/scenario.py`).
Deterministic and offline: no model, no network, the same WorldBible always gives the same bytes.

With it, any title the Universe Compiler can compile is playable without hand-written JSON.

## The generic flow

```
title ─► CanonPacket ─► WorldBible ─► Scenario Builder ─► runtime ─► Unreal
         acquire.py     compiler.py   scenario.py         RIFT_WORLD_BIBLE   world_event
         (TMDB +        (Gemini,      (deterministic)     + RIFT_SCENARIO    over WebSocket
          Wikipedia)     validated)
```

```sh
cd canon
uv run python -m rift_canon.compile "The Matrix" --output ../cache/universes/the_matrix.world.json
uv run python -m rift_canon.scenario \
  --world ../cache/universes/the_matrix.world.json \
  --output ../cache/universes/the_matrix.scenario.json

cd ..
RIFT_WORLD_BIBLE=cache/universes/the_matrix.world.json \
RIFT_SCENARIO=cache/universes/the_matrix.scenario.json \
make backend
```

The first step needs `TMDB_API_KEY` and `GEMINI_API_KEY` ([UNIVERSE_COMPILER.md](UNIVERSE_COMPILER.md)).
The Scenario Builder needs neither. Without `--output` the scenario is printed on stdout.
Exit codes: `0` success, `1` the file is not a valid WorldBible or the scenario would not load.

## An entry slice, not the universe

The generated scenario is deliberately small: **enough canon to enter the story**, not a
simulation of the franchise. It covers the WorldBible's `opening_conflict` and nothing else:

- one mission
- one primary objective and one follow-up
- one beat (checkpoint)
- the starting location (plus the opening's location when it is a different one)
- only the characters of the opening conflict
- one starting flag
- no secrets

Everything else in the WorldBible is still in play: the runtime seeds every NPC and the
Director's world digest from the WorldBible itself ([RUNTIME.md](RUNTIME.md), "Worlds"). What
happens after the opening is the Director's job.

## Mapping

| WorldBible | Scenario |
|---|---|
| `universe.universe_id` | `plan.universe_id`; `plan.plan_id` = `<universe_id>.entry` |
| `opening_conflict.title`, `.summary` | mission `opening_conflict`: `title`, `description` |
| `opening_conflict.stakes` | mission `metadata.stakes`; checkpoint `description` |
| `opening_conflict.involved_character_ids` | `world.characters` (placed at the opening's location), mission `related_characters`, one essential `character_alive` prerequisite each |
| `opening_conflict.location_id` | mission `related_locations`, essential `location_available` prerequisite |
| `opening_conflict.immediate_goal` | objective `opening_conflict.resolve`: `description` |
| `opening_conflict.decision_options[n]` | `resolve` completes when `interacted:choice_<n>` or `opening_conflict.choice_<n>` is set; text kept in mission `metadata.decision_option_<n>` |
| `player_role.connections`, `relationships` with `player` | the *contact*: the first opening character the player knows. Mission `metadata.giver_npc_id`; objective `opening_conflict.report` ("Report to <name>.") completes on `interacted:<contact>` |
| `player_role.title` | mission `metadata.player_role` |
| `starting_location.location_id` | `world.player_location`, `world.locations` |
| `world_rules[n]` | `world.truths.world_rule_<n>` (`canon_relation` = `classification`) |
| — | `world.flags`: `opening_conflict.resolved = false`, set to `true` by `resolve` |
| — | checkpoint `opening_conflict_resolved`, reached when the mission completes |
| — | `secrets: []` |

Ids are fixed (`opening_conflict`, `opening_conflict.resolve`, `opening_conflict.report`,
`opening_conflict_resolved`) and are the ones `narrative::build_initial` uses for the plan the
runtime derives when no scenario is given. When the starting location is not the opening's
location, the objectives are `opening_conflict.reach_scene` (`player_at`) followed by `resolve`,
and there is no `report`.

Why a scenario at all, when the runtime already derives an opening plan? That plan can only be
completed by flags (`opening_conflict.choice_<n>`) that no player action sets. The generated
scenario adds the `interacted:*` conditions a player can reach from Unreal.

Why no Gemini? Every field the scenario format requires can be derived from the WorldBible. The
one thing that cannot is `secrets` (the keywords that turn a spoken line into a disclosure), and
those are optional, so they are left empty rather than guessed.

## Playing it

| Player action | Result |
|---|---|
| `interact` with `choice_<n>` | `opening_conflict.resolve` completes, `opening_conflict.report` becomes active, the Director is asked |
| `interact` with the contact NPC | `opening_conflict.report` completes, the mission completes, the beat is reached |

In Unreal that is one actor per decision option with a **Rift Entity** whose *Rift Id* is
`choice_1`, `choice_2`, … (*Interact Action Type* `interact`), and one `BP_RiftNPC` per character
with the character's id (*Interact Action Type* `interact` on the contact). The option texts are
printed by the CLI and stored in the mission metadata. Level assembly is the same as in
[UNREAL_HOSPITAL_DEMO.md](UNREAL_HOSPITAL_DEMO.md).

The Director can also resolve the conflict itself with `set_world_flag opening_conflict.choice_<n>`.

## Validation

`scenario.scenario_errors` checks the invariants a generated scenario could break (id charset and
length, unique ids, every referenced character, location, mission and objective exists, universe
id matches) and the CLI refuses to write a scenario that fails them. The authority is still the
runtime: `RuntimeWorld::check` runs at backend startup and stops it if the scenario does not load.

## Tests

- `canon/tests/test_scenario.py` — Breaking Bad, The Matrix and Harry Potter WorldBibles each
  build a valid scenario; references, determinism, CLI. Sockets are blocked.
- `backend/tests/scenario_builder.rs` — the runtime boots from each generated scenario: session
  created at the starting location, mission and first objective active, a player action accepted,
  a valid `DirectorContext` built, the Director's decision applied, the mission played to
  completion. No Unreal.

Both use the fixtures in `backend/tests/fixtures/scenario_builder/`. The `*.scenario.json` files
there are the builder's output; the Python test fails if they drift. To regenerate one:

```sh
cd canon
uv run python -m rift_canon.scenario \
  --world ../backend/tests/fixtures/scenario_builder/the_matrix.world.json \
  --output ../backend/tests/fixtures/scenario_builder/the_matrix.scenario.json
```

## Limits

- One opening conflict only. No second mission, no campaign.
- No `secrets`, so a `speak` action discloses nothing and never triggers the Director in a
  generated scenario. Add them by hand if the slice needs one (see `scenario_burner_phone.json`).
- Which decision option was taken is recorded as a flag; the options do not branch the plan.
- If the player interacts with the contact before resolving the conflict, `report` completes as
  soon as it becomes active.
- No numeric relationships are seeded: WorldBible relationship kinds are free text. The NPC layer
  reads relationships from the WorldBible itself.
- The Unreal level (actors, ids, markers) is still assembled by hand.
