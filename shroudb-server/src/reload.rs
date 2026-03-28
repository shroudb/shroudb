//! Config hot-reload support.
//!
//! Watches the config file for mtime changes and reloads:
//! - Auth tokens (swapped atomically via RwLock)
//! - Rate limits (broadcast via watch channel)
//!
//! Non-reloadable settings (bind address, TLS, data directory, storage)
//! require a server restart.

use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Duration;

use shroudb_acl::{AclError, StaticTokenValidator, Token, TokenValidator};
use tokio::sync::watch;

use crate::config;

/// Token validator that can be reloaded at runtime.
///
/// Wraps `StaticTokenValidator` in a `RwLock` so the reloader task can
/// swap in a new validator without interrupting active connections.
pub struct ReloadableValidator {
    inner: RwLock<StaticTokenValidator>,
}

impl ReloadableValidator {
    pub fn new(validator: StaticTokenValidator) -> Self {
        Self {
            inner: RwLock::new(validator),
        }
    }

    /// Replace the inner validator with a new one.
    pub fn replace(&self, validator: StaticTokenValidator) {
        *self.inner.write().unwrap() = validator;
    }

    /// Number of registered tokens.
    pub fn len(&self) -> usize {
        self.inner.read().unwrap().len()
    }
}

impl TokenValidator for ReloadableValidator {
    fn validate(&self, raw: &str) -> Result<Token, AclError> {
        self.inner.read().unwrap().validate(raw)
    }
}

/// Run the config reloader task.
///
/// Polls the config file's mtime every 10 seconds. On change:
/// - Reloads auth tokens into the ReloadableValidator
/// - Updates rate limit via watch channel
pub async fn run_reloader(
    config_path: PathBuf,
    validator: std::sync::Arc<ReloadableValidator>,
    rate_limit_tx: watch::Sender<Option<u32>>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut last_mtime = file_mtime(&config_path);
    let mut interval = tokio::time::interval(Duration::from_secs(10));

    loop {
        tokio::select! {
            biased;
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() { break; }
            }
            _ = interval.tick() => {
                let current_mtime = file_mtime(&config_path);
                if current_mtime == last_mtime {
                    continue;
                }
                last_mtime = current_mtime;

                tracing::info!(path = %config_path.display(), "config file changed, reloading");

                let new_cfg = match config::load_config(&config_path) {
                    Ok(cfg) => cfg,
                    Err(e) => {
                        tracing::error!(error = %e, "failed to reload config, keeping current");
                        continue;
                    }
                };

                // Reload tokens
                let new_validator = config::build_token_validator(&new_cfg);
                let token_count = new_validator.len();
                validator.replace(new_validator);
                tracing::info!(tokens = token_count, "auth tokens reloaded");

                // Reload rate limit
                let new_rate = new_cfg.server.rate_limit_per_second;
                let _ = rate_limit_tx.send(new_rate);
                if let Some(limit) = new_rate {
                    tracing::info!(limit, "rate limit updated");
                }
            }
        }
    }
}

fn file_mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}
