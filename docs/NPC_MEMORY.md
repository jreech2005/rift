# NPC State & Memory V1

Persistent-agent foundation for NPCs: identity, current state, relationships,
explicit knowledge, episodic memories and bounded retrieved context.

Code: `backend/src/npc/`. **Not wired into the WebSocket path** — `ws.rs`,
`GameSession` and the `PlayerAction` flow are untouched. No dialogue
generation, no LLM, no ElevenLabs.

```
WorldBible character ──► CharacterState ◄── NpcDirectory (authoritative, in memory)
                               ▲
       NpcEvent ──► perceive() ┤  only the event's audience
                               ▼
                         MemoryEntry ──► MemoryStore (in-memory | TiDB)
                                              │ bounded, ranked retrieval
                                              ▼
                                         NpcContext
```

## Ground rules

| Rule | How it is enforced |
|---|---|
| No omniscient NPCs | Knowledge lives in each `CharacterState`. Nothing in `npc/` reads `GameSession`. An NPC knows a fact/flag only after `learn_fact` / `learn_flag` ran for *that* NPC. |
| No broadcast | Every `NpcEvent` carries an `Audience`. There is no "everyone" variant. |
| NPCs cannot leak what they do not know | `FactRevealed` with an NPC speaker is rejected (`UnknownFact`) unless the speaker knows the fact. |
| Deterministic | No clock, RNG or LLM. Callers pass `now` / event timestamps. Memory ids are UUIDv5 of `(event_id, character_id)`. |
| Bounded | Hard caps on every collection, every text field and every query result. |
| Rust validates every mutation | Ids are validated newtypes (also on deserialization); `CharacterState` fields are private and only change through checked methods; events apply atomically. |
| Gameplay never blocks on storage | `NpcDirectory` is in-memory with short synchronous locks. `MemoryStore` is async and called outside any lock. |

## Modules

| File | Contents |
|---|---|
| `ids.rs` | `CharacterId`, `EntityId`, `LocationId` (≤64), `FactId`, `FlagKey` (≤128). Protocol V1 charset `[A-Za-z0-9_.:-]`, so WorldBible ids and protocol targets are valid as-is. |
| `relationship.rs` | `Relationship { trust, fear, affinity }`, `RelationshipDelta`, `Disposition`. |
| `state.rs` | `CharacterState` V1, `LifeStatus`, `Fact`, `KnownFact`, `KnowledgeSource`, WorldBible seeding. |
| `memory.rs` | `MemoryEntry` V1, `MemoryQuery`, deterministic scorer `rank_memories`. |
| `store.rs` | `MemoryStore` trait, `StoreError`, `InMemoryMemoryStore`. |
| `events.rs` | `NpcEvent`, `Audience`, pure adapter `perceive()` → `Perception { memory, effects }`. |
| `directory.rs` | `NpcDirectory`: per-session NPC states, atomic `apply_event`, `record_event`. |
| `context.rs` | `NpcContext`, `build_context()`. |
| `tidb.rs` | `TiDbConfig` (always compiled) and `TiDbStore` (`--features tidb`). |

## CharacterState V1

```jsonc
{
  "schema_version": 1,
  "session_id": "…uuid…",
  "character_id": "hank_schrader",
  "name": "Hank Schrader",
  "role": "DEA agent",                       // optional one-line reference
  "status": "alive",                         // alive | incapacitated | dead (terminal)
  "location": "dea_office",                  // optional
  "relationships": { "player": { "trust": 10, "fear": 0, "affinity": 5 } },
  "goals": ["Catch Heisenberg"],             // ≤ 8
  "knowledge": {                             // ≤ 256; absence = does not know
    "secret:lab": {
      "statement": "The lab is hidden under the laundry.",
      "source": { "kind": "told", "by": "player" },   // canon | witnessed | told
      "learned_at": "2026-10-03T12:00:00Z",
      "source_event": "…uuid…"
    }
  },
  "known_flags": { "lab_destroyed": true },  // ≤ 256; the NPC's *belief*, not GameSession truth
  "version": 6,                              // bumped on every mutation
  "updated_at": "2026-10-03T12:00:05Z"
}
```

Disposition toward the player is derived, not stored:
`disposition_toward_player()` buckets `trust + affinity - fear` into
`hostile | wary | neutral | friendly | loyal`.

Knowledge API (`CharacterState`, or through `NpcDirectory::update` / `knows`):

| Call | Effect |
|---|---|
| `learn_fact(fact, source, source_event, now) -> bool` | `false` if already known; the original source/time are kept (no duplicates). |
| `forget_fact(&fact_id, now) -> bool` | Invalidate a fact (disproved, retconned). |
| `knows(&fact_id) -> bool` | Never consults global state. |
| `learn_flag` / `known_flag` / `forget_flag` | Same, for world flags the NPC has heard about. |

`CharacterState::from_seed` builds the initial state from a WorldBible V1
character: `known_facts` become `canon` knowledge with a fact id derived from
the statement (same statement ⇒ same fact id across characters), `goals` are
copied, and outgoing relationship `kind` labels give a coarse starting
relationship (`partner`, `brother`… ⇒ ally; `rival`, `enemy`… ⇒ hostile).
`NpcDirectory::seed_from_world_bible(session_id, &bible_json, now)` does this
for every character.

## Relationships

Three integer dimensions, clamped on every update and on deserialization:

| Dimension | Range |
|---|---|
| `trust` | -100 ..= 100 |
| `fear` | 0 ..= 100 |
| `affinity` | -100 ..= 100 (≥ 30 ⇒ "ally") |

`adjust_relationship(&entity, delta, now)` is a saturating add. Event-driven
deltas are fixed constants in `events.rs`.

## MemoryEntry V1

```jsonc
{
  "schema_version": 1,
  "memory_id": "…uuid…",          // v5(event_id, character_id) for event memories
  "session_id": "…uuid…",
  "character_id": "walter_white", // who holds the memory
  "event_id": "…uuid…",           // source event, optional
  "memory_type": "observation",   // interaction | observation | revelation | report
  "summary": "I saw the player harm Jesse Pinkman.",   // ≤ 500 chars
  "entities": ["jesse_pinkman", "player"],             // ≤ 16
  "location": "lab",
  "importance": 80,               // 0..=100
  "emotional_valence": -80,       // -100..=100
  "world_time": 12,               // session event sequence
  "created_at": "2026-10-03T12:00:00Z",
  "valid": true,                  // false once invalidated; never retrieved again
  "metadata": { "event_type": "harmed", "perception": "witnessed" }
}
```

## MemoryStore

```rust
trait MemoryStore: Send + Sync {            // object-safe: Arc<dyn MemoryStore>
    fn store_memory(&self, &MemoryEntry)                       -> ();   // validates; idempotent per memory_id
    fn get_memory(&self, memory_id)                            -> Option<MemoryEntry>;
    fn query_recent(&self, session_id, &character_id, limit)   -> Vec<MemoryEntry>;
    fn query_relevant(&self, &MemoryQuery)                     -> Vec<ScoredMemory>;
    fn invalidate_memory(&self, memory_id)                     -> bool;
}
```

`InMemoryMemoryStore` is the default and what every offline test uses. It keeps
at most 512 memories per NPC, evicting the least important, then oldest.

### Bounded retrieval

`MemoryQuery { session_id, character_id, location?, entities, recent_event?, text?, world_time?, limit }`.
Results are capped at **10** (`MAX_QUERY_LIMIT`; default 5) whatever `limit`
says. Ranking is an integer score, identical in every store:

```
importance                          0..=100
+ 40 per query entity in the memory (max 3)
+ 15 per query keyword in summary   (max 4)
+ 20 if it happened at the query location
+ 50 if it is the memory of `recent_event`
+ recency: 30 − events elapsed      (floor 0)
ties: newest world_time, newest created_at, smallest memory_id
```

`build_context(&state, &store, &query)` returns an `NpcContext`: identity,
status, location, disposition and relationships (player + queried entities),
goals, ≤ 12 known facts (query-relevant first), ≤ 16 known flags and the ≤ 10
retrieved memories. This — never the full history — is what a later dialogue
layer should hand to an LLM.

## Event → memory

`perceive(&NpcEvent, &roster)` is a pure function returning, per perceiving
NPC, the memory it forms and the state effects (`LearnFact`, `LearnFlag`,
`AdjustRelationship`).

| `NpcEventKind` | Subject | Effect on those who perceive it |
|---|---|---|
| `helped { actor, target }` | target | relationship toward actor ↑ |
| `threatened { actor, target }` | target | fear ↑, trust ↓ |
| `harmed { actor, target }` | target | trust/affinity ↓↓, fear ↑ (stronger for the target's allies) |
| `fact_revealed { speaker, listener, fact }` | listener | learns the fact |
| `character_died { character, killer? }` | — | learn `died:<id>`; relationship toward killer ↓ |
| `mission_failed { mission_id, summary }` | — | learn flag `mission_failed:<id>` |
| `world_event { summary, entities, flag? }` | — | learn the flag, if any |

| `Audience` | Who gets a memory |
|---|---|
| `participants` | only the subject (a private conversation) |
| `witnesses { witnesses }` | subject + the listed NPCs |
| `location` | subject + every NPC whose `location` equals the event's |
| `reported { informant, listeners }` | only the listeners, second-hand: `report` memory, importance −10, relationship deltas halved. This is the information-transfer event. |

Dead and incapacitated NPCs perceive nothing. Unknown characters, dead actors,
self-targeting, a death reported about a living character, and NPC speakers
revealing unknown facts are rejected and change nothing.

## Integration API (for the later wiring layer)

```rust
let npcs  = NpcDirectory::new();                     // put next to SessionStore in AppState
let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());

npcs.seed_from_world_bible(session_id, &bible_json, now)?;

// After Rust validated a world/Director event, describe it for the NPC layer:
let memories = npcs.record_event(&*store, &npc_event).await?;   // or apply_event() + store later

// When an NPC must react:
let state = npcs.get(session_id, &character_id).unwrap();
let ctx   = build_context(&state, &*store, &MemoryQuery::new(session_id, character_id)
                .with_entity(EntityId::player()).with_text(player_utterance)).await?;
```

`apply_event` is synchronous, atomic and rejects replays
(`DuplicateEvent`). `record_event` then persists; if the store fails the
in-memory state change stands and the error is returned (persistence is
best-effort and off the gameplay path; memory ids are deterministic, so
re-storing is safe).

## TiDB

`TiDbStore` implements `MemoryStore` plus `save_character_state` /
`load_character_state` over the MySQL protocol (`mysql_async`, rustls). It is
behind a cargo feature so the default build and tests need neither the
dependency nor a network:

```sh
cargo test                                   # offline, default
cargo clippy --all-targets --features tidb -- -D warnings
cargo test --features tidb --test tidb_live -- --ignored --nocapture   # live
```

Configuration (repo-root `.env`, server-side only):
`TIDB_HOST`, `TIDB_PORT`, `TIDB_USER`, `TIDB_PASSWORD`, `TIDB_DATABASE`, and
optionally `TIDB_TLS=true|false` (default: on, except for loopback hosts).
`TiDbConfig::from_env()` returns `Missing([...names...])` when any is unset or
empty. The password is held in a `Secret` whose `Debug` prints `<redacted>`,
and driver error messages are scrubbed before they become a `StoreError`.

The live test creates disposable tables `rift_test_<random>_npc_*`, runs a
store/query/invalidate/state round trip and drops them. Without credentials it
**fails with `BLOCKED`**; it never reports a connection it did not make.

**Status: verified live on 2026-10-03 against TiDB Cloud Serverless
(`8.0.11-TiDB-v8.5.3-serverless`, TLS): connect, create tables, store, get,
recent and relevant queries, owner-conflict rejection, invalidation, character
state save/load, drop tables.** `TIDB_DATABASE` must name a database the user
can create tables in (e.g. the default `test`); system schemas such as `sys`
reject `CREATE TABLE`.

Tables (`TableNames::create_statements`, created by `ensure_schema()`):

```
npc_memories(memory_id PK, session_id, character_id, event_id, memory_type, location,
             importance, emotional_valence, world_time, created_at_us, valid,
             summary VARCHAR(500), entry MEDIUMTEXT /* full MemoryEntry JSON */)
  KEY (session_id, character_id, valid, world_time)
  KEY (session_id, character_id, valid, importance)
npc_character_states(session_id, character_id PK, version, updated_at_us, state MEDIUMTEXT)
```

### Vector / full-text extension point

V1 `query_relevant` in TiDB fetches a bounded candidate set (200 newest + 100
most important valid memories of that NPC) and ranks it with the same
`rank_memories` as the in-memory store. No embedding provider is required.

To add semantic retrieval later, only the candidate query changes:

```sql
ALTER TABLE npc_memories ADD COLUMN embedding VECTOR(768) NULL;
ALTER TABLE npc_memories ADD VECTOR INDEX idx_embedding ((VEC_COSINE_DISTANCE(embedding)));
-- candidates: WHERE session_id = ? AND character_id = ? AND valid = 1
--             ORDER BY VEC_COSINE_DISTANCE(embedding, ?) LIMIT 50
-- or lexical: ALTER TABLE npc_memories ADD FULLTEXT INDEX idx_summary (summary);
```

Embeddings would be computed asynchronously by a provider behind an interface
and written after the row exists; rows without an embedding keep working
through the current path.

## Not in V1

- Live session wiring, dialogue prompts, voice.
- Lies and rumours (an NPC told something false), forgetting by decay.
- NPC memories for the *actor* of an event.
- JSON Schemas under `shared/schemas/npc/` — the serde types above are the
  contract until something outside Rust needs them.
