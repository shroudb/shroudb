use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use metrics::gauge;
use shroudb_core::KeyspacePolicy;
use shroudb_protocol::auth::AuthPolicy;
use shroudb_protocol::{Command, CommandDispatcher};
use shroudb_storage::StorageEngine;
use shroudb_storage::snapshot::SnapshotReader;
use tokio::sync::watch;
use tokio::task::JoinHandle;

const SCHEDULER_INTERVAL: Duration = Duration::from_secs(60);

pub fn spawn_all(
    engine: Arc<StorageEngine>,
    dispatcher: Arc<CommandDispatcher>,
    shutdown_rx: watch::Receiver<bool>,
    config_path: PathBuf,
) -> Vec<JoinHandle<()>> {
    let mut handles = vec![
        tokio::spawn(revocation_reaper(engine.clone(), shutdown_rx.clone())),
        tokio::spawn(idempotency_reaper(dispatcher.clone(), shutdown_rx.clone())),
        tokio::spawn(snapshot_compactor(engine.clone(), shutdown_rx.clone())),
        tokio::spawn(rotation_scheduler(
            engine.clone(),
            dispatcher.clone(),
            shutdown_rx.clone(),
        )),
        tokio::spawn(refresh_token_reaper(engine.clone(), shutdown_rx.clone())),
        tokio::spawn(password_rate_limit_reaper(
            engine.clone(),
            shutdown_rx.clone(),
        )),
        tokio::spawn(metrics_reporter(engine.clone(), shutdown_rx.clone())),
        tokio::spawn(config_reloader(
            config_path,
            engine.clone(),
            shutdown_rx.clone(),
        )),
    ];

    // Spawn WAL fsync batcher only if the fsync mode requires it (Batched or Periodic).
    if let Some(interval_ms) = engine.fsync_interval_ms() {
        handles.push(tokio::spawn(wal_fsync_batcher(
            engine.clone(),
            interval_ms,
            shutdown_rx.clone(),
        )));
    }

    handles
}

/// Periodically prune expired revocation entries from all keyspaces.
async fn revocation_reaper(engine: Arc<StorageEngine>, mut shutdown_rx: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = shutdown_rx.wait_for(|v| *v) => break,
            _ = tokio::time::sleep(SCHEDULER_INTERVAL) => {}
        }

        let mut total_pruned = 0usize;
        for entry in engine.index().revocations.iter() {
            total_pruned += entry.value().prune_expired();
        }
        if total_pruned > 0 {
            tracing::info!(
                pruned = total_pruned,
                "revocation reaper: pruned expired entries"
            );
        }
    }
}

/// Periodically prune expired idempotency keys.
async fn idempotency_reaper(
    dispatcher: Arc<CommandDispatcher>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = shutdown_rx.wait_for(|v| *v) => break,
            _ = tokio::time::sleep(SCHEDULER_INTERVAL) => {}
        }

        let pruned = dispatcher.prune_idempotency().await;
        if pruned > 0 {
            tracing::info!(pruned, "idempotency reaper: pruned expired entries");
        }
    }
}

/// Periodically check if a snapshot is needed and take one.
async fn snapshot_compactor(engine: Arc<StorageEngine>, mut shutdown_rx: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = shutdown_rx.wait_for(|v| *v) => break,
            _ = tokio::time::sleep(SCHEDULER_INTERVAL) => {}
        }

        let entry_threshold_exceeded =
            engine.total_entries_since_snapshot().await >= engine.snapshot_entry_threshold();
        let time_threshold_exceeded = engine.time_since_last_snapshot()
            >= Duration::from_secs(engine.snapshot_time_threshold_secs());

        if entry_threshold_exceeded || time_threshold_exceeded {
            match engine.snapshot().await {
                Ok(()) => {
                    tracing::info!("snapshot compactor: snapshot completed");
                }
                Err(e) => {
                    tracing::error!(error = %e, "snapshot compactor: snapshot failed");
                }
            }
        }
    }
}

/// Periodically check for keyspaces that need key rotation and trigger it.
async fn rotation_scheduler(
    engine: Arc<StorageEngine>,
    dispatcher: Arc<CommandDispatcher>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = shutdown_rx.wait_for(|v| *v) => break,
            _ = tokio::time::sleep(SCHEDULER_INTERVAL) => {}
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        for entry in engine.index().keyspaces.iter() {
            let name = entry.key().clone();
            let rotation_days = match &entry.value().policy {
                KeyspacePolicy::Jwt { rotation_days, .. } => *rotation_days,
                KeyspacePolicy::Hmac { rotation_days, .. } => *rotation_days,
                _ => continue,
            };

            // Check active key's created_at from the appropriate ring
            let active_created_at = match &entry.value().policy {
                KeyspacePolicy::Jwt { .. } => engine
                    .index()
                    .jwt_rings
                    .get(&name)
                    .and_then(|ring| ring.active_key().map(|k| k.created_at)),
                KeyspacePolicy::Hmac { .. } => engine
                    .index()
                    .hmac_rings
                    .get(&name)
                    .and_then(|ring| ring.active_key().map(|k| k.created_at)),
                _ => None,
            };

            if let Some(created_at) = active_created_at {
                let rotation_due_at = created_at + (rotation_days as u64 * 86400);
                if now >= rotation_due_at {
                    tracing::info!(
                        keyspace = %name,
                        "rotation scheduler: rotation due, triggering rotate"
                    );
                    let system_auth = AuthPolicy::system();
                    let resp = dispatcher
                        .execute(
                            Command::Rotate {
                                keyspace: name.clone(),
                                force: true,
                                nowait: false,
                                dryrun: false,
                            },
                            Some(&system_auth),
                        )
                        .await;
                    match resp {
                        shroudb_protocol::CommandResponse::Success(_) => {
                            tracing::info!(
                                keyspace = %name,
                                "rotation scheduler: rotation completed"
                            );
                        }
                        shroudb_protocol::CommandResponse::Error(ref e) => {
                            tracing::error!(
                                keyspace = %name,
                                error = %e,
                                "rotation scheduler: rotation failed"
                            );
                        }
                        _ => {
                            tracing::warn!(
                                keyspace = %name,
                                "rotation scheduler: unexpected response"
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Periodically prune expired refresh tokens from all keyspaces.
async fn refresh_token_reaper(engine: Arc<StorageEngine>, mut shutdown_rx: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = shutdown_rx.wait_for(|v| *v) => break,
            _ = tokio::time::sleep(SCHEDULER_INTERVAL) => {}
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut total_removed = 0usize;
        for entry in engine.index().refresh_tokens.iter() {
            total_removed += entry.value().remove_expired(now);
        }
        if total_removed > 0 {
            tracing::info!(
                removed = total_removed,
                "refresh token reaper: removed expired tokens"
            );
        }
    }
}

/// Periodically prune expired rate limiter entries from password keyspaces.
/// Prevents unbounded growth when many distinct users trigger failed attempts.
async fn password_rate_limit_reaper(
    engine: Arc<StorageEngine>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = shutdown_rx.wait_for(|v| *v) => break,
            _ = tokio::time::sleep(SCHEDULER_INTERVAL) => {}
        }

        let mut total_pruned = 0usize;
        for ks_entry in engine.index().keyspaces.iter() {
            let lockout_secs = match &ks_entry.value().policy {
                KeyspacePolicy::Password {
                    lockout_duration_secs,
                    ..
                } => *lockout_duration_secs,
                _ => continue,
            };
            if let Some(limiter) = engine.index().password_rate_limiters.get(ks_entry.key()) {
                total_pruned += limiter.prune_expired(lockout_secs);
            }
        }
        if total_pruned > 0 {
            tracing::info!(
                pruned = total_pruned,
                "password rate limit reaper: pruned expired entries"
            );
        }
    }
}

/// Periodically flush pending WAL writes to disk for Batched/Periodic fsync modes.
async fn wal_fsync_batcher(
    engine: Arc<StorageEngine>,
    interval_ms: u64,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let interval = Duration::from_millis(interval_ms);
    loop {
        tokio::select! {
            _ = shutdown_rx.wait_for(|v| *v) => break,
            _ = tokio::time::sleep(interval) => {}
        }
        if let Err(e) = engine.flush_wal().await {
            tracing::error!(error = %e, "WAL fsync flush failed");
        }
    }
}

const METRICS_INTERVAL: Duration = Duration::from_secs(30);

/// Periodically report per-keyspace gauges to the metrics system.
async fn metrics_reporter(engine: Arc<StorageEngine>, mut shutdown_rx: watch::Receiver<bool>) {
    let start_time = std::time::Instant::now();

    loop {
        tokio::select! {
            _ = shutdown_rx.wait_for(|v| *v) => break,
            _ = tokio::time::sleep(METRICS_INTERVAL) => {}
        }

        // System-level metrics
        gauge!("shroudb_wal_entries_since_snapshot")
            .set(engine.total_entries_since_snapshot().await as f64);
        gauge!("shroudb_uptime_seconds").set(start_time.elapsed().as_secs_f64());

        for entry in engine.index().keyspaces.iter() {
            let name = entry.key();
            // API key counts
            if let Some(idx) = engine.index().api_keys.get(name) {
                gauge!("shroudb_credentials_total", "keyspace" => name.clone(), "type" => "api_key")
                    .set(idx.len() as f64);
            }
            // Refresh token counts
            if let Some(idx) = engine.index().refresh_tokens.get(name) {
                gauge!("shroudb_credentials_total", "keyspace" => name.clone(), "type" => "refresh_token")
                    .set(idx.len() as f64);
            }
            // Signing key counts
            if let Some(ring) = engine.index().jwt_rings.get(name) {
                gauge!("shroudb_signing_keys_total", "keyspace" => name.clone())
                    .set(ring.len() as f64);
            }
            if let Some(ring) = engine.index().hmac_rings.get(name) {
                gauge!("shroudb_signing_keys_total", "keyspace" => name.clone())
                    .set(ring.len() as f64);
            }
            // Password credential counts
            if let Some(idx) = engine.index().passwords.get(name) {
                gauge!("shroudb_credentials_total", "keyspace" => name.clone(), "type" => "password")
                    .set(idx.len() as f64);
            }
            // Revocation set size
            if let Some(revs) = engine.index().revocations.get(name) {
                gauge!("shroudb_revocations_active", "keyspace" => name.clone())
                    .set(revs.len() as f64);
            }

            // Key age and rotation countdown for JWT/HMAC keyspaces
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();

            if let Some(ring) = engine.index().jwt_rings.get(name)
                && let Some(active) = ring.active_key()
            {
                let age_secs = now.saturating_sub(active.created_at);
                gauge!("shroudb_active_key_age_seconds", "keyspace" => name.clone())
                    .set(age_secs as f64);

                // Rotation countdown from keyspace policy
                if let Some(ks) = engine.index().keyspaces.get(name)
                    && let shroudb_core::KeyspacePolicy::Jwt { rotation_days, .. } = &ks.policy
                {
                    let rotation_at = active.created_at + (*rotation_days as u64 * 86400);
                    let countdown = rotation_at.saturating_sub(now);
                    gauge!("shroudb_rotation_countdown_seconds", "keyspace" => name.clone())
                        .set(countdown as f64);
                }
            }
            if let Some(ring) = engine.index().hmac_rings.get(name)
                && let Some(active) = ring.active_key()
            {
                let age_secs = now.saturating_sub(active.created_at);
                gauge!("shroudb_active_key_age_seconds", "keyspace" => name.clone())
                    .set(age_secs as f64);

                if let Some(ks) = engine.index().keyspaces.get(name)
                    && let shroudb_core::KeyspacePolicy::Hmac { rotation_days, .. } = &ks.policy
                {
                    let rotation_at = active.created_at + (*rotation_days as u64 * 86400);
                    let countdown = rotation_at.saturating_sub(now);
                    gauge!("shroudb_rotation_countdown_seconds", "keyspace" => name.clone())
                        .set(countdown as f64);
                }
            }
        }

        // Phase 0 replication metric: snapshot size
        let snap_reader =
            SnapshotReader::new(engine.data_dir().to_path_buf(), engine.namespace().clone());
        if let Ok(Some(snap_path)) = snap_reader.find_latest().await
            && let Ok(metadata) = tokio::fs::metadata(&snap_path).await
        {
            gauge!("shroudb_snapshot_size_bytes").set(metadata.len() as f64);
        }
    }
}

const CONFIG_RELOAD_INTERVAL: Duration = Duration::from_secs(30);

/// Periodically re-read the config file and apply runtime-safe changes.
///
/// Currently hot-reloadable:
/// - Keyspace `disabled` flag (toggling enables/disables keyspaces without restart)
async fn config_reloader(
    config_path: PathBuf,
    engine: Arc<StorageEngine>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut last_modified = std::fs::metadata(&config_path)
        .and_then(|m| m.modified())
        .ok();

    loop {
        tokio::select! {
            _ = shutdown_rx.wait_for(|v| *v) => break,
            _ = tokio::time::sleep(CONFIG_RELOAD_INTERVAL) => {}
        }

        let current_modified = std::fs::metadata(&config_path)
            .and_then(|m| m.modified())
            .ok();

        if current_modified == last_modified {
            continue;
        }

        last_modified = current_modified;
        match crate::config::load(&config_path) {
            Ok(Some(cfg)) => {
                // Apply safe changes: keyspace disabled flag.
                for (name, ks_config) in &cfg.keyspaces {
                    let disabled = ks_config.disabled.unwrap_or(false);
                    if let Some(mut ks) = engine.index().keyspaces.get_mut(name)
                        && ks.disabled != disabled
                    {
                        ks.disabled = disabled;
                        tracing::info!(
                            keyspace = name.as_str(),
                            disabled,
                            "keyspace disabled flag updated via hot-reload"
                        );
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(error = %e, "config hot-reload failed");
            }
        }
    }
}
