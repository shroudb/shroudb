//! Webhook notification infrastructure for ShrouDB.
//!
//! Provides HMAC-signed webhook event dispatch with configurable retry logic,
//! HTTP delivery with exponential backoff, event filtering, and an async
//! event queue.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use shroudb_crypto::{HmacAlgorithm, hmac_sign};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Webhook endpoint configuration for a keyspace.
#[derive(Debug, Clone, Deserialize)]
pub struct WebhookConfig {
    /// Target URL to deliver webhook events to.
    pub url: String,
    /// HMAC secret used to sign the payload (hex-encoded).
    pub secret: String,
    /// Event types to subscribe to: "rotate", "family_revoked", "reuse_detected",
    /// "issue", "revoke", "suspend", "unsuspend".
    #[serde(default)]
    pub events: Vec<String>,
    /// Maximum delivery retry attempts before giving up.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

fn default_max_retries() -> u32 {
    3
}

// ---------------------------------------------------------------------------
// Event types
// ---------------------------------------------------------------------------

/// Well-known webhook event types.
pub mod event_types {
    pub const ROTATE: &str = "rotate";
    pub const FAMILY_REVOKED: &str = "family_revoked";
    pub const REUSE_DETECTED: &str = "reuse_detected";
    pub const ISSUE: &str = "issue";
    pub const REVOKE: &str = "revoke";
    pub const SUSPEND: &str = "suspend";
    pub const UNSUSPEND: &str = "unsuspend";
}

/// Webhook event payload.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookEvent {
    /// Event type (one of the `event_types` constants).
    pub event_type: String,
    /// Keyspace the event originated from.
    pub keyspace: String,
    /// Unix timestamp of when the event was generated.
    pub timestamp: u64,
    /// Event-specific data.
    pub data: serde_json::Value,
}

/// A signed webhook payload ready for delivery.
#[derive(Debug, Clone)]
pub struct SignedPayload {
    /// The JSON-serialized event body.
    pub body: Vec<u8>,
    /// The hex-encoded HMAC-SHA256 signature over `body`.
    pub signature: String,
    /// Target URL.
    pub url: String,
    /// Maximum retries for delivery.
    pub max_retries: u32,
}

// ---------------------------------------------------------------------------
// HMAC signing
// ---------------------------------------------------------------------------

/// Compute the HMAC-SHA256 signature over a JSON payload.
///
/// The signature is returned as a hex-encoded string, suitable for inclusion
/// in the `X-ShrouDB-Signature` HTTP header.
pub fn sign_payload(secret: &[u8], payload: &[u8]) -> String {
    let signature = hmac_sign(HmacAlgorithm::Sha256, secret, payload)
        .expect("HMAC-SHA256 signing should never fail");
    hex::encode(signature)
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/// Webhook dispatcher. Queues events for asynchronous delivery.
pub struct WebhookDispatcher {
    tx: mpsc::Sender<WebhookEvent>,
    configs: Vec<WebhookConfig>,
}

impl WebhookDispatcher {
    /// Create a new dispatcher with the given webhook configurations.
    ///
    /// Returns the dispatcher and a receiver for the background delivery loop.
    pub fn new(configs: Vec<WebhookConfig>) -> (Self, mpsc::Receiver<WebhookEvent>) {
        let (tx, rx) = mpsc::channel(1000);
        (Self { tx, configs }, rx)
    }

    /// Queue a webhook event for delivery.
    ///
    /// This is non-blocking. If the channel is full, the event is dropped
    /// with a tracing warning.
    pub fn send(&self, event: WebhookEvent) {
        if self.tx.try_send(event).is_err() {
            tracing::warn!(
                target: "shroudb::webhook",
                "webhook event queue full, dropping event"
            );
        }
    }

    /// Build a `WebhookEvent` and queue it if any webhook config subscribes to this event type.
    pub fn notify(&self, event_type: &str, keyspace: &str, data: serde_json::Value) {
        // Check if any config is interested in this event type.
        let interested = self
            .configs
            .iter()
            .any(|c| c.events.is_empty() || c.events.iter().any(|e| e == event_type));

        if !interested {
            return;
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.send(WebhookEvent {
            event_type: event_type.to_string(),
            keyspace: keyspace.to_string(),
            timestamp,
            data,
        });
    }

    /// Sign an event payload against all matching webhook configs.
    ///
    /// Returns a list of signed payloads ready for HTTP delivery.
    pub fn prepare_deliveries(&self, event: &WebhookEvent) -> Vec<SignedPayload> {
        let body = serde_json::to_vec(event).expect("webhook event serialization should not fail");

        self.configs
            .iter()
            .filter(|c| c.events.is_empty() || c.events.iter().any(|e| e == &event.event_type))
            .map(|config| {
                let secret = config.secret.as_bytes();
                let signature = sign_payload(secret, &body);
                SignedPayload {
                    body: body.clone(),
                    signature,
                    url: config.url.clone(),
                    max_retries: config.max_retries,
                }
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Background delivery loop
// ---------------------------------------------------------------------------

/// Background task that processes the webhook event queue and delivers
/// signed payloads via HTTP POST with exponential-backoff retries.
pub async fn webhook_delivery_loop(
    mut rx: mpsc::Receiver<WebhookEvent>,
    configs: Vec<WebhookConfig>,
) {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("failed to build HTTP client");

    // Build a dispatcher just for signing (we already have the configs).
    let (dispatcher, _unused_rx) = WebhookDispatcher::new(configs);

    while let Some(event) = rx.recv().await {
        let deliveries = dispatcher.prepare_deliveries(&event);

        for delivery in &deliveries {
            let mut last_err = None;
            for attempt in 0..=delivery.max_retries {
                if attempt > 0 {
                    let backoff = std::time::Duration::from_secs(1 << (attempt - 1).min(5));
                    tokio::time::sleep(backoff).await;
                }
                match client
                    .post(&delivery.url)
                    .header("Content-Type", "application/json")
                    .header("X-ShrouDB-Signature", &delivery.signature)
                    .body(delivery.body.clone())
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        tracing::info!(
                            target: "shroudb::webhook",
                            url = %delivery.url,
                            event_type = %event.event_type,
                            "webhook delivered"
                        );
                        last_err = None;
                        break;
                    }
                    Ok(resp) => {
                        last_err = Some(format!("HTTP {}", resp.status()));
                    }
                    Err(e) => {
                        last_err = Some(e.to_string());
                    }
                }
            }
            if let Some(err) = last_err {
                tracing::warn!(
                    target: "shroudb::webhook",
                    url = %delivery.url,
                    error = %err,
                    "webhook delivery failed after retries"
                );
            }
        }

        if deliveries.is_empty() {
            tracing::debug!(
                target: "shroudb::webhook",
                event_type = %event.event_type,
                keyspace = %event.keyspace,
                "no webhook configs matched this event"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_payload_produces_hex() {
        let sig = sign_payload(b"test-secret", b"hello world");
        // Should be a valid hex string (64 chars for SHA-256).
        assert_eq!(sig.len(), 64);
        assert!(sig.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn sign_payload_deterministic() {
        let sig1 = sign_payload(b"secret", b"payload");
        let sig2 = sign_payload(b"secret", b"payload");
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn sign_payload_different_secrets_differ() {
        let sig1 = sign_payload(b"secret-a", b"payload");
        let sig2 = sign_payload(b"secret-b", b"payload");
        assert_ne!(sig1, sig2);
    }

    #[test]
    fn prepare_deliveries_filters_by_event_type() {
        let configs = vec![
            WebhookConfig {
                url: "http://a.example.com/hook".to_string(),
                secret: "secret-a".to_string(),
                events: vec!["rotate".to_string()],
                max_retries: 3,
            },
            WebhookConfig {
                url: "http://b.example.com/hook".to_string(),
                secret: "secret-b".to_string(),
                events: vec!["revoke".to_string()],
                max_retries: 3,
            },
        ];

        let (dispatcher, _rx) = WebhookDispatcher::new(configs);

        let event = WebhookEvent {
            event_type: "rotate".to_string(),
            keyspace: "tokens".to_string(),
            timestamp: 1234567890,
            data: serde_json::json!({"key_id": "k1"}),
        };

        let deliveries = dispatcher.prepare_deliveries(&event);
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].url, "http://a.example.com/hook");
    }

    #[test]
    fn prepare_deliveries_empty_events_matches_all() {
        let configs = vec![WebhookConfig {
            url: "http://catch-all.example.com/hook".to_string(),
            secret: "secret".to_string(),
            events: vec![], // empty = all events
            max_retries: 3,
        }];

        let (dispatcher, _rx) = WebhookDispatcher::new(configs);

        let event = WebhookEvent {
            event_type: "reuse_detected".to_string(),
            keyspace: "sessions".to_string(),
            timestamp: 1234567890,
            data: serde_json::json!({}),
        };

        let deliveries = dispatcher.prepare_deliveries(&event);
        assert_eq!(deliveries.len(), 1);
    }
}
