//! `MemoryStore`: the storage abstraction for NPC episodic memory.
//!
//! The trait is async and object-safe (`Arc<dyn MemoryStore>`), so the same
//! call sites work against the in-process [`InMemoryMemoryStore`] and a remote
//! database (see [`super::tidb`]). Stores are *outside* the latency-critical
//! gameplay path: callers await them from async tasks, never while holding a
//! session lock.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::RwLock;

use uuid::Uuid;

use super::ids::CharacterId;
use super::memory::{
    MemoryEntry, MemoryQuery, ScoredMemory, clamp_limit, rank_memories, recency_order,
};

/// Default cap on memories kept per NPC by [`InMemoryMemoryStore`].
pub const DEFAULT_MEMORIES_PER_CHARACTER: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// The entry failed validation and was not stored.
    #[error("invalid memory entry: {0}")]
    InvalidEntry(String),
    /// `memory_id` already exists for a different session or character.
    #[error("memory {0} already belongs to another character or session")]
    Conflict(Uuid),
    /// The backing store could not be reached. Safe to retry.
    #[error("memory store unavailable: {0}")]
    Unavailable(String),
    /// The backing store rejected the operation (e.g. a SQL error).
    #[error("memory store error: {0}")]
    Backend(String),
    /// The backing store returned data that could not be decoded.
    #[error("memory store returned corrupt data: {0}")]
    Corrupt(String),
}

pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, StoreError>> + Send + 'a>>;

pub trait MemoryStore: Send + Sync {
    /// Validate and persist a memory. Idempotent for the same `memory_id`
    /// (the entry is replaced); a `memory_id` owned by another character or
    /// session is a [`StoreError::Conflict`].
    fn store_memory<'a>(&'a self, entry: &'a MemoryEntry) -> StoreFuture<'a, ()>;

    /// Fetch one memory by id, including invalidated ones.
    fn get_memory(&self, memory_id: Uuid) -> StoreFuture<'_, Option<MemoryEntry>>;

    /// The NPC's newest valid memories, newest first. At most
    /// [`super::memory::MAX_QUERY_LIMIT`] are returned whatever `limit` says.
    fn query_recent<'a>(
        &'a self,
        session_id: Uuid,
        character_id: &'a CharacterId,
        limit: usize,
    ) -> StoreFuture<'a, Vec<MemoryEntry>>;

    /// The NPC's valid memories most relevant to `query`, best first. Bounded
    /// the same way as [`MemoryStore::query_recent`].
    fn query_relevant<'a>(&'a self, query: &'a MemoryQuery) -> StoreFuture<'a, Vec<ScoredMemory>>;

    /// Mark a memory invalid so it is never retrieved again. Returns whether
    /// a valid memory with that id existed.
    fn invalidate_memory(&self, memory_id: Uuid) -> StoreFuture<'_, bool>;
}

type OwnerKey = (Uuid, CharacterId);

#[derive(Debug, Default)]
struct Inner {
    by_owner: HashMap<OwnerKey, Vec<MemoryEntry>>,
    owner_of: HashMap<Uuid, OwnerKey>,
}

/// Process-local store used by tests and by sessions running without TiDB.
///
/// Bounded: each NPC keeps at most `capacity` memories; when full, the least
/// important (then oldest) memory is evicted.
#[derive(Debug)]
pub struct InMemoryMemoryStore {
    inner: RwLock<Inner>,
    capacity: usize,
}

impl Default for InMemoryMemoryStore {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_MEMORIES_PER_CHARACTER)
    }
}

impl InMemoryMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// A store keeping at most `capacity` (minimum 1) memories per NPC.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: RwLock::new(Inner::default()),
            capacity: capacity.max(1),
        }
    }

    /// Total memories held, including invalidated ones.
    pub fn len(&self) -> usize {
        self.read().owner_of.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drop every memory of a session (e.g. when the session ends).
    pub fn remove_session(&self, session_id: Uuid) {
        let mut inner = self.write();
        inner.by_owner.retain(|(sid, _), _| *sid != session_id);
        inner.owner_of.retain(|_, (sid, _)| *sid != session_id);
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().expect("memory store lock poisoned")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().expect("memory store lock poisoned")
    }

    fn store_sync(&self, entry: &MemoryEntry) -> Result<(), StoreError> {
        entry
            .validate()
            .map_err(|e| StoreError::InvalidEntry(e.to_string()))?;
        let key: OwnerKey = (entry.session_id, entry.character_id.clone());
        let mut inner = self.write();
        match inner.owner_of.get(&entry.memory_id) {
            Some(owner) if *owner != key => return Err(StoreError::Conflict(entry.memory_id)),
            Some(_) => {
                let memories = inner.by_owner.entry(key).or_default();
                if let Some(slot) = memories.iter_mut().find(|m| m.memory_id == entry.memory_id) {
                    *slot = entry.clone();
                }
                return Ok(());
            }
            None => {}
        }

        let capacity = self.capacity;
        let memories = inner.by_owner.entry(key.clone()).or_default();
        memories.push(entry.clone());
        let evicted = if memories.len() > capacity {
            let (index, _) = memories
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    a.importance
                        .cmp(&b.importance)
                        .then_with(|| recency_order(b, a))
                })
                .expect("non-empty after push");
            Some(memories.remove(index).memory_id)
        } else {
            None
        };
        inner.owner_of.insert(entry.memory_id, key);
        if let Some(evicted) = evicted {
            inner.owner_of.remove(&evicted);
        }
        Ok(())
    }

    fn get_sync(&self, memory_id: Uuid) -> Option<MemoryEntry> {
        let inner = self.read();
        let owner = inner.owner_of.get(&memory_id)?;
        inner
            .by_owner
            .get(owner)?
            .iter()
            .find(|m| m.memory_id == memory_id)
            .cloned()
    }

    fn valid_memories(&self, session_id: Uuid, character_id: &CharacterId) -> Vec<MemoryEntry> {
        self.read()
            .by_owner
            .get(&(session_id, character_id.clone()))
            .map(|memories| memories.iter().filter(|m| m.valid).cloned().collect())
            .unwrap_or_default()
    }

    fn invalidate_sync(&self, memory_id: Uuid) -> bool {
        let mut inner = self.write();
        let Some(owner) = inner.owner_of.get(&memory_id).cloned() else {
            return false;
        };
        inner
            .by_owner
            .get_mut(&owner)
            .and_then(|memories| memories.iter_mut().find(|m| m.memory_id == memory_id))
            .is_some_and(|m| std::mem::replace(&mut m.valid, false))
    }
}

impl MemoryStore for InMemoryMemoryStore {
    fn store_memory<'a>(&'a self, entry: &'a MemoryEntry) -> StoreFuture<'a, ()> {
        Box::pin(std::future::ready(self.store_sync(entry)))
    }

    fn get_memory(&self, memory_id: Uuid) -> StoreFuture<'_, Option<MemoryEntry>> {
        Box::pin(std::future::ready(Ok(self.get_sync(memory_id))))
    }

    fn query_recent<'a>(
        &'a self,
        session_id: Uuid,
        character_id: &'a CharacterId,
        limit: usize,
    ) -> StoreFuture<'a, Vec<MemoryEntry>> {
        let mut memories = self.valid_memories(session_id, character_id);
        memories.sort_by(recency_order);
        memories.truncate(clamp_limit(limit));
        Box::pin(std::future::ready(Ok(memories)))
    }

    fn query_relevant<'a>(&'a self, query: &'a MemoryQuery) -> StoreFuture<'a, Vec<ScoredMemory>> {
        let candidates = self.valid_memories(query.session_id, &query.character_id);
        Box::pin(std::future::ready(Ok(rank_memories(candidates, query))))
    }

    fn invalidate_memory(&self, memory_id: Uuid) -> StoreFuture<'_, bool> {
        Box::pin(std::future::ready(Ok(self.invalidate_sync(memory_id))))
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A store whose every operation fails, for error-path tests.
    #[derive(Debug, Default)]
    pub struct UnavailableStore;

    fn down<T: Send + 'static>() -> StoreFuture<'static, T> {
        Box::pin(std::future::ready(Err(StoreError::Unavailable(
            "connection refused".into(),
        ))))
    }

    impl MemoryStore for UnavailableStore {
        fn store_memory<'a>(&'a self, _: &'a MemoryEntry) -> StoreFuture<'a, ()> {
            down()
        }
        fn get_memory(&self, _: Uuid) -> StoreFuture<'_, Option<MemoryEntry>> {
            down()
        }
        fn query_recent<'a>(
            &'a self,
            _: Uuid,
            _: &'a CharacterId,
            _: usize,
        ) -> StoreFuture<'a, Vec<MemoryEntry>> {
            down()
        }
        fn query_relevant<'a>(&'a self, _: &'a MemoryQuery) -> StoreFuture<'a, Vec<ScoredMemory>> {
            down()
        }
        fn invalidate_memory(&self, _: Uuid) -> StoreFuture<'_, bool> {
            down()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::memory::MAX_QUERY_LIMIT;
    use super::super::memory::test_support::*;
    use super::test_support::UnavailableStore;
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn stores_and_gets_memories() {
        let store = InMemoryMemoryStore::new();
        assert!(store.is_empty());
        let m = memory("hank", 1, "The player helped me.", 60);
        store.store_memory(&m).await.unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store.get_memory(m.memory_id).await.unwrap(), Some(m));
        assert_eq!(store.get_memory(Uuid::from_u128(404)).await.unwrap(), None);
    }

    #[tokio::test]
    async fn storing_is_idempotent_but_ids_cannot_change_owner() {
        let store = InMemoryMemoryStore::new();
        let mut m = memory("hank", 1, "First wording.", 60);
        store.store_memory(&m).await.unwrap();
        m.summary = "Second wording.".into();
        store.store_memory(&m).await.unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(
            store
                .get_memory(m.memory_id)
                .await
                .unwrap()
                .unwrap()
                .summary,
            "Second wording."
        );

        let mut stolen = memory("walter", 1, "Not mine.", 60);
        stolen.memory_id = m.memory_id;
        assert_eq!(
            store.store_memory(&stolen).await,
            Err(StoreError::Conflict(m.memory_id))
        );
        assert!(
            store
                .query_recent(Uuid::nil(), &cid("walter"), 5)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn rejects_invalid_entries() {
        let store = InMemoryMemoryStore::new();
        let mut m = memory("hank", 1, "ok", 60);
        m.importance = 250;
        assert!(matches!(
            store.store_memory(&m).await,
            Err(StoreError::InvalidEntry(_))
        ));
        assert!(store.is_empty());
    }

    #[tokio::test]
    async fn recent_is_newest_first_bounded_and_per_character() {
        let store = InMemoryMemoryStore::new();
        for n in 1..=25 {
            store
                .store_memory(&memory("hank", n, &format!("Event {n}."), 10))
                .await
                .unwrap();
        }
        store
            .store_memory(&memory("walter", 100, "Walter only.", 10))
            .await
            .unwrap();

        let recent = store
            .query_recent(Uuid::nil(), &cid("hank"), 3)
            .await
            .unwrap();
        let times: Vec<u64> = recent.iter().map(|m| m.world_time).collect();
        assert_eq!(times, [25, 24, 23]);

        let capped = store
            .query_recent(Uuid::nil(), &cid("hank"), 1_000)
            .await
            .unwrap();
        assert_eq!(capped.len(), MAX_QUERY_LIMIT);
        assert!(capped.iter().all(|m| m.character_id == cid("hank")));

        assert!(
            store
                .query_recent(Uuid::from_u128(1), &cid("hank"), 5)
                .await
                .unwrap()
                .is_empty(),
            "other sessions see nothing"
        );
        assert!(
            store
                .query_recent(Uuid::nil(), &cid("nobody"), 5)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn relevant_ranks_by_query_and_is_bounded() {
        let store = InMemoryMemoryStore::new();
        for n in 1..=40 {
            store
                .store_memory(&memory("hank", n, &format!("Paperwork day {n}."), 20))
                .await
                .unwrap();
        }
        let mut threat = memory("hank", 5, "The player threatened Jesse at the lab.", 20);
        threat.memory_id = Uuid::from_u128(500);
        threat.entities.insert(eid("jesse"));
        store.store_memory(&threat).await.unwrap();
        let mut walters = memory("walter", 41, "Jesse and the lab, all mine.", 100);
        walters.memory_id = Uuid::from_u128(501);
        walters.entities.insert(eid("jesse"));
        store.store_memory(&walters).await.unwrap();

        let query = MemoryQuery::new(Uuid::nil(), cid("hank"))
            .with_entity(eid("jesse"))
            .with_text("what happened at the lab");
        let hits = store.query_relevant(&query).await.unwrap();
        assert_eq!(hits.len(), 5);
        assert_eq!(hits[0].memory.memory_id, threat.memory_id);
        assert!(hits[0].score > hits[1].score);
        assert!(hits.iter().all(|h| h.memory.character_id == cid("hank")));
        assert_eq!(store.query_relevant(&query).await.unwrap(), hits);

        let all = store
            .query_relevant(&query.clone().with_limit(usize::MAX))
            .await
            .unwrap();
        assert_eq!(all.len(), MAX_QUERY_LIMIT);
    }

    #[tokio::test]
    async fn invalidated_memories_are_not_retrieved() {
        let store = InMemoryMemoryStore::new();
        let m = memory("hank", 1, "A lie I was told.", 90);
        store.store_memory(&m).await.unwrap();
        assert!(store.invalidate_memory(m.memory_id).await.unwrap());
        assert!(!store.invalidate_memory(m.memory_id).await.unwrap());
        assert!(!store.invalidate_memory(Uuid::from_u128(404)).await.unwrap());

        assert!(
            store
                .query_recent(Uuid::nil(), &cid("hank"), 5)
                .await
                .unwrap()
                .is_empty()
        );
        let query = MemoryQuery::new(Uuid::nil(), cid("hank"));
        assert!(store.query_relevant(&query).await.unwrap().is_empty());
        let kept = store.get_memory(m.memory_id).await.unwrap().unwrap();
        assert!(!kept.valid, "kept for audit, flagged invalid");
    }

    #[tokio::test]
    async fn capacity_evicts_least_important_then_oldest() {
        let store = InMemoryMemoryStore::with_capacity(3);
        store
            .store_memory(&memory("hank", 1, "Old trivia.", 5))
            .await
            .unwrap();
        store
            .store_memory(&memory("hank", 2, "Trauma.", 95))
            .await
            .unwrap();
        store
            .store_memory(&memory("hank", 3, "New trivia.", 5))
            .await
            .unwrap();
        store
            .store_memory(&memory("hank", 4, "Useful.", 50))
            .await
            .unwrap();
        // Another NPC has its own budget.
        store
            .store_memory(&memory("walter", 5, "Mine.", 1))
            .await
            .unwrap();

        assert_eq!(store.len(), 4);
        assert_eq!(store.get_memory(Uuid::from_u128(1)).await.unwrap(), None);
        let times: Vec<u64> = store
            .query_recent(Uuid::nil(), &cid("hank"), 10)
            .await
            .unwrap()
            .iter()
            .map(|m| m.world_time)
            .collect();
        assert_eq!(times, [4, 3, 2]);
    }

    #[tokio::test]
    async fn remove_session_drops_only_that_session() {
        let store = InMemoryMemoryStore::new();
        let keep = memory("hank", 1, "Keep.", 10);
        let mut gone = memory("hank", 2, "Gone.", 10);
        gone.session_id = Uuid::from_u128(77);
        store.store_memory(&keep).await.unwrap();
        store.store_memory(&gone).await.unwrap();
        store.remove_session(gone.session_id);
        assert_eq!(store.len(), 1);
        assert_eq!(store.get_memory(gone.memory_id).await.unwrap(), None);
        assert!(store.get_memory(keep.memory_id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn storage_errors_surface_through_the_trait_object() {
        let store: Arc<dyn MemoryStore> = Arc::new(UnavailableStore);
        let m = memory("hank", 1, "x", 1);
        let unavailable = StoreError::Unavailable("connection refused".into());
        assert_eq!(store.store_memory(&m).await, Err(unavailable.clone()));
        assert_eq!(
            store.get_memory(m.memory_id).await,
            Err(unavailable.clone())
        );
        assert_eq!(
            store.query_recent(Uuid::nil(), &cid("hank"), 5).await,
            Err(unavailable.clone())
        );
        assert_eq!(
            store
                .query_relevant(&MemoryQuery::new(Uuid::nil(), cid("hank")))
                .await,
            Err(unavailable.clone())
        );
        assert_eq!(store.invalidate_memory(m.memory_id).await, Err(unavailable));

        // The same call sites work against the real in-memory store.
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        store.store_memory(&m).await.unwrap();
    }
}
