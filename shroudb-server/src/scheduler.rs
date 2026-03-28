use std::sync::Arc;
use std::time::Duration;

use shroudb_storage::StorageEngine;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Spawn all background tasks. Returns handles for shutdown.
pub fn spawn_all(
    engine: Arc<StorageEngine>,
    shutdown_rx: watch::Receiver<bool>,
) -> Vec<JoinHandle<()>> {
    let mut handles = Vec::new();

    // Snapshot compactor (60s interval)
    {
        let engine = Arc::clone(&engine);
        let mut rx = shutdown_rx.clone();
        handles.push(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                tokio::select! {
                    biased;
                    _ = rx.changed() => {
                        if *rx.borrow() { break; }
                    }
                    _ = interval.tick() => {
                        let entries = engine.total_entries_since_snapshot().await;
                        let time = engine.time_since_last_snapshot();
                        let threshold_entries = engine.snapshot_entry_threshold();
                        let threshold_secs = engine.snapshot_time_threshold_secs();

                        if (entries >= threshold_entries
                            || time.as_secs() >= threshold_secs)
                            && let Err(e) = engine.snapshot().await
                        {
                            tracing::error!(error = %e, "snapshot compactor failed");
                        }
                    }
                }
            }
        }));
    }

    // WAL fsync batcher (if not PerWrite mode)
    if let Some(interval_ms) = engine.fsync_interval_ms() {
        let engine = Arc::clone(&engine);
        let mut rx = shutdown_rx.clone();
        handles.push(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(interval_ms));
            loop {
                tokio::select! {
                    biased;
                    _ = rx.changed() => {
                        if *rx.borrow() { break; }
                    }
                    _ = interval.tick() => {
                        if let Err(e) = engine.flush_wal().await {
                            tracing::error!(error = %e, "WAL fsync batcher failed");
                        }
                    }
                }
            }
        }));
    }

    // Metrics reporter (30s interval)
    {
        let engine = Arc::clone(&engine);
        let mut rx = shutdown_rx.clone();
        handles.push(tokio::spawn(async move {
            let start = std::time::Instant::now();
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                tokio::select! {
                    biased;
                    _ = rx.changed() => {
                        if *rx.borrow() { break; }
                    }
                    _ = interval.tick() => {
                        metrics::gauge!("shroudb_uptime_seconds")
                            .set(start.elapsed().as_secs_f64());

                        let entries = engine.total_entries_since_snapshot().await;
                        metrics::gauge!("shroudb_wal_entries_since_snapshot")
                            .set(entries as f64);

                        let ns_count = engine.index().namespaces.len();
                        metrics::gauge!("shroudb_namespace_count")
                            .set(ns_count as f64);

                        let total_keys: u64 = engine
                            .index()
                            .namespaces
                            .iter()
                            .map(|ns| ns.value().active_key_count())
                            .sum();
                        metrics::gauge!("shroudb_active_keys_total")
                            .set(total_keys as f64);
                    }
                }
            }
        }));
    }

    handles
}
