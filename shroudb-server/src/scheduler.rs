use std::sync::Arc;
use std::time::Duration;

use shroudb_protocol::CommandDispatcher;
use shroudb_storage::StorageEngine;
use shroudb_store::Store;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Spawn all background tasks. Returns handles for shutdown.
pub fn spawn_all<S: Store + 'static>(
    engine: Arc<StorageEngine>,
    dispatcher: Arc<CommandDispatcher<S>>,
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

                        if let Some(tracker) = engine.cache_tracker() {
                            metrics::gauge!("shroudb_cache_memory_bytes")
                                .set(tracker.total_bytes() as f64);
                            metrics::gauge!("shroudb_cache_resident_keys")
                                .set(tracker.len() as f64);
                            metrics::gauge!("shroudb_cache_budget_bytes")
                                .set(tracker.budget_bytes() as f64);
                        }
                    }
                }
            }
        }));
    }

    // Idempotency map reaper (60s interval)
    {
        let disp = Arc::clone(&dispatcher);
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
                        disp.idempotency().prune();
                    }
                }
            }
        }));
    }

    // Tombstone compaction reaper (300s interval)
    {
        let engine = Arc::clone(&engine);
        let mut rx = shutdown_rx.clone();
        handles.push(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            loop {
                tokio::select! {
                    biased;
                    _ = rx.changed() => {
                        if *rx.borrow() { break; }
                    }
                    _ = interval.tick() => {
                        compact_tombstones(&engine).await;
                    }
                }
            }
        }));
    }

    // TTL sweeper. Uses the engine-configured interval (default 1s).
    // Pops expired entries from the heap — or falls back to a full-index
    // scan if the heap has saturated — and emits standard EntryDeleted
    // WAL entries for each expired key. See `StorageEngine::sweep_tick`.
    {
        let engine = Arc::clone(&engine);
        let mut rx = shutdown_rx.clone();
        let interval_ms = engine.ttl_sweep_interval_ms();
        handles.push(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(interval_ms));
            loop {
                tokio::select! {
                    biased;
                    _ = rx.changed() => {
                        if *rx.borrow() { break; }
                    }
                    _ = interval.tick() => {
                        let now_ms = match std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                        {
                            Ok(d) => d.as_millis() as u64,
                            Err(_) => continue,
                        };
                        match engine.sweep_tick(now_ms).await {
                            Ok(swept) if swept > 0 => {
                                tracing::debug!(swept, "ttl sweeper tombstoned expired entries");
                                metrics::counter!("shroudb_ttl_swept_total").increment(swept);
                            }
                            Ok(_) => {}
                            Err(e) => {
                                tracing::error!(error = %e, "ttl sweeper failed");
                            }
                        }
                    }
                }
            }
        }));
    }

    handles
}

/// Scan all namespaces for expired tombstones and write compaction entries.
async fn compact_tombstones(engine: &Arc<StorageEngine>) {
    let now = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        Err(_) => return,
    };

    for ns_entry in engine.index().namespaces.iter() {
        let ns_name = ns_entry.key().clone();
        let retention = match ns_entry.value().config.tombstone_retention_secs {
            Some(secs) if secs > 0 => secs,
            _ => continue,
        };

        let cutoff = now.saturating_sub(retention);
        let expired = engine.index().find_expired_tombstones(&ns_name, cutoff);

        if expired.is_empty() {
            continue;
        }

        tracing::info!(
            namespace = %ns_name,
            count = expired.len(),
            "compacting expired tombstones"
        );

        if let Err(e) = engine
            .apply(
                &ns_name,
                shroudb_storage::OpType::TombstoneCompacted,
                shroudb_storage::WalPayload::TombstoneCompacted { keys: expired },
            )
            .await
        {
            tracing::error!(
                namespace = %ns_name,
                error = %e,
                "tombstone compaction failed"
            );
        }
    }
}
