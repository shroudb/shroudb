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
mod reload;
mod scheduler;
mod server;
mod webhooks;

use std::path::{Path, PathBuf};
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

    /// Export a namespace to an encrypted backup file.
    Export {
        /// Namespace to export.
        #[arg(long)]
        namespace: String,
        /// Output file path.
        #[arg(long, short)]
        output: PathBuf,
    },

    /// Import a namespace from an encrypted backup file.
    Import {
        /// Input file path.
        #[arg(long, short)]
        input: PathBuf,
        /// Optional: rename the namespace on import.
        #[arg(long)]
        namespace: Option<String>,
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

    // Load TOML config early (before telemetry) so we can use otel_endpoint.
    let default_config_path = PathBuf::from("config.toml");
    let mut cfg = if cli.config.exists() {
        config::load_config(&cli.config)
            .with_context(|| format!("loading config from {}", cli.config.display()))?
    } else if cli.config == default_config_path {
        config::ShrouDBConfig::default()
    } else {
        anyhow::bail!("config file not found: {}", cli.config.display());
    };

    // Apply CLI/env overrides BEFORE telemetry (data_dir affects audit.log path)
    if let Some(ref bind) = cli.bind {
        cfg.server.bind = bind
            .parse()
            .with_context(|| format!("invalid bind address: {bind}"))?;
    }
    if let Some(ref data_dir) = cli.data_dir {
        cfg.storage.data_dir = data_dir.clone();
    }

    // Ensure data directory exists before telemetry init (audit.log goes there)
    std::fs::create_dir_all(&cfg.storage.data_dir).with_context(|| {
        format!(
            "creating data directory: {}",
            cfg.storage.data_dir.display()
        )
    })?;

    // Initialize telemetry (console + audit file + optional OTEL)
    let log_level = cli.log_level.as_deref().unwrap_or("info");
    // SAFETY: called before any threads are spawned (single-threaded main init).
    unsafe { std::env::set_var("SHROUDB_LOG_LEVEL", log_level) };
    let telemetry_config = shroudb_telemetry::TelemetryConfig {
        service_name: "shroudb".into(),
        console: true,
        audit_file: true,
        data_dir: Some(cfg.storage.data_dir.display().to_string()),
        otel_endpoint: cfg.server.otel_endpoint.clone(),
    };
    let _telemetry_guard = shroudb_telemetry::init_telemetry(&telemetry_config)
        .context("failed to initialize telemetry")?;

    if !cli.config.exists() && cli.config == default_config_path {
        tracing::info!("no config file found at default path, using defaults");
    }

    // Disable core dumps (Linux + macOS)
    shroudb_crypto::disable_core_dumps();

    // (CLI overrides already applied above, before telemetry init)

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
            SubCommand::Export { namespace, output } => {
                run_export(&cfg, key_source.as_ref(), &namespace, &output).await
            }
            SubCommand::Import { input, namespace } => {
                run_import(&cfg, key_source.as_ref(), &input, namespace.as_deref()).await
            }
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

    // Token validator + auth (reloadable)
    let token_validator = Arc::new(reload::ReloadableValidator::new(
        config::build_token_validator(&cfg),
    ));
    let auth_required = config::auth_required(&cfg);

    // Rate limit watch channel (reloadable)
    let (rate_limit_tx, rate_limit_rx) =
        tokio::sync::watch::channel(cfg.server.rate_limit_per_second);

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
    let scheduler_handles = scheduler::spawn_all(
        Arc::clone(&engine),
        Arc::clone(&dispatcher),
        shutdown_rx.clone(),
    );

    // Webhook actors
    let webhook_handles = if !cfg.webhooks.is_empty() {
        tracing::info!(count = cfg.webhooks.len(), "starting webhook actors");
        webhooks::spawn_all(cfg.webhooks.clone(), &engine, &shutdown_rx)
    } else {
        Vec::new()
    };

    // Config hot-reload task
    let reload_handle = if cli.config.exists() {
        let handle = tokio::spawn(reload::run_reloader(
            cli.config.clone(),
            Arc::clone(&token_validator),
            rate_limit_tx,
            shutdown_rx.clone(),
        ));
        Some(handle)
    } else {
        None
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
        rate_limit_rx,
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
    if let Some(handle) = reload_handle {
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
    // Doctor uses Strict mode to detect corruption that Recover would silently skip.
    let mut engine_config = config::to_engine_config(cfg);
    engine_config.recovery_mode = shroudb_storage::RecoveryMode::Strict;
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

    let rekey_start = std::time::Instant::now();

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
                    wal_position: record.wal_position,
                    vlog_offset: record.vlog_offset,
                    vlog_generation: record.vlog_generation,
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
                                    value: record
                                        .value
                                        .as_bytes()
                                        .expect("value must be resident during import")
                                        .to_vec(),
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

    let elapsed = rekey_start.elapsed();
    let keys_per_sec = if elapsed.as_secs_f64() > 0.0 {
        replayed_keys as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };

    eprintln!(
        "  re-encrypted: {} namespaces, {} keys in {:.2}s ({:.0} keys/s)",
        snapshot_data.len(),
        replayed_keys,
        elapsed.as_secs_f64(),
        keys_per_sec,
    );
    eprintln!();
    eprintln!(
        "Rekey complete. Update SHROUDB_MASTER_KEY to the new key before starting the server."
    );
    eprintln!();
    eprintln!("NOTE: This is an offline operation. The server was not running during rekey.");
    eprintln!("      Online (zero-downtime) rekey is planned for a future release.");

    Ok(())
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// Export bundle format:
/// `[magic:4]["SDB\x01"] [header_json_len:u32] [header_json:N] [encrypted_payload:M]`
///
/// Header JSON: `{ "namespace": "...", "version": 1, "created_at": ... }`
/// Encrypted payload: AES-256-GCM encrypted postcard of `Vec<ExportEntry>`.
/// Encryption key: HKDF-derived from master key with context `"__export__"`.
/// AAD: the header JSON bytes (binds encryption to the export metadata).

#[derive(serde::Serialize, serde::Deserialize)]
struct ExportHeader {
    namespace: String,
    version: u32,
    created_at: u64,
    key_count: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ExportEntry {
    key: Vec<u8>,
    versions: Vec<ExportVersion>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ExportVersion {
    state: u8, // 0 = Active, 1 = Deleted
    value: Vec<u8>,
    metadata: shroudb_store::Metadata,
    updated_at: u64,
    actor: String,
}

const EXPORT_MAGIC: &[u8; 4] = b"SDB\x01";

async fn run_export(
    cfg: &config::ShrouDBConfig,
    key_source: &dyn MasterKeySource,
    namespace: &str,
    output: &Path,
) -> anyhow::Result<()> {
    eprintln!("ShrouDB export");
    eprintln!();

    let engine_config = config::to_engine_config(cfg);
    let engine = StorageEngine::open(engine_config, key_source)
        .await
        .context("failed to open storage engine")?;

    let ns_ref = engine
        .index()
        .namespaces
        .get(namespace)
        .ok_or_else(|| anyhow::anyhow!("namespace not found: {namespace}"))?;

    let ns_config = ns_ref.config.clone();
    let created_at = ns_ref.created_at;
    let mut entries = Vec::new();

    for key_entry in ns_ref.keys.iter() {
        let key = key_entry.key().clone();
        let ks = key_entry.value();
        let versions: Vec<ExportVersion> = ks
            .versions
            .values()
            .map(|record| ExportVersion {
                state: match record.state {
                    shroudb_store::EntryState::Active => 0,
                    shroudb_store::EntryState::Deleted => 1,
                },
                value: record
                    .value
                    .as_bytes()
                    .expect("value must be resident during export")
                    .to_vec(),
                metadata: record.metadata.clone(),
                updated_at: record.updated_at,
                actor: record.actor.clone(),
            })
            .collect();
        entries.push(ExportEntry { key, versions });
    }
    let key_count = entries.len() as u64;
    drop(ns_ref);

    eprintln!("  namespace: {namespace}");
    eprintln!("  keys:      {key_count}");

    // Serialize payload
    let payload_bytes =
        postcard::to_allocvec(&entries).context("failed to serialize export payload")?;

    // Derive export encryption key via HKDF from the master key
    let export_key = engine
        .key_manager()
        .keyspace_key("__export__")
        .map_err(|e| anyhow::anyhow!("failed to derive export key: {e}"))?;

    // Build header
    let header = ExportHeader {
        namespace: namespace.to_string(),
        version: 1,
        created_at,
        key_count,
    };
    let header_json = serde_json::to_vec(&header)?;

    // Encrypt payload with header as AAD
    let encrypted =
        shroudb_crypto::aes_gcm_encrypt(export_key.as_bytes(), &payload_bytes, &header_json)
            .context("failed to encrypt export payload")?;

    // Write bundle
    let mut bundle = Vec::new();
    bundle.extend_from_slice(EXPORT_MAGIC);
    bundle.extend_from_slice(&(header_json.len() as u32).to_le_bytes());
    bundle.extend_from_slice(&header_json);
    bundle.extend_from_slice(&encrypted);

    // Also include namespace config as a separate section after the encrypted payload
    let config_json = serde_json::to_vec(&ns_config)?;
    bundle.extend_from_slice(&(config_json.len() as u32).to_le_bytes());
    bundle.extend_from_slice(&config_json);

    tokio::fs::write(output, &bundle)
        .await
        .with_context(|| format!("failed to write export file: {}", output.display()))?;

    engine.shutdown().await.map_err(|e| anyhow::anyhow!(e))?;

    eprintln!("  output:    {}", output.display());
    eprintln!("  size:      {} bytes", bundle.len());
    eprintln!();
    eprintln!("Export complete.");

    Ok(())
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

async fn run_import(
    cfg: &config::ShrouDBConfig,
    key_source: &dyn MasterKeySource,
    input: &Path,
    rename_namespace: Option<&str>,
) -> anyhow::Result<()> {
    eprintln!("ShrouDB import");
    eprintln!();

    let bundle = tokio::fs::read(input)
        .await
        .with_context(|| format!("failed to read import file: {}", input.display()))?;

    // Parse magic
    if bundle.len() < 8 || &bundle[..4] != EXPORT_MAGIC {
        anyhow::bail!("invalid export file: bad magic bytes");
    }

    // Parse header
    let header_len = u32::from_le_bytes(
        bundle[4..8]
            .try_into()
            .context("invalid export file: header length truncated")?,
    ) as usize;
    if bundle.len() < 8 + header_len {
        anyhow::bail!("invalid export file: truncated header");
    }
    let header_json = &bundle[8..8 + header_len];
    let header: ExportHeader =
        serde_json::from_slice(header_json).context("invalid export file: bad header JSON")?;

    eprintln!("  source namespace: {}", header.namespace);
    eprintln!("  keys:             {}", header.key_count);

    let ns_name = rename_namespace.unwrap_or(&header.namespace);
    eprintln!("  target namespace: {ns_name}");

    // Open engine
    let engine_config = config::to_engine_config(cfg);
    let engine = StorageEngine::open(engine_config, key_source)
        .await
        .context("failed to open storage engine")?;

    // Check target namespace doesn't already exist
    if engine.index().namespaces.contains_key(ns_name) {
        engine.shutdown().await.map_err(|e| anyhow::anyhow!(e))?;
        anyhow::bail!("namespace already exists: {ns_name}");
    }

    // Derive export key and decrypt
    let export_key = engine
        .key_manager()
        .keyspace_key("__export__")
        .map_err(|e| anyhow::anyhow!("failed to derive export key: {e}"))?;

    // Find where encrypted payload ends and config section begins
    let encrypted_start = 8 + header_len;
    // Config section is at the end: [config_len:u32][config_json:N]
    // We need to find the boundary. The encrypted payload includes nonce+ciphertext+tag.
    // The config section is the last config_len+4 bytes.
    if bundle.len() < encrypted_start + 4 {
        anyhow::bail!("invalid export file: truncated");
    }

    // Read config from the end
    // Actually, config_len is stored BEFORE config_json, so we need to know where.
    // The format is: [...encrypted...] [config_len:u32] [config_json:N]
    // So config_json ends at bundle.len(), config_len is at bundle.len() - config_json.len() - 4
    // But we don't know config_json.len() without config_len.
    //
    // Let me read config_len from after the encrypted section. We need to know where
    // the encrypted section ends. Since AES-GCM output is variable length, we stored
    // config_len right after the encrypted payload.
    //
    // Actually the format from export is:
    //   [magic:4] [header_len:u32] [header_json:N] [encrypted:M] [config_len:u32] [config_json:P]
    //
    // We know magic(4) + header_len(4) + header_json(N). Encrypted goes until config_len.
    // config_len is at bundle.len() - P - 4 where P = config_len.
    //
    // We need to scan from the end. Read last bytes as potential config_len, check if valid.
    // Safer: try different offsets. But simplest: the encrypted payload is everything between
    // header end and the config section.

    // Read config_len: it's 4 bytes before the config JSON at the end.
    // We try: last 4 bytes of the non-config part tell us config_json length.
    // Let's scan backwards.
    let mut config_end = bundle.len();
    // Try reading a u32 at various positions from the end
    // The config section is: [config_len_u32_le] [config_json_bytes]
    // config_json_bytes is at the very end, preceded by its length.
    // So: config_len is at (bundle.len() - config_json.len() - 4)
    // and config_json is at (bundle.len() - config_json.len())
    // We don't know config_json.len() a priori, but we can try:
    // Read u32 at position X, check if X + 4 + that_u32 == bundle.len()
    // Start from end, work backwards
    let mut ns_config = None;
    for try_offset in (encrypted_start..bundle.len().saturating_sub(4)).rev() {
        let Ok(len_bytes) = bundle[try_offset..try_offset + 4].try_into() else {
            continue;
        };
        let candidate_len = u32::from_le_bytes(len_bytes) as usize;
        if try_offset + 4 + candidate_len == bundle.len() && candidate_len < 1_000_000 {
            // Try parsing as JSON
            let config_bytes = &bundle[try_offset + 4..];
            if let Ok(config) =
                serde_json::from_slice::<shroudb_store::NamespaceConfig>(config_bytes)
            {
                ns_config = Some(config);
                config_end = try_offset;
                break;
            }
        }
    }

    let ns_config = ns_config.unwrap_or_default();
    let encrypted = &bundle[encrypted_start..config_end];

    let payload_bytes =
        shroudb_crypto::aes_gcm_decrypt(export_key.as_bytes(), encrypted, header_json)
            .context("failed to decrypt export — wrong master key or corrupted file")?;

    let entries: Vec<ExportEntry> =
        postcard::from_bytes(&payload_bytes).context("failed to deserialize export payload")?;

    // Create namespace
    use shroudb_storage::wal::{OpType, WalPayload};

    engine
        .apply(
            "__system__",
            OpType::NamespaceCreated,
            WalPayload::NamespaceCreated {
                name: ns_name.to_string(),
                config: ns_config,
                created_at: header.created_at,
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("failed to create namespace: {e}"))?;

    // Replay entries
    let mut replayed = 0u64;
    for entry in &entries {
        for version in &entry.versions {
            let next_version = engine
                .index()
                .namespaces
                .get(ns_name)
                .and_then(|ns| ns.keys.get(&entry.key).map(|ks| ks.current_version + 1))
                .unwrap_or(1);

            match version.state {
                0 => {
                    engine
                        .apply(
                            ns_name,
                            OpType::EntryPut,
                            WalPayload::EntryPut {
                                key: entry.key.clone(),
                                value: version.value.clone(),
                                metadata: version.metadata.clone(),
                                version: next_version,
                                actor: version.actor.clone(),
                            },
                        )
                        .await
                        .map_err(|e| anyhow::anyhow!("failed to write key: {e}"))?;
                }
                1 => {
                    engine
                        .apply(
                            ns_name,
                            OpType::EntryDeleted,
                            WalPayload::EntryDeleted {
                                key: entry.key.clone(),
                                version: next_version,
                                actor: version.actor.clone(),
                            },
                        )
                        .await
                        .map_err(|e| anyhow::anyhow!("failed to write tombstone: {e}"))?;
                }
                _ => {
                    tracing::warn!(
                        state = version.state,
                        "unknown version state in export, skipping"
                    );
                }
            }
        }
        replayed += 1;
    }

    engine.snapshot().await.map_err(|e| anyhow::anyhow!(e))?;
    engine.shutdown().await.map_err(|e| anyhow::anyhow!(e))?;

    eprintln!("  imported:  {replayed} keys");
    eprintln!();
    eprintln!("Import complete.");

    Ok(())
}
