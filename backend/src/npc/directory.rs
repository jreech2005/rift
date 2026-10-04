//! `NpcDirectory`: the authoritative in-memory NPC states of every session.
//!
//! Mirrors `SessionStore`: cheap to clone, short synchronous critical
//! sections, never locked across an `.await`. Only this type mutates
//! `CharacterState`s in response to events, and it applies each event
//! atomically — either every perceiving NPC is updated or none is.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::NpcError;
use super::events::{Audience, NpcEvent, NpcEventKind, Roster, perceive};
use super::ids::{CharacterId, FactId};
use super::memory::MemoryEntry;
use super::state::{CharacterState, seeds_from_world_bible};
use super::store::MemoryStore;

/// Maximum NPCs per session.
pub const MAX_CHARACTERS_PER_SESSION: usize = 64;
/// Recently applied event ids remembered per session to reject replays.
pub const APPLIED_EVENTS_CAP: usize = 256;

#[derive(Debug, Default)]
struct SessionNpcs {
    characters: Roster,
    applied: VecDeque<Uuid>,
}

#[derive(Debug, Clone, Default)]
pub struct NpcDirectory {
    inner: Arc<RwLock<HashMap<Uuid, SessionNpcs>>>,
}

impl NpcDirectory {
    pub fn new() -> Self {
        Self::default()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<Uuid, SessionNpcs>> {
        self.inner.read().expect("npc directory lock poisoned")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<Uuid, SessionNpcs>> {
        self.inner.write().expect("npc directory lock poisoned")
    }

    /// Register an NPC in its session.
    pub fn insert(&self, state: CharacterState) -> Result<(), NpcError> {
        state.validate()?;
        let mut sessions = self.write();
        let session = sessions.entry(state.session_id()).or_default();
        if session.characters.contains_key(state.character_id()) {
            return Err(NpcError::DuplicateCharacter(
                state.character_id().to_string(),
            ));
        }
        if session.characters.len() >= MAX_CHARACTERS_PER_SESSION {
            return Err(NpcError::LimitExceeded {
                what: "characters per session",
                max: MAX_CHARACTERS_PER_SESSION,
            });
        }
        session
            .characters
            .insert(state.character_id().clone(), state);
        Ok(())
    }

    /// Create every character of a WorldBible V1 document in `session_id`.
    /// All-or-nothing. Returns the new character ids.
    pub fn seed_from_world_bible(
        &self,
        session_id: Uuid,
        bible: &serde_json::Value,
        now: DateTime<Utc>,
    ) -> Result<Vec<CharacterId>, NpcError> {
        let states = seeds_from_world_bible(bible)?
            .iter()
            .map(|seed| CharacterState::from_seed(session_id, seed, now))
            .collect::<Result<Vec<_>, _>>()?;

        let mut sessions = self.write();
        let session = sessions.entry(session_id).or_default();
        let mut ids: Vec<CharacterId> = Vec::with_capacity(states.len());
        for state in &states {
            let id = state.character_id();
            if session.characters.contains_key(id) || ids.contains(id) {
                return Err(NpcError::DuplicateCharacter(id.to_string()));
            }
            ids.push(id.clone());
        }
        if session.characters.len() + states.len() > MAX_CHARACTERS_PER_SESSION {
            return Err(NpcError::LimitExceeded {
                what: "characters per session",
                max: MAX_CHARACTERS_PER_SESSION,
            });
        }
        for state in states {
            session
                .characters
                .insert(state.character_id().clone(), state);
        }
        Ok(ids)
    }

    /// Snapshot of one NPC.
    pub fn get(&self, session_id: Uuid, character_id: &CharacterId) -> Option<CharacterState> {
        self.read()
            .get(&session_id)?
            .characters
            .get(character_id)
            .cloned()
    }

    /// Snapshots of every NPC of a session, ordered by id.
    pub fn list(&self, session_id: Uuid) -> Vec<CharacterState> {
        self.read()
            .get(&session_id)
            .map(|s| s.characters.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Does this NPC know the fact? Errors if the NPC does not exist, so a
    /// typo can never read as "does not know".
    pub fn knows(
        &self,
        session_id: Uuid,
        character_id: &CharacterId,
        fact_id: &FactId,
    ) -> Result<bool, NpcError> {
        let sessions = self.read();
        let session = sessions
            .get(&session_id)
            .ok_or(NpcError::UnknownSession(session_id))?;
        session
            .characters
            .get(character_id)
            .map(|state| state.knows(fact_id))
            .ok_or_else(|| NpcError::UnknownCharacter(character_id.to_string()))
    }

    /// Mutate one NPC through its validated API (`set_location`,
    /// `learn_fact`, `forget_fact`, ...). The change is committed only if `f`
    /// succeeds.
    pub fn update<R>(
        &self,
        session_id: Uuid,
        character_id: &CharacterId,
        f: impl FnOnce(&mut CharacterState) -> Result<R, NpcError>,
    ) -> Result<R, NpcError> {
        let mut sessions = self.write();
        let slot = sessions
            .get_mut(&session_id)
            .ok_or(NpcError::UnknownSession(session_id))?
            .characters
            .get_mut(character_id)
            .ok_or_else(|| NpcError::UnknownCharacter(character_id.to_string()))?;
        let mut draft = slot.clone();
        let result = f(&mut draft)?;
        *slot = draft;
        Ok(result)
    }

    /// Apply an event to the session's NPCs: only NPCs in the event's audience
    /// are touched. Returns the memories they formed, for the caller to
    /// persist in a [`MemoryStore`] (outside this lock).
    ///
    /// Atomic and replay-safe: a malformed event changes nothing, and an
    /// `event_id` seen recently is rejected with [`NpcError::DuplicateEvent`].
    pub fn apply_event(&self, event: &NpcEvent) -> Result<Vec<MemoryEntry>, NpcError> {
        let mut sessions = self.write();
        let session = sessions
            .get_mut(&event.session_id)
            .ok_or(NpcError::UnknownSession(event.session_id))?;
        if session.applied.contains(&event.event_id) {
            return Err(NpcError::DuplicateEvent(event.event_id));
        }

        let perceptions = perceive(event, &session.characters)?;
        let mut draft = session.characters.clone();
        for perception in &perceptions {
            let state = draft
                .get_mut(&perception.character_id)
                .ok_or_else(|| NpcError::UnknownCharacter(perception.character_id.to_string()))?;
            perception.apply_to(state, event)?;
        }
        // Death is a fact about the world, not about anyone's knowledge: the
        // character dies whether or not anybody saw it.
        // (A `Reported` death is news of an earlier one and kills nobody.)
        if let NpcEventKind::CharacterDied { character, .. } = &event.kind
            && !matches!(event.audience, Audience::Reported { .. })
        {
            draft
                .get_mut(character)
                .ok_or_else(|| NpcError::UnknownCharacter(character.to_string()))?
                .kill(event.timestamp);
        }

        session.characters = draft;
        if session.applied.len() == APPLIED_EVENTS_CAP {
            session.applied.pop_front();
        }
        session.applied.push_back(event.event_id);
        Ok(perceptions.into_iter().map(|p| p.memory).collect())
    }

    /// [`NpcDirectory::apply_event`], then persist the resulting memories.
    ///
    /// State is authoritative and in memory; persistence is best-effort. If
    /// the store fails, the state change stands and the error is returned.
    /// Memory ids are deterministic, so persisting the same memories again
    /// later is safe.
    pub async fn record_event(
        &self,
        store: &dyn MemoryStore,
        event: &NpcEvent,
    ) -> Result<Vec<MemoryEntry>, NpcError> {
        let memories = self.apply_event(event)?;
        for memory in &memories {
            store.store_memory(memory).await?;
        }
        Ok(memories)
    }

    /// Forget a session's NPCs. Returns whether the session existed.
    pub fn remove_session(&self, session_id: Uuid) -> bool {
        self.write().remove(&session_id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::npc::events::death_fact_id;
    use crate::npc::events::test_support::*;
    use crate::npc::ids::{EntityId, LocationId};
    use crate::npc::memory::test_support::{cid, t};
    use crate::npc::state::{KnowledgeSource, LifeStatus};
    use crate::npc::store::test_support::UnavailableStore;
    use crate::npc::store::{InMemoryMemoryStore, StoreError};
    use serde_json::json;

    fn directory() -> NpcDirectory {
        let dir = NpcDirectory::new();
        for (id, name, location) in [
            ("hank", "Hank Schrader", "dea_office"),
            ("walter", "Walter White", "lab"),
            ("jesse", "Jesse Pinkman", "lab"),
        ] {
            dir.insert(npc(id, name, Some(location))).unwrap();
        }
        dir
    }

    fn harmed(n: u64, target: &str, audience: Audience) -> NpcEvent {
        event(
            n,
            NpcEventKind::Harmed {
                actor: EntityId::player(),
                target: cid(target),
            },
            audience,
        )
    }

    #[test]
    fn insert_get_list() {
        let dir = directory();
        assert_eq!(
            dir.get(SESSION, &cid("hank")).unwrap().name(),
            "Hank Schrader"
        );
        assert_eq!(dir.get(SESSION, &cid("nobody")), None);
        assert_eq!(dir.get(Uuid::nil(), &cid("hank")), None);
        let ids: Vec<String> = dir
            .list(SESSION)
            .iter()
            .map(|s| s.character_id().to_string())
            .collect();
        assert_eq!(ids, ["hank", "jesse", "walter"]);
        assert!(dir.list(Uuid::nil()).is_empty());

        assert_eq!(
            dir.insert(npc("hank", "Impostor", None)),
            Err(NpcError::DuplicateCharacter("hank".into()))
        );
    }

    #[test]
    fn sessions_are_isolated() {
        let dir = directory();
        let other = Uuid::from_u128(2);
        let mut twin = npc("hank", "Other Hank", None);
        twin = serde_json::from_value({
            let mut v = serde_json::to_value(&twin).unwrap();
            v["session_id"] = json!(other);
            v
        })
        .unwrap();
        dir.insert(twin).unwrap();

        dir.apply_event(&harmed(1, "hank", Audience::Participants))
            .unwrap();
        assert_ne!(
            dir.get(SESSION, &cid("hank")).unwrap().version(),
            dir.get(other, &cid("hank")).unwrap().version()
        );
        assert!(dir.remove_session(other));
        assert!(!dir.remove_session(other));
        assert!(dir.get(SESSION, &cid("hank")).is_some());
    }

    #[test]
    fn update_commits_only_on_success() {
        let dir = directory();
        let lab = LocationId::new("lab").unwrap();
        dir.update(SESSION, &cid("hank"), |s| {
            s.set_location(Some(lab.clone()), t(1))
        })
        .unwrap();
        assert_eq!(
            dir.get(SESSION, &cid("hank")).unwrap().location(),
            Some(&lab)
        );

        let before = dir.get(SESSION, &cid("hank")).unwrap();
        let result: Result<(), NpcError> = dir.update(SESSION, &cid("hank"), |s| {
            s.set_location(None, t(2))?;
            Err(NpcError::InvalidEvent("abort".into()))
        });
        assert!(result.is_err());
        assert_eq!(dir.get(SESSION, &cid("hank")).unwrap(), before);

        assert!(matches!(
            dir.update(SESSION, &cid("nobody"), |_| Ok(())),
            Err(NpcError::UnknownCharacter(_))
        ));
        assert!(matches!(
            dir.update(Uuid::nil(), &cid("hank"), |_| Ok(())),
            Err(NpcError::UnknownSession(_))
        ));
    }

    #[test]
    fn knows_requires_an_existing_character() {
        let dir = directory();
        let fact = secret_x();
        assert_eq!(dir.knows(SESSION, &cid("hank"), &fact.fact_id), Ok(false));
        dir.update(SESSION, &cid("hank"), |s| {
            s.learn_fact(fact.clone(), KnowledgeSource::Witnessed, None, t(1))
        })
        .unwrap();
        assert_eq!(dir.knows(SESSION, &cid("hank"), &fact.fact_id), Ok(true));
        assert_eq!(dir.knows(SESSION, &cid("walter"), &fact.fact_id), Ok(false));
        assert!(dir.knows(SESSION, &cid("nobody"), &fact.fact_id).is_err());
        assert!(dir.knows(Uuid::nil(), &cid("hank"), &fact.fact_id).is_err());

        // ...and can be made to forget.
        assert!(
            dir.update(SESSION, &cid("hank"), |s| Ok(
                s.forget_fact(&fact.fact_id, t(2))
            ))
            .unwrap()
        );
        assert_eq!(dir.knows(SESSION, &cid("hank"), &fact.fact_id), Ok(false));
    }

    #[test]
    fn apply_event_touches_only_the_audience() {
        let dir = directory();
        let before_hank = dir.get(SESSION, &cid("hank")).unwrap();
        let memories = dir
            .apply_event(&harmed(1, "jesse", witnesses(&["walter"])))
            .unwrap();
        let owners: Vec<&str> = memories.iter().map(|m| m.character_id.as_str()).collect();
        assert_eq!(owners, ["jesse", "walter"]);
        assert_eq!(dir.get(SESSION, &cid("hank")).unwrap(), before_hank);
        assert_eq!(
            dir.get(SESSION, &cid("jesse"))
                .unwrap()
                .relationship(&EntityId::player())
                .trust(),
            -35
        );
    }

    #[test]
    fn replayed_and_malformed_events_change_nothing() {
        let dir = directory();
        let e = harmed(1, "jesse", Audience::Participants);
        dir.apply_event(&e).unwrap();
        let snapshot = dir.list(SESSION);
        assert_eq!(
            dir.apply_event(&e),
            Err(NpcError::DuplicateEvent(e.event_id))
        );
        assert!(matches!(
            dir.apply_event(&harmed(2, "jesse", witnesses(&["nobody"]))),
            Err(NpcError::UnknownCharacter(_))
        ));
        let mut elsewhere = harmed(3, "jesse", Audience::Participants);
        elsewhere.session_id = Uuid::nil();
        assert_eq!(
            dir.apply_event(&elsewhere),
            Err(NpcError::UnknownSession(Uuid::nil()))
        );
        assert_eq!(dir.list(SESSION), snapshot);
        // A rejected event id is not burned.
        dir.apply_event(&harmed(2, "jesse", Audience::Participants))
            .unwrap();
    }

    #[test]
    fn death_is_authoritative_but_knowledge_of_it_is_not() {
        let dir = directory();
        let died = event(
            1,
            NpcEventKind::CharacterDied {
                character: cid("jesse"),
                killer: Some(EntityId::player()),
            },
            witnesses(&["walter"]),
        );
        let memories = dir.apply_event(&died).unwrap();
        assert_eq!(memories.len(), 1);

        let jesse = dir.get(SESSION, &cid("jesse")).unwrap();
        assert_eq!(jesse.status(), LifeStatus::Dead);
        let fact = death_fact_id(&cid("jesse"));
        assert_eq!(dir.knows(SESSION, &cid("walter"), &fact), Ok(true));
        assert_eq!(dir.knows(SESSION, &cid("hank"), &fact), Ok(false));

        // Hank learns it only when somebody tells him.
        let news = event(
            2,
            NpcEventKind::CharacterDied {
                character: cid("jesse"),
                killer: Some(EntityId::player()),
            },
            reported("walter", &["hank"]),
        );
        let memories = dir.apply_event(&news).unwrap();
        assert_eq!(
            memories[0].summary,
            "Walter White told me that the player killed Jesse Pinkman."
        );
        assert_eq!(dir.knows(SESSION, &cid("hank"), &fact), Ok(true));

        // Dying twice is rejected.
        let mut again = died.clone();
        again.event_id = Uuid::from_u128(3);
        assert_eq!(
            dir.apply_event(&again),
            Err(NpcError::CharacterDead("jesse".into()))
        );
    }

    #[test]
    fn seeds_a_session_from_a_world_bible() {
        let dir = NpcDirectory::new();
        let bible = json!({"characters": [
            {"id": "hank_schrader", "name": "Hank Schrader",
             "known_facts": [{"text": "Blue meth is spreading"}]},
            {"id": "walter_white", "name": "Walter White",
             "known_facts": [{"text": "Walter White is Heisenberg"}]}
        ]});
        let ids = dir.seed_from_world_bible(SESSION, &bible, t(0)).unwrap();
        assert_eq!(ids, [cid("hank_schrader"), cid("walter_white")]);
        let secret = crate::npc::state::canon_fact_id("Walter White is Heisenberg");
        assert_eq!(dir.knows(SESSION, &cid("walter_white"), &secret), Ok(true));
        assert_eq!(
            dir.knows(SESSION, &cid("hank_schrader"), &secret),
            Ok(false)
        );

        // Seeding again would duplicate: rejected, nothing changes.
        assert!(matches!(
            dir.seed_from_world_bible(SESSION, &bible, t(1)),
            Err(NpcError::DuplicateCharacter(_))
        ));
        assert_eq!(dir.list(SESSION).len(), 2);
    }

    #[tokio::test]
    async fn record_event_persists_memories() {
        let dir = directory();
        let store = InMemoryMemoryStore::new();
        let memories = dir
            .record_event(&store, &harmed(1, "jesse", witnesses(&["walter"])))
            .await
            .unwrap();
        assert_eq!(memories.len(), 2);
        assert_eq!(store.len(), 2);
        for memory in &memories {
            assert_eq!(
                store.get_memory(memory.memory_id).await.unwrap().as_ref(),
                Some(memory)
            );
        }
    }

    #[tokio::test]
    async fn record_event_reports_storage_errors() {
        let dir = directory();
        let e = harmed(1, "jesse", Audience::Participants);
        let err = dir.record_event(&UnavailableStore, &e).await.unwrap_err();
        assert_eq!(
            err,
            NpcError::Store(StoreError::Unavailable("connection refused".into()))
        );
        // Authoritative state still advanced; the event is not re-applied.
        assert_eq!(
            dir.get(SESSION, &cid("jesse"))
                .unwrap()
                .relationship(&EntityId::player())
                .trust(),
            -35
        );
        assert_eq!(
            dir.apply_event(&e),
            Err(NpcError::DuplicateEvent(e.event_id))
        );
    }
}
