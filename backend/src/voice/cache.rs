//! Ephemeral in-memory store for synthesized audio, served by `GET /audio/{id}`.
//!
//! Bounded three ways (entries, total bytes, age) so it cannot grow forever.
//! Nothing here is persisted; a restart drops every clip.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::VoiceAudio;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheLimits {
    pub max_entries: usize,
    /// Total bytes across all clips.
    pub max_bytes: usize,
    /// A clip older than this is gone.
    pub ttl: Duration,
}

impl Default for CacheLimits {
    fn default() -> Self {
        Self {
            max_entries: 32,
            max_bytes: 16 * 1024 * 1024,
            ttl: Duration::from_secs(10 * 60),
        }
    }
}

struct Entry {
    id: Uuid,
    audio: VoiceAudio,
    stored: Instant,
}

#[derive(Default)]
struct Inner {
    /// Oldest first.
    entries: VecDeque<Entry>,
    bytes: usize,
}

impl Inner {
    fn pop_oldest(&mut self) {
        if let Some(entry) = self.entries.pop_front() {
            self.bytes -= entry.audio.bytes.len();
        }
    }

    fn drop_expired(&mut self, ttl: Duration) {
        while self
            .entries
            .front()
            .is_some_and(|entry| entry.stored.elapsed() >= ttl)
        {
            self.pop_oldest();
        }
    }
}

/// Cheap to clone; clones share the clips.
#[derive(Clone, Default)]
pub struct AudioCache {
    inner: Arc<Mutex<Inner>>,
    limits: CacheLimits,
}

impl AudioCache {
    pub fn new(limits: CacheLimits) -> Self {
        Self {
            inner: Arc::default(),
            limits,
        }
    }

    /// Store a clip under a fresh random id, evicting the oldest clips to
    /// stay within the limits. `None` when the clip alone exceeds them.
    pub fn insert(&self, audio: VoiceAudio) -> Option<Uuid> {
        let size = audio.bytes.len();
        if size > self.limits.max_bytes || self.limits.max_entries == 0 {
            return None;
        }
        let mut inner = self.inner.lock().expect("audio cache lock poisoned");
        inner.drop_expired(self.limits.ttl);
        while inner.entries.len() >= self.limits.max_entries
            || inner.bytes + size > self.limits.max_bytes
        {
            inner.pop_oldest();
        }
        let id = Uuid::new_v4();
        inner.bytes += size;
        inner.entries.push_back(Entry {
            id,
            audio,
            stored: Instant::now(),
        });
        Some(id)
    }

    pub fn get(&self, id: Uuid) -> Option<VoiceAudio> {
        let mut inner = self.inner.lock().expect("audio cache lock poisoned");
        inner.drop_expired(self.limits.ttl);
        inner
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.audio.clone())
    }

    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("audio cache lock poisoned")
            .entries
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes currently held.
    pub fn bytes(&self) -> usize {
        self.inner.lock().expect("audio cache lock poisoned").bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(size: usize) -> VoiceAudio {
        VoiceAudio {
            bytes: vec![7; size],
            content_type: "audio/mpeg".into(),
        }
    }

    fn limits(max_entries: usize, max_bytes: usize) -> CacheLimits {
        CacheLimits {
            max_entries,
            max_bytes,
            ttl: Duration::from_secs(60),
        }
    }

    #[test]
    fn stores_and_returns_a_clip() {
        let cache = AudioCache::default();
        let id = cache.insert(clip(10)).unwrap();
        assert_eq!(cache.get(id), Some(clip(10)));
        assert_eq!(cache.get(Uuid::new_v4()), None);
    }

    #[test]
    fn entry_cap_evicts_the_oldest() {
        let cache = AudioCache::new(limits(2, 1000));
        let first = cache.insert(clip(1)).unwrap();
        let second = cache.insert(clip(1)).unwrap();
        let third = cache.insert(clip(1)).unwrap();
        assert_eq!(cache.len(), 2);
        assert!(cache.get(first).is_none());
        assert!(cache.get(second).is_some());
        assert!(cache.get(third).is_some());
    }

    #[test]
    fn byte_cap_evicts_until_the_new_clip_fits() {
        let cache = AudioCache::new(limits(10, 100));
        let a = cache.insert(clip(40)).unwrap();
        let b = cache.insert(clip(40)).unwrap();
        let c = cache.insert(clip(70)).unwrap();
        assert!(cache.get(a).is_none());
        assert!(cache.get(b).is_none());
        assert!(cache.get(c).is_some());
        assert_eq!(cache.bytes(), 70);
    }

    #[test]
    fn a_clip_larger_than_the_cache_is_refused() {
        let cache = AudioCache::new(limits(10, 100));
        let kept = cache.insert(clip(50)).unwrap();
        assert!(cache.insert(clip(101)).is_none());
        assert!(cache.get(kept).is_some());
    }

    #[test]
    fn never_grows_past_its_limits() {
        let cache = AudioCache::new(limits(4, 64));
        for size in (1..40).cycle().take(500) {
            cache.insert(clip(size));
            assert!(cache.len() <= 4);
            assert!(cache.bytes() <= 64);
        }
    }

    #[test]
    fn expired_clips_are_gone() {
        let cache = AudioCache::new(CacheLimits {
            ttl: Duration::ZERO,
            ..CacheLimits::default()
        });
        let id = cache.insert(clip(10)).unwrap();
        assert!(cache.get(id).is_none());
        assert!(cache.is_empty());
        assert_eq!(cache.bytes(), 0);
    }
}
