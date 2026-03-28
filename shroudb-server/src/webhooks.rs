//! Webhook delivery actor.
//!
//! Receives subscription events from the storage engine's broadcast channel
//! and delivers them as HMAC-SHA256 signed HTTP POSTs to configured endpoints.

use std::sync::Arc;
use std::time::Duration;

use ring::hmac;
use shroudb_store::{EventType, SubscriptionEvent};
use tokio::sync::{broadcast, watch};

use crate::config::WebhookConfig;

/// Run a webhook delivery actor for a single endpoint.
///
/// Subscribes to the engine's event broadcast channel and delivers matching
/// events as HTTP POSTs. Retries failed deliveries with exponential backoff.
pub async fn run_webhook(
    config: WebhookConfig,
    mut event_rx: broadcast::Receiver<SubscriptionEvent>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(config.timeout_ms))
        .build()
        .unwrap_or_default();

    let signing_key = hmac::Key::new(hmac::HMAC_SHA256, config.secret.as_bytes());

    tracing::info!(url = %config.url, "webhook actor started");

    loop {
        tokio::select! {
            biased;

            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    tracing::debug!(url = %config.url, "webhook actor shutting down");
                    break;
                }
            }

            result = event_rx.recv() => {
                match result {
                    Ok(event) => {
                        if !matches_filter(&config, &event) {
                            continue;
                        }
                        deliver_with_retry(&client, &config, &signing_key, &event).await;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            url = %config.url,
                            lagged = n,
                            "webhook receiver lagged, some events were dropped"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::debug!(url = %config.url, "event channel closed");
                        break;
                    }
                }
            }
        }
    }
}

/// Check whether an event matches the webhook's namespace and event type filters.
fn matches_filter(config: &WebhookConfig, event: &SubscriptionEvent) -> bool {
    // Event type filter
    if !config.events.is_empty() {
        let event_str = match event.event {
            EventType::Put => "put",
            EventType::Delete => "delete",
        };
        if !config
            .events
            .iter()
            .any(|e| e.eq_ignore_ascii_case(event_str))
        {
            return false;
        }
    }

    // Namespace filter
    if !config.namespaces.is_empty() {
        let matched = config.namespaces.iter().any(|pattern| {
            if let Some(prefix) = pattern.strip_suffix('*') {
                event.namespace.starts_with(prefix)
            } else {
                event.namespace == *pattern
            }
        });
        if !matched {
            return false;
        }
    }

    true
}

/// Deliver an event with exponential backoff retry.
async fn deliver_with_retry(
    client: &reqwest::Client,
    config: &WebhookConfig,
    signing_key: &hmac::Key,
    event: &SubscriptionEvent,
) {
    let body = match serde_json::to_vec(event) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, "failed to serialize webhook event");
            return;
        }
    };

    let signature = hmac::sign(signing_key, &body);
    let signature_hex = hex::encode(signature.as_ref());

    for attempt in 0..=config.max_retries {
        let result = client
            .post(&config.url)
            .header("Content-Type", "application/json")
            .header(
                "X-ShrouDB-Signature-256",
                &format!("sha256={signature_hex}"),
            )
            .header("X-ShrouDB-Event", event_type_str(event.event))
            .body(body.clone())
            .send()
            .await;

        match result {
            Ok(resp) if resp.status().is_success() => {
                tracing::debug!(
                    url = %config.url,
                    status = resp.status().as_u16(),
                    event = event_type_str(event.event),
                    namespace = %event.namespace,
                    "webhook delivered"
                );
                return;
            }
            Ok(resp) => {
                tracing::warn!(
                    url = %config.url,
                    status = resp.status().as_u16(),
                    attempt = attempt + 1,
                    max_retries = config.max_retries,
                    "webhook delivery got non-success status"
                );
            }
            Err(e) => {
                tracing::warn!(
                    url = %config.url,
                    error = %e,
                    attempt = attempt + 1,
                    max_retries = config.max_retries,
                    "webhook delivery failed"
                );
            }
        }

        if attempt < config.max_retries {
            let backoff = Duration::from_secs(1 << attempt.min(3)); // 1s, 2s, 4s, 8s
            tokio::time::sleep(backoff).await;
        }
    }

    tracing::error!(
        url = %config.url,
        event = event_type_str(event.event),
        namespace = %event.namespace,
        "webhook delivery exhausted all retries"
    );
}

fn event_type_str(event: EventType) -> &'static str {
    match event {
        EventType::Put => "put",
        EventType::Delete => "delete",
    }
}

/// Spawn webhook actors for all configured endpoints.
///
/// Returns task handles for graceful shutdown.
pub fn spawn_all(
    configs: Vec<WebhookConfig>,
    engine: &Arc<shroudb_storage::StorageEngine>,
    shutdown_rx: &watch::Receiver<bool>,
) -> Vec<tokio::task::JoinHandle<()>> {
    configs
        .into_iter()
        .map(|config| {
            let rx = engine.event_subscribe();
            let shutdown = shutdown_rx.clone();
            tokio::spawn(run_webhook(config, rx, shutdown))
        })
        .collect()
}
