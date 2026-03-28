//! ShrouDB — encrypted key-value database.
//!
//! Binary entry point: CLI argument parsing, config loading, and server startup.
//!
//! # Configuration precedence
//!
//! CLI flag > env var > TOML config > default
//!
//! | Setting          | CLI flag       | Env var                  | TOML key          | Default       |
//! |------------------|----------------|--------------------------|-------------------|---------------|
//! | Config file      | -c, --config   | SHROUDB_CONFIG           | —                 | config.toml   |
//! | Master key       | —              | SHROUDB_MASTER_KEY       | —                 | ephemeral     |
//! | Master key file  | —              | SHROUDB_MASTER_KEY_FILE  | —                 | —             |
//! | Data directory   | --data-dir     | SHROUDB_DATA_DIR         | storage.data_dir  | ./data        |
//! | Bind address     | --bind         | SHROUDB_BIND             | server.bind       | 0.0.0.0:6399  |
//! | Log level        | --log-level    | SHROUDB_LOG_LEVEL        | —                 | info          |

mod config;
mod connection;
mod scheduler;
mod server;
mod webhooks;

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use shroudb_crypto::SecretBytes;
use shroudb_protocol::CommandDispatcher;
use shroudb_storage::{EmbeddedStore, MasterKeySource, StorageEngine, StorageError};
use tokio::sync::watch;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "shroudb", about = "ShrouDB — encrypted key-value database")]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(short, long, env = "SHROUDB_CONFIG", default_value = "config.toml")]
    config: PathBuf,

    /// Bind address (host:port).
    #[arg(long, env = "SHROUDB_BIND")]
    bind: Option<String>,

    /// Data directory for WAL and snapshots.
    #[arg(long, env = "SHROUDB_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Log level (trace, debug, info, warn, error).
    #[arg(long, env = "SHROUDB_LOG_LEVEL")]
    log_level: Option<String>,

    #[command(subcommand)]
    command: Option<SubCommand>,
}

#[derive(clap::Subcommand)]
enum SubCommand {
    /// Health check without starting the server. Validates config, master key,
    /// data directory, WAL integrity, and snapshot readability.
    Doctor,

    /// Re-encrypt WAL segments and snapshots with a new master key.
    /// The server must be stopped before running this.
    Rekey {
        /// Old master key (hex-encoded, 64 chars).
        #[arg(long)]
        old_key: String,
        /// New master key (hex-encoded, 64 chars).
        #[arg(long)]
        new_key: String,
    },
}

// ---------------------------------------------------------------------------
// Master key sources
// ---------------------------------------------------------------------------

struct EnvMasterKey;

impl MasterKeySource for EnvMasterKey {
    fn load(
        &self,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<SecretBytes, StorageError>> + Send + '_>>
    {
        Box::pin(async {
            if let Ok(hex_key) = std::env::var("SHROUDB_MASTER_KEY") {
                let bytes = hex::decode(&hex_key)
                    .map_err(|e| StorageError::MasterKeyInvalid(format!("invalid hex: {e}")))?;
                if bytes.len() != 32 {
                    return Err(StorageError::MasterKeyInvalid(format!(
                        "expected 32 bytes, got {}",
                        bytes.len()
                    )));
                }
                return Ok(SecretBytes::new(bytes));
            }

            if let Ok(path) = std::env::var("SHROUDB_MASTER_KEY_FILE") {
                let bytes = tokio::fs::read(&path).await.map_err(|e| {
                    StorageError::MasterKeyInvalid(format!("failed to read {path}: {e}"))
                })?;
                let key_bytes = if bytes.len() == 64 {
                    hex::decode(&bytes).map_err(|e| {
                        StorageError::MasterKeyInvalid(format!(
                            "file appears hex-encoded (64 bytes) but decode failed: {e}"
                        ))
                    })?
                } else {
                    bytes
                };
                if key_bytes.len() != 32 {
                    return Err(StorageError::MasterKeyInvalid(format!(
                        "expected 32 bytes, got {}",
                        key_bytes.len()
                    )));
                }
                return Ok(SecretBytes::new(key_bytes));
            }

            Err(StorageError::MasterKeyNotFound {
                sources: "SHROUDB_MASTER_KEY, SHROUDB_MASTER_KEY_FILE".into(),
            })
        })
    }

    fn source_name(&self) -> &str {
        "environment"
    }
}

/// Ephemeral in-memory key for development. Data does not survive restart.
struct EphemeralMasterKey;

impl MasterKeySource for EphemeralMasterKey {
    fn load(
        &self,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<SecretBytes, StorageError>> + Send + '_>>
    {
        Box::pin(async {
            tracing::warn!("using ephemeral master key — data will not survive restart");
            let mut key = vec![0u8; 32];
            ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut key)
                .map_err(|_| StorageError::MasterKeyInvalid("RNG failure".into()))?;
            Ok(SecretBytes::new(key))
        })
    }

    fn source_name(&self) -> &str {
        "ephemeral"
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Log level: CLI > env > default
    // Clap already resolved CLI vs env via `env = "SHROUDB_LOG_LEVEL"`.
    let log_level = cli.log_level.as_deref().unwrap_or("info");
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(log_level))
        .init();

    // Disable core dumps on Linux
    #[cfg(target_os = "linux")]
    {
        unsafe {
            libc::prctl(libc::PR_SET_DUMPABLE, 0);
        }
        tracing::debug!("core dumps disabled");
    }

    // Load TOML config (may not exist for zero-config dev mode)
    let default_config_path = PathBuf::from("config.toml");
    let mut cfg = if cli.config.exists() {
        config::load_config(&cli.config)
            .with_context(|| format!("loading config from {}", cli.config.display()))?
    } else if cli.config == default_config_path {
        // Default path doesn't exist — fine, use defaults
        tracing::info!("no config file found at default path, using defaults");
        config::ShrouDBConfig::default()
    } else {
        // User explicitly specified a config path that doesn't exist
        anyhow::bail!("config file not found: {}", cli.config.display());
    };

    // Apply CLI/env overrides (precedence: CLI > env > TOML > default)
    // Clap's `env` attribute already merges CLI and env for us.
    if let Some(ref bind) = cli.bind {
        cfg.server.bind = bind
            .parse()
            .with_context(|| format!("invalid bind address: {bind}"))?;
    }
    if let Some(ref data_dir) = cli.data_dir {
        cfg.storage.data_dir = data_dir.clone();
    }

    // Master key
    let key_source: Box<dyn MasterKeySource> = if std::env::var("SHROUDB_MASTER_KEY").is_ok()
        || std::env::var("SHROUDB_MASTER_KEY_FILE").is_ok()
    {
        Box::new(EnvMasterKey)
    } else {
        tracing::warn!("no master key configured — using ephemeral key (dev mode)");
        Box::new(EphemeralMasterKey)
    };

    // Handle subcommands (offline operations)
    if let Some(subcmd) = cli.command {
        return match subcmd {
            SubCommand::Doctor => run_doctor(&cfg, key_source.as_ref()).await,
            SubCommand::Rekey { old_key, new_key } => run_rekey(&cfg, &old_key, &new_key).await,
        };
    }

    // Storage engine (runs WAL recovery)
    let engine_config = config::to_engine_config(&cfg);
    let engine = StorageEngine::open(engine_config, key_source.as_ref())
        .await
        .context("failed to open storage engine")?;
    let engine = Arc::new(engine);

    // Embedded Store
    let store = EmbeddedStore::new(Arc::clone(&engine), "server");

    // Command dispatcher
    let dispatcher = Arc::new(CommandDispatcher::new(store, Arc::clone(&engine)));

    // Token validator + auth
    let token_validator = Arc::new(config::build_token_validator(&cfg));
    let auth_required = config::auth_required(&cfg);

    // Prometheus metrics
    if let Some(metrics_bind) = cfg.server.metrics_bind {
        let builder = metrics_exporter_prometheus::PrometheusBuilder::new();
        builder
            .with_http_listener(metrics_bind)
            .install()
            .context("failed to install Prometheus metrics exporter")?;
    }

    // Startup banner
    let version = env!("CARGO_PKG_VERSION");
    let auth_status = if auth_required {
        format!("token ({} tokens)", token_validator.len())
    } else {
        "disabled".to_string()
    };
    let tls_status = if cfg.server.tls_cert.is_some() {
        if cfg.server.tls_client_ca.is_some() {
            "on (mTLS)"
        } else {
            "on"
        }
    } else {
        "off"
    };
    let key_status = if std::env::var("SHROUDB_MASTER_KEY").is_ok()
        || std::env::var("SHROUDB_MASTER_KEY_FILE").is_ok()
    {
        "configured"
    } else {
        "ephemeral (dev mode)"
    };

    eprintln!();
    eprintln!("ShrouDB v{version}");
    eprintln!("\u{251c}\u{2500} bind:     {}", cfg.server.bind);
    eprintln!(
        "\u{251c}\u{2500} data:     {}",
        cfg.storage.data_dir.display()
    );
    eprintln!("\u{251c}\u{2500} auth:     {auth_status}");
    eprintln!("\u{251c}\u{2500} tls:      {tls_status}");
    eprintln!("\u{2514}\u{2500} key:      {key_status}");
    if let Some(metrics_bind) = cfg.server.metrics_bind {
        eprintln!("   metrics:  {metrics_bind}");
    }
    eprintln!();
    eprintln!("Ready.");

    // Shutdown signal
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Background tasks
    let scheduler_handles = scheduler::spawn_all(Arc::clone(&engine), shutdown_rx.clone());

    // Webhook actors
    let webhook_handles = if !cfg.webhooks.is_empty() {
        tracing::info!(count = cfg.webhooks.len(), "starting webhook actors");
        webhooks::spawn_all(cfg.webhooks.clone(), &engine, &shutdown_rx)
    } else {
        Vec::new()
    };

    // Ctrl-C / SIGTERM handler
    let stx = shutdown_tx.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("shutdown signal received");
        let _ = stx.send(true);
    });

    // Run server (blocks until shutdown)
    server::run(
        &cfg.server,
        dispatcher,
        token_validator,
        auth_required,
        shutdown_rx,
    )
    .await?;

    // Shutdown
    for handle in scheduler_handles {
        handle.abort();
    }
    for handle in webhook_handles {
        handle.abort();
    }
    engine.shutdown().await.map_err(|e| anyhow::anyhow!(e))?;

    tracing::info!("shroudb shut down cleanly");
    Ok(())
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

/// Doctor: validate config, master key, data directory, WAL, and snapshots
/// without starting the server.
async fn run_doctor(
    cfg: &config::ShrouDBConfig,
    key_source: &dyn MasterKeySource,
) -> anyhow::Result<()> {
    eprintln!("ShrouDB doctor");
    eprintln!();

    // 1. Config
    eprintln!("  config:     ok");

    // 2. Data directory
    let data_dir = &cfg.storage.data_dir;
    if data_dir.exists() {
        eprintln!("  data dir:   {} (exists)", data_dir.display());
    } else {
        eprintln!(
            "  data dir:   {} (will be created on first run)",
            data_dir.display()
        );
    }

    // 3. Master key
    match key_source.load().await {
        Ok(_) => {
            eprintln!("  master key: ok ({})", key_source.source_name());
        }
        Err(e) => {
            eprintln!("  master key: FAILED — {e}");
            anyhow::bail!("master key check failed");
        }
    }

    // 4. Storage engine recovery (validates WAL + snapshots)
    let engine_config = config::to_engine_config(cfg);
    match StorageEngine::open(engine_config, key_source).await {
        Ok(engine) => {
            let ns_count = engine.index().namespaces.len();
            let total_keys: u64 = engine
                .index()
                .namespaces
                .iter()
                .map(|ns| ns.value().active_key_count())
                .sum();
            eprintln!("  storage:    ok ({ns_count} namespaces, {total_keys} active keys)");
            engine.shutdown().await.map_err(|e| anyhow::anyhow!(e))?;
        }
        Err(e) => {
            eprintln!("  storage:    FAILED — {e}");
            anyhow::bail!("storage check failed");
        }
    }

    // 5. Auth config
    let validator = config::build_token_validator(cfg);
    if config::auth_required(cfg) {
        eprintln!("  auth:       enabled ({} tokens)", validator.len());
    } else {
        eprintln!("  auth:       disabled");
    }

    eprintln!();
    eprintln!("All checks passed.");
    Ok(())
}

/// Fixed key source for rekey operations.
struct FixedKeySource(SecretBytes);

impl MasterKeySource for FixedKeySource {
    fn load(
        &self,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<SecretBytes, StorageError>> + Send + '_>>
    {
        let bytes = self.0.as_bytes().to_vec();
        Box::pin(async move { Ok(SecretBytes::new(bytes)) })
    }

    fn source_name(&self) -> &str {
        "fixed"
    }
}

/// Rekey: re-encrypt WAL segments and snapshots with a new master key.
/// The server must be stopped before running this.
async fn run_rekey(
    cfg: &config::ShrouDBConfig,
    old_key_hex: &str,
    new_key_hex: &str,
) -> anyhow::Result<()> {
    eprintln!("ShrouDB rekey");
    eprintln!();

    // Safety check: ensure no server is running on the configured port.
    // Rekey iterates the in-memory index without locks — concurrent server
    // access would produce inconsistent results.
    match tokio::net::TcpListener::bind(&cfg.server.bind).await {
        Ok(_listener) => { /* port is free, no server running — listener drops immediately */ }
        Err(_) => {
            anyhow::bail!(
                "port {} is in use — stop the running server before rekey",
                cfg.server.bind,
            );
        }
    }

    // Parse keys
    let old_bytes = hex::decode(old_key_hex).context("invalid old key hex")?;
    let new_bytes = hex::decode(new_key_hex).context("invalid new key hex")?;

    if old_bytes.len() != 32 {
        anyhow::bail!(
            "old key must be 32 bytes (64 hex chars), got {}",
            old_bytes.len()
        );
    }
    if new_bytes.len() != 32 {
        anyhow::bail!(
            "new key must be 32 bytes (64 hex chars), got {}",
            new_bytes.len()
        );
    }

    let old_source = FixedKeySource(SecretBytes::new(old_bytes));
    let new_source = FixedKeySource(SecretBytes::new(new_bytes));

    // Step 1: Open with old key — loads all data into KvIndex
    eprintln!("  opening with old key...");
    let engine_config = config::to_engine_config(cfg);
    let engine = StorageEngine::open(engine_config, &old_source)
        .await
        .context("failed to open storage with old key — is the old key correct?")?;

    let ns_count = engine.index().namespaces.len();
    let total_keys: u64 = engine
        .index()
        .namespaces
        .iter()
        .map(|ns| ns.value().active_key_count())
        .sum();
    eprintln!("  loaded: {ns_count} namespaces, {total_keys} active keys");

    // Step 2: Capture all data from the in-memory index before shutdown
    type NamespaceSnapshot = (
        String,                                                     // namespace name
        shroudb_store::NamespaceConfig,                             // config
        u64,                                                        // created_at
        Vec<(Vec<u8>, Vec<shroudb_storage::index::VersionRecord>)>, // key → versions
    );
    let mut snapshot_data: Vec<NamespaceSnapshot> = Vec::new();

    for ns_entry in engine.index().namespaces.iter() {
        let ns_name = ns_entry.key().clone();
        let ns_state = ns_entry.value();
        let mut keys = Vec::new();

        for key_entry in ns_state.keys.iter() {
            let key = key_entry.key().clone();
            let ks = key_entry.value();
            // Collect all version records
            let versions: Vec<_> = ks
                .versions
                .values()
                .map(|record| shroudb_storage::index::VersionRecord {
                    state: record.state,
                    value: record.value.clone(),
                    metadata: record.metadata.clone(),
                    updated_at: record.updated_at,
                    actor: record.actor.clone(),
                })
                .collect();
            keys.push((key, versions));
        }

        snapshot_data.push((ns_name, ns_state.config.clone(), ns_state.created_at, keys));
    }

    engine.shutdown().await.map_err(|e| anyhow::anyhow!(e))?;

    // Step 3: Wipe the data directory
    eprintln!("  wiping old data...");
    let data_dir = &cfg.storage.data_dir;
    if data_dir.exists() {
        std::fs::remove_dir_all(data_dir)
            .with_context(|| format!("failed to remove data dir: {}", data_dir.display()))?;
    }

    // Step 4: Open fresh engine with new key
    eprintln!("  re-encrypting with new key...");
    let new_engine_config = config::to_engine_config(cfg);
    let new_engine = StorageEngine::open(new_engine_config, &new_source)
        .await
        .context("failed to open fresh storage with new key")?;

    // Step 5: Replay all data into the new engine
    use shroudb_storage::wal::{OpType, WalPayload};

    let mut replayed_keys: u64 = 0;

    for (ns_name, ns_config, created_at, keys) in &snapshot_data {
        // Create namespace
        new_engine
            .apply(
                "__system__",
                OpType::NamespaceCreated,
                WalPayload::NamespaceCreated {
                    name: ns_name.clone(),
                    config: ns_config.clone(),
                    created_at: *created_at,
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!("failed to create namespace {ns_name}: {e}"))?;

        // Replay all key versions in order
        for (key, versions) in keys {
            for record in versions {
                match record.state {
                    shroudb_store::EntryState::Active => {
                        let version = new_engine
                            .index()
                            .namespaces
                            .get(ns_name.as_str())
                            .and_then(|ns| ns.keys.get(key).map(|ks| ks.current_version + 1))
                            .unwrap_or(1);

                        new_engine
                            .apply(
                                ns_name,
                                OpType::EntryPut,
                                WalPayload::EntryPut {
                                    key: key.clone(),
                                    value: record.value.clone(),
                                    metadata: record.metadata.clone(),
                                    version,
                                    actor: record.actor.clone(),
                                },
                            )
                            .await
                            .map_err(|e| anyhow::anyhow!("failed to write key: {e}"))?;
                    }
                    shroudb_store::EntryState::Deleted => {
                        let version = new_engine
                            .index()
                            .namespaces
                            .get(ns_name.as_str())
                            .and_then(|ns| ns.keys.get(key).map(|ks| ks.current_version + 1))
                            .unwrap_or(1);

                        new_engine
                            .apply(
                                ns_name,
                                OpType::EntryDeleted,
                                WalPayload::EntryDeleted {
                                    key: key.clone(),
                                    version,
                                    actor: record.actor.clone(),
                                },
                            )
                            .await
                            .map_err(|e| anyhow::anyhow!("failed to write tombstone: {e}"))?;
                    }
                }
            }
            replayed_keys += 1;
        }
    }

    // Step 6: Snapshot the new state for fast recovery
    new_engine
        .snapshot()
        .await
        .map_err(|e| anyhow::anyhow!("failed to snapshot new state: {e}"))?;
    new_engine
        .shutdown()
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

    eprintln!(
        "  re-encrypted: {} namespaces, {} keys",
        snapshot_data.len(),
        replayed_keys
    );
    eprintln!();
    eprintln!(
        "Rekey complete. Update SHROUDB_MASTER_KEY to the new key before starting the server."
    );

    Ok(())
}
