//! Pub/sub event bus for lifecycle events.
//!
//! Commands like REVOKE, ROTATE, and REFRESH publish lifecycle events.
//! SUBSCRIBE consumers receive them via a `tokio::sync::broadcast` channel.

use serde::Serialize;
use tokio::sync::broadcast;

/// A lifecycle event published by command handlers.
#[derive(Debug, Clone, Serialize)]
pub struct LifecycleEvent {
    /// Event type: `"rotation"`, `"revocation"`, `"reuse_detected"`, `"family_revoked"`.
    pub event_type: String,
    /// The keyspace that generated the event.
    pub keyspace: String,
    /// Event-specific detail (credential ID, key ID, family ID, etc.).
    pub detail: String,
    /// Unix timestamp of when the event was generated.
    pub timestamp: u64,
}

/// Broadcast-based event bus for lifecycle events.
pub struct EventBus {
    tx: broadcast::Sender<LifecycleEvent>,
}

impl EventBus {
    /// Create a new event bus with the given channel capacity.
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Publish a lifecycle event. If there are no subscribers the event is silently dropped.
    pub fn publish(&self, event: LifecycleEvent) {
        let _ = self.tx.send(event);
    }

    /// Subscribe to the event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<LifecycleEvent> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn publish_and_receive() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();

        bus.publish(LifecycleEvent {
            event_type: "rotation".into(),
            keyspace: "tokens".into(),
            detail: "k1".into(),
            timestamp: 1234567890,
        });

        let event = rx.recv().await.unwrap();
        assert_eq!(event.event_type, "rotation");
        assert_eq!(event.keyspace, "tokens");
        assert_eq!(event.detail, "k1");
    }

    #[tokio::test]
    async fn no_subscribers_does_not_panic() {
        let bus = EventBus::new(16);
        // Publishing with no subscribers should not panic.
        bus.publish(LifecycleEvent {
            event_type: "revocation".into(),
            keyspace: "sessions".into(),
            detail: "cred_123".into(),
            timestamp: 0,
        });
    }
}
