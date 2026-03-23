use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::response::ResponseMap;

/// Short-lived dedup cache for idempotency keys on ISSUE commands.
pub struct IdempotencyMap {
    inner: Mutex<HashMap<String, (Instant, ResponseMap)>>,
    ttl: Duration,
}

impl IdempotencyMap {
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    /// Check if an idempotency key has a cached response.
    pub async fn check(&self, key: &str) -> Option<ResponseMap> {
        let inner = self.inner.lock().await;
        if let Some((created_at, response)) = inner.get(key)
            && created_at.elapsed() < self.ttl
        {
            return Some(response.clone());
        }
        None
    }

    /// Cache a response for an idempotency key.
    pub async fn insert(&self, key: String, response: ResponseMap) {
        let mut inner = self.inner.lock().await;
        inner.insert(key, (Instant::now(), response));
    }

    /// Remove entries whose TTL has expired. Returns count pruned.
    pub async fn prune_expired(&self) -> usize {
        let mut inner = self.inner.lock().await;
        let before = inner.len();
        let ttl = self.ttl;
        inner.retain(|_, (created_at, _)| created_at.elapsed() < ttl);
        before - inner.len()
    }
}

impl Default for IdempotencyMap {
    fn default() -> Self {
        Self::new(Duration::from_secs(300)) // 5 minutes
    }
}
