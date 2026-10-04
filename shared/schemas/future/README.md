# Future contracts (placeholders — not implemented)

Nothing here is wired into code. These notes reserve names and intent so later
phases start from a shared vocabulary.

| Contract | Phase | Owner | Intent |
|---|---|---|---|
| `WorldBible` | 1 — Universe Compiler | Python (produces), Rust (loads) | **Defined in Phase 1:** `../universe/v1/world_bible.schema.json`. Canon for one story universe, produced offline and cached under `cache/universes/`. Loading it in Rust is still future work. |
| `CharacterState` | 4 — NPC Agents & Memory | Rust | Per-NPC runtime state: location, disposition, goals, memory references. Authoritative copy lives in the GameSession. |
| `Mission` | 2/5 — Director, Narrative Causality | Rust | Objective with preconditions, progress and outcomes; may be proposed by the Director but only activated after Rust validation. |
| `DirectorAction` | 2 — Director Engine | Rust validates | A *proposal* from the async AI Director (spawn, dialogue, mission change...). Never mutates state directly; Rust validates it and emits `WorldEvent`s. |

Rule: anything an LLM produces is a proposal. Only validated `WorldEvent`s reach Unreal.
