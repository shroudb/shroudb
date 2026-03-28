//! Pipeline idempotency map.
//!
//! Caches pipeline responses by request ID so that retried pipelines
//! return the same response without re-executing. Entries expire after
//! a configurable TTL (default: 5 minutes).

use std::time::Instant;

use dashmap::DashMap;

use crate::resp3::Resp3Frame;

/// Default TTL for idempotency entries: 5 minutes.
const DEFAULT_TTL_SECS: u64 = 300;

/// Thread-safe idempotency map keyed by request ID.
///
/// Caches the serialized RESP3 frame for completed pipeline responses
/// so that retries can replay the exact same wire bytes.
pub struct IdempotencyMap {
    entries: DashMap<String, CachedResponse>,
    ttl_secs: u64,
}

struct CachedResponse {
    frame: Resp3Frame,
    created_at: Instant,
}

impl IdempotencyMap {
    pub fn new() -> Self {
        Self {
            entries: DashMap::new(),
            ttl_secs: DEFAULT_TTL_SECS,
        }
    }

    /// Look up a cached response frame for the given request ID.
    pub fn get(&self, request_id: &str) -> Option<Resp3Frame> {
        let entry = self.entries.get(request_id)?;
        if entry.created_at.elapsed().as_secs() >= self.ttl_secs {
            drop(entry);
            self.entries.remove(request_id);
            return None;
        }
        Some(entry.frame.clone())
    }

    /// Cache a response frame for the given request ID.
    pub fn insert(&self, request_id: String, frame: Resp3Frame) {
        self.entries.insert(
            request_id,
            CachedResponse {
                frame,
                created_at: Instant::now(),
            },
        );
    }

    /// Remove expired entries. Called periodically by the reaper task.
    pub fn prune(&self) {
        self.entries
            .retain(|_, entry| entry.created_at.elapsed().as_secs() < self.ttl_secs);
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the map is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for IdempotencyMap {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get() {
        let map = IdempotencyMap::new();
        let frame = Resp3Frame::SimpleString("OK".into());
        map.insert("req-1".into(), frame);
        assert!(map.get("req-1").is_some());
        assert!(map.get("req-2").is_none());
    }

    #[test]
    fn prune_removes_expired() {
        let map = IdempotencyMap {
            entries: DashMap::new(),
            ttl_secs: 0,
        };
        map.insert("req-1".into(), Resp3Frame::SimpleString("OK".into()));
        // TTL=0 means entries expire immediately
        map.prune();
        assert!(map.is_empty());
    }
}
