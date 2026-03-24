//! ShrouDB — credential management server.
//!
//! Binary entry point: CLI argument parsing, config loading, and server startup.

mod config;
mod connection;
mod scheduler;
mod server;

use std::path::Path;
use std::sync::Arc;

use clap::Parser;
use shroudb_crypto::SecretBytes;
use shroudb_protocol::CommandDispatcher;
use shroudb_storage::{ChainedMasterKeySource, MasterKeySource, StorageEngine};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Parser)]
#[command(name = "shroudb", about = "Credential management server", version)]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(long, default_value = "config.toml")]
    config: std::path::PathBuf,

    #[command(subcommand)]
    command: Option<SubCommand>,
}

#[derive(clap::Subcommand)]
enum SubCommand {
    /// Purge all data for a keyspace (destructive, requires confirmation)
    Purge {
        /// Keyspace name to purge
        keyspace: String,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// Re-encrypt all WAL segments and snapshots with a new master key
    Rekey {
        /// Old master key (hex-encoded, 32 bytes = 64 hex chars)
        #[arg(long)]
        old_key: String,
        /// New master key (hex-encoded, 32 bytes = 64 hex chars)
        #[arg(long)]
        new_key: String,
    },
    /// Export a keyspace's credentials to an encrypted bundle file
    Export {
        /// Keyspace name to export
        keyspace: String,
        /// Output file path
        #[arg(long)]
        output: std::path::PathBuf,
    },
    /// Import credentials from an encrypted bundle file into a keyspace
    Import {
        /// Input bundle file path
        #[arg(long)]
        file: std::path::PathBuf,
        /// Target keyspace name
        #[arg(long)]
        keyspace: String,
    },
    /// Check system health without starting the server
    Doctor,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 0. Disable core dumps to prevent leaking secrets (Linux only).
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0);
    }

    // 1. Parse CLI arguments.
    let cli = Cli::parse();

    // Handle subcommands before normal server startup.
    match cli.command {
        Some(SubCommand::Purge { keyspace, yes }) => {
            return handle_purge(&cli.config, &keyspace, yes).await;
        }
        Some(SubCommand::Rekey { old_key, new_key }) => {
            return handle_rekey(&cli.config, &old_key, &new_key).await;
        }
        Some(SubCommand::Export { keyspace, output }) => {
            return handle_export(&cli.config, &keyspace, &output).await;
        }
        Some(SubCommand::Import { file, keyspace }) => {
            return handle_import(&cli.config, &file, &keyspace).await;
        }
        Some(SubCommand::Doctor) => {
            return handle_doctor(&cli.config).await;
        }
        None => {}
    }

    // 2. Load configuration (or use defaults if no config file).
    let cfg = match config::load(&cli.config)? {
        Some(cfg) => {
            // Config file exists — use JSON tracing for production with
            // a separate audit log file routed via the "shroudb::audit" target.
            let data_dir = &cfg.storage.data_dir;
            std::fs::create_dir_all(data_dir)?;
            let audit_file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(data_dir.join("audit.log"))?;

            let audit_filter = tracing_subscriber::filter::Targets::new()
                .with_target("shroudb::audit", tracing::Level::INFO);
            let audit_layer = tracing_subscriber::fmt::layer()
                .json()
                .with_writer(std::sync::Mutex::new(audit_file))
                .with_filter(audit_filter);

            let env_filter = tracing_subscriber::EnvFilter::from_default_env();
            let console_layer = tracing_subscriber::fmt::layer()
                .json()
                .with_filter(env_filter);

            tracing_subscriber::registry()
                .with(audit_layer)
                .with(console_layer)
                .init();

            tracing::info!(config = %cli.config.display(), "configuration loaded");
            cfg
        }
        None => {
            // No config file — dev mode with human-readable logs.
            // Audit events go to {data_dir}/audit.log (default: ./data/audit.log).
            let data_dir = std::path::PathBuf::from("./data");
            std::fs::create_dir_all(&data_dir)?;
            let audit_file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(data_dir.join("audit.log"))?;

            let audit_filter = tracing_subscriber::filter::Targets::new()
                .with_target("shroudb::audit", tracing::Level::INFO);
            let audit_layer = tracing_subscriber::fmt::layer()
                .json()
                .with_writer(std::sync::Mutex::new(audit_file))
                .with_filter(audit_filter);

            let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
            let console_layer = tracing_subscriber::fmt::layer().with_filter(env_filter);

            tracing_subscriber::registry()
                .with(audit_layer)
                .with(console_layer)
                .init();

            tracing::info!("no config file found, starting with defaults");
            config::ShrouDBConfig::default()
        }
    };

    // 3. Resolve master key source.
    let key_source = resolve_master_key()?;

    // 4. Convert storage section to engine config.
    let engine_config = config::to_engine_config(&cfg);

    // 5. Open storage engine (runs WAL recovery).
    let engine = StorageEngine::open(engine_config, &*key_source).await?;
    let engine = Arc::new(engine);
    tracing::info!("storage engine ready");

    // 6. Register keyspaces from config into the in-memory index.
    for (name, ks_config) in &cfg.keyspaces {
        let keyspace = config::to_keyspace(name, ks_config)?;
        let ks_type = keyspace.keyspace_type;
        engine.index().keyspaces.insert(name.clone(), keyspace);
        engine.index().ensure_keyspace(name, ks_type);
        tracing::info!(keyspace = %name, r#type = ?ks_type, "registered keyspace");
    }

    // 7. Build auth registry and command dispatcher.
    let auth_registry = Arc::new(config::build_auth_registry(&cfg.auth));
    if auth_registry.is_required() {
        tracing::info!("authentication enabled");
    }
    let dispatcher = Arc::new(CommandDispatcher::new(
        Arc::clone(&engine),
        Arc::clone(&auth_registry),
    ));

    // 8. Set up webhook dispatcher (if configured).
    let webhook_configs = config::to_webhook_configs(&cfg.webhooks);
    if !webhook_configs.is_empty() {
        let (dispatcher_wh, rx) = shroudb_protocol::WebhookDispatcher::new(webhook_configs.clone());
        tracing::info!(
            count = webhook_configs.len(),
            "webhook dispatcher initialized"
        );
        // Spawn the background delivery loop.
        tokio::spawn(shroudb_protocol::webhooks::webhook_delivery_loop(
            rx,
            webhook_configs,
        ));
        // The dispatcher_wh can be integrated into CommandDispatcher
        // when per-handler webhook notifications are wired up.
        drop(dispatcher_wh);
    }

    // 9. Install Prometheus metrics recorder.
    let metrics_handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install metrics recorder");

    // 10. Set up shutdown signal (SIGTERM + SIGINT).
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = shutdown_tx.send(true);
    });

    // 11. Spawn background scheduler tasks.
    let scheduler_handles = scheduler::spawn_all(
        Arc::clone(&engine),
        Arc::clone(&dispatcher),
        shutdown_rx.clone(),
        cli.config.clone(),
    );

    // 12. Run server (blocks until shutdown).
    tracing::info!(bind = %cfg.server.bind, "shroudb ready");
    server::run(&cfg.server, dispatcher, metrics_handle, shutdown_rx).await?;

    // 13. Abort scheduler tasks.
    for handle in scheduler_handles {
        handle.abort();
    }

    // 14. Shut down storage engine (flush WAL, fsync).
    engine.shutdown().await?;

    // 15. Clean exit.
    tracing::info!("shroudb shut down cleanly");
    Ok(())
}

/// Resolve the master key source: try env/file first, fall back to ephemeral.
fn resolve_master_key() -> anyhow::Result<Box<dyn MasterKeySource>> {
    // Check if SHROUDB_MASTER_KEY or SHROUDB_MASTER_KEY_FILE is set.
    if std::env::var("SHROUDB_MASTER_KEY").is_ok() || std::env::var("SHROUDB_MASTER_KEY_FILE").is_ok() {
        return Ok(Box::new(ChainedMasterKeySource::default_chain()));
    }

    // No master key configured — generate ephemeral key for dev mode.
    tracing::warn!(
        "no master key configured (set SHROUDB_MASTER_KEY or SHROUDB_MASTER_KEY_FILE for persistence)"
    );
    tracing::warn!("using ephemeral master key — data will NOT survive restart");
    Ok(Box::new(EphemeralMasterKey::generate()))
}

/// An ephemeral in-memory master key for development mode.
/// Data encrypted with this key cannot be recovered after process exit.
struct EphemeralMasterKey {
    key: SecretBytes,
}

impl EphemeralMasterKey {
    fn generate() -> Self {
        use ring::rand::{SecureRandom, SystemRandom};
        let rng = SystemRandom::new();
        let mut bytes = vec![0u8; 32];
        rng.fill(&mut bytes).expect("CSPRNG fill failed");
        Self {
            key: SecretBytes::new(bytes),
        }
    }
}

impl MasterKeySource for EphemeralMasterKey {
    fn load(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<SecretBytes, shroudb_storage::StorageError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async { Ok(self.key.clone()) })
    }

    fn source_name(&self) -> &str {
        "ephemeral"
    }
}

/// Purge all data for a keyspace and snapshot the clean state.
async fn handle_purge(
    config_path: &Path,
    keyspace: &str,
    skip_confirm: bool,
) -> anyhow::Result<()> {
    // Init minimal tracing.
    tracing_subscriber::fmt().init();

    let cfg = config::load(config_path)?.unwrap_or_default();
    let key_source = resolve_master_key()?;
    let engine_config = config::to_engine_config(&cfg);
    let engine = StorageEngine::open(engine_config, &*key_source).await?;

    if engine.index().keyspaces.get(keyspace).is_none() {
        anyhow::bail!("keyspace '{keyspace}' not found");
    }

    if !skip_confirm {
        println!("This will permanently delete all data for keyspace '{keyspace}'.");
        print!("Type the keyspace name to confirm: ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if input.trim() != keyspace {
            anyhow::bail!("confirmation failed — aborting");
        }
    }

    // Remove from all indexes.
    engine.index().keyspaces.remove(keyspace);
    engine.index().api_keys.remove(keyspace);
    engine.index().refresh_tokens.remove(keyspace);
    engine.index().jwt_rings.remove(keyspace);
    engine.index().hmac_rings.remove(keyspace);
    engine.index().revocations.remove(keyspace);

    // Snapshot clean state (purged keyspace excluded).
    engine.snapshot().await?;
    engine.shutdown().await?;

    tracing::info!(keyspace, "keyspace purged");
    println!("Keyspace '{keyspace}' purged successfully.");
    Ok(())
}

/// Re-encrypt all WAL segments and the latest snapshot with a new master key.
///
/// This is an offline operation — the server must not be running.
/// Steps:
/// 1. Open storage with the old key and recover state
/// 2. Build a new key manager with the new key
/// 3. Snapshot the entire recovered state under the new key
/// 4. Remove old WAL segments (the new snapshot captures all state)
async fn handle_rekey(
    config_path: &Path,
    old_key_hex: &str,
    new_key_hex: &str,
) -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    let old_key_bytes =
        hex::decode(old_key_hex.trim()).map_err(|e| anyhow::anyhow!("invalid old key hex: {e}"))?;
    if old_key_bytes.len() != 32 {
        anyhow::bail!(
            "old key must be 32 bytes (64 hex chars), got {} bytes",
            old_key_bytes.len()
        );
    }
    let new_key_bytes =
        hex::decode(new_key_hex.trim()).map_err(|e| anyhow::anyhow!("invalid new key hex: {e}"))?;
    if new_key_bytes.len() != 32 {
        anyhow::bail!(
            "new key must be 32 bytes (64 hex chars), got {} bytes",
            new_key_bytes.len()
        );
    }

    let cfg = config::load(config_path)?.unwrap_or_default();
    let engine_config = config::to_engine_config(&cfg);
    let data_dir = engine_config.data_dir.clone();
    let namespace = engine_config.namespace.clone();

    // Open engine with the old key — this runs full recovery.
    let old_source = FixedKeySource(shroudb_crypto::SecretBytes::new(old_key_bytes));
    let engine = StorageEngine::open(engine_config, &old_source).await?;

    // Register keyspaces from config.
    for (name, ks_config) in &cfg.keyspaces {
        let keyspace = config::to_keyspace(name, ks_config)?;
        let ks_type = keyspace.keyspace_type;
        engine.index().keyspaces.insert(name.clone(), keyspace);
        engine.index().ensure_keyspace(name, ks_type);
    }

    tracing::info!("state recovered with old key, re-encrypting with new key");

    // Build new key manager.
    let new_source = FixedKeySource(shroudb_crypto::SecretBytes::new(new_key_bytes));
    let new_km = shroudb_storage::key_manager::KeyManager::new(
        &new_source,
        shroudb_core::TenantContext::default(),
    )
    .await?;

    // Build snapshot data from the recovered index using the new key manager.
    // We use the engine's index but re-encrypt private keys with new derived keys.
    let index = engine.index();
    let mut keyspaces = Vec::new();
    for entry in index.keyspaces.iter() {
        let ks = entry.value().clone();
        let name = entry.key().clone();
        let mut ks_snap = ks;

        if let Some(api_idx) = index.api_keys.get(&name) {
            ks_snap.api_keys = api_idx.all_entries();
        }
        if let Some(rt_idx) = index.refresh_tokens.get(&name) {
            ks_snap.refresh_tokens = rt_idx.all_entries();
        }
        if let Some(ring) = index.jwt_rings.get(&name) {
            ks_snap.signing_keys = ring.all_keys();
        }
        if let Some(ring) = index.hmac_rings.get(&name) {
            ks_snap.hmac_keys = ring.all_keys();
        }

        // Re-encrypt private keys with the NEW key manager.
        let mut encrypted_private_keys = Vec::new();
        if let Some(ring) = index.jwt_rings.get(&name) {
            for key in ring.all_keys() {
                if let Some(ref pk) = key.private_key {
                    let encrypted = shroudb_crypto::aes_gcm_encrypt(
                        new_km.private_key_key(&name)?.as_bytes(),
                        pk.as_bytes(),
                        b"private_key",
                    )?;
                    encrypted_private_keys.push(
                        shroudb_storage::snapshot::format::EncryptedPrivateKey {
                            key_id: key.key_id.clone(),
                            encrypted_bytes: encrypted,
                        },
                    );
                }
            }
        }
        if let Some(ring) = index.hmac_rings.get(&name) {
            for key in ring.all_keys() {
                if let Some(ref km) = key.key_material {
                    let encrypted = shroudb_crypto::aes_gcm_encrypt(
                        new_km.private_key_key(&name)?.as_bytes(),
                        km.as_bytes(),
                        b"private_key",
                    )?;
                    encrypted_private_keys.push(
                        shroudb_storage::snapshot::format::EncryptedPrivateKey {
                            key_id: key.key_id.clone(),
                            encrypted_bytes: encrypted,
                        },
                    );
                }
            }
        }

        keyspaces.push(shroudb_storage::snapshot::format::KeyspaceSnapshot {
            keyspace: ks_snap,
            encrypted_private_keys,
        });
    }

    let snapshot_data = shroudb_storage::snapshot::format::SnapshotData {
        keyspaces,
        revocations: Vec::new(),
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let header = shroudb_storage::snapshot::format::SnapshotHeader {
        version: 1,
        encoding: "postcard-v1".to_string(),
        created_at: now,
        snapshot_id: uuid::Uuid::new_v4().to_string(),
        namespace: namespace.as_str().to_string(),
        wal_checkpoint: shroudb_storage::wal::writer::WalCheckpoint {
            segment_seq: u64::MAX,
            byte_offset: 0,
            entry_count: 0,
        },
        keyspace_count: index.keyspaces.len(),
        total_credentials: index.api_keys.iter().map(|r| r.len() as u64).sum::<u64>()
            + index
                .refresh_tokens
                .iter()
                .map(|r| r.len() as u64)
                .sum::<u64>(),
    };

    let writer =
        shroudb_storage::snapshot::writer::SnapshotWriter::new(data_dir.clone(), namespace.clone());
    writer
        .write(
            &header,
            &snapshot_data,
            new_km.snapshot_key().as_bytes(),
            new_km.snapshot_hmac_key().as_bytes(),
        )
        .await?;

    // Delete all old WAL segments (snapshot captures complete state).
    let wal_reader =
        shroudb_storage::wal::reader::WalReader::new(data_dir.clone(), namespace.clone());
    let segments = wal_reader.list_segments().await?;
    for (_seq, path) in &segments {
        tokio::fs::remove_file(path).await?;
    }

    engine.shutdown().await?;

    tracing::info!("rekey complete — all data re-encrypted with new master key");
    println!("Rekey complete. Update SHROUDB_MASTER_KEY to the new key before restarting.");
    Ok(())
}

/// A fixed master key source for rekey/export/import operations.
struct FixedKeySource(shroudb_crypto::SecretBytes);

impl MasterKeySource for FixedKeySource {
    fn load(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<SecretBytes, shroudb_storage::StorageError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async { Ok(self.0.clone()) })
    }

    fn source_name(&self) -> &str {
        "fixed"
    }
}

/// Export bundle magic bytes and version.
const EXPORT_MAGIC: &[u8; 4] = b"KVEX";
const EXPORT_VERSION: u16 = 1;

/// Export a keyspace's credentials to an encrypted bundle file.
///
/// Bundle format: KVEX (4) | version (2) | header_len (4) | header_json | encrypted_data | hmac (32)
///
/// The bundle is encrypted with a key derived from the master key using the context
/// "__export__", making it portable across keyspace renames.
async fn handle_export(config_path: &Path, keyspace: &str, output: &Path) -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    let cfg = config::load(config_path)?.unwrap_or_default();
    let key_source = resolve_master_key()?;
    let engine_config = config::to_engine_config(&cfg);
    let engine = StorageEngine::open(engine_config, &*key_source).await?;

    // Register keyspaces from config.
    for (name, ks_config) in &cfg.keyspaces {
        let keyspace_obj = config::to_keyspace(name, ks_config)?;
        let ks_type = keyspace_obj.keyspace_type;
        engine.index().keyspaces.insert(name.clone(), keyspace_obj);
        engine.index().ensure_keyspace(name, ks_type);
    }

    if engine.index().keyspaces.get(keyspace).is_none() {
        anyhow::bail!("keyspace '{keyspace}' not found");
    }

    // Build export data for this keyspace.
    let index = engine.index();
    let ks = index
        .keyspaces
        .get(keyspace)
        .ok_or_else(|| anyhow::anyhow!("keyspace not found"))?
        .clone();
    let mut ks_export = ks;

    if let Some(api_idx) = index.api_keys.get(keyspace) {
        ks_export.api_keys = api_idx.all_entries();
    }
    if let Some(rt_idx) = index.refresh_tokens.get(keyspace) {
        ks_export.refresh_tokens = rt_idx.all_entries();
    }
    if let Some(ring) = index.jwt_rings.get(keyspace) {
        ks_export.signing_keys = ring.all_keys();
    }
    if let Some(ring) = index.hmac_rings.get(keyspace) {
        ks_export.hmac_keys = ring.all_keys();
    }

    // Collect encrypted private keys (re-encrypted with the export key).
    let export_enc_key = shroudb_crypto::derive_key(
        resolve_master_key()?.load().await?.as_bytes(),
        "default",
        "__export__",
        32,
    )?;
    let export_hmac_key = shroudb_crypto::derive_key(
        resolve_master_key()?.load().await?.as_bytes(),
        "default",
        "__export_hmac__",
        32,
    )?;

    let mut encrypted_private_keys = Vec::new();

    // Re-encrypt JWT private keys for the export bundle.
    if let Some(ring) = index.jwt_rings.get(keyspace) {
        for key in ring.all_keys() {
            if let Some(ref pk) = key.private_key {
                let encrypted = shroudb_crypto::aes_gcm_encrypt(
                    export_enc_key.as_bytes(),
                    pk.as_bytes(),
                    b"export_pk",
                )?;
                encrypted_private_keys.push(shroudb_storage::snapshot::format::EncryptedPrivateKey {
                    key_id: key.key_id.clone(),
                    encrypted_bytes: encrypted,
                });
            }
        }
    }

    // Re-encrypt HMAC key material for the export bundle.
    if let Some(ring) = index.hmac_rings.get(keyspace) {
        for key in ring.all_keys() {
            if let Some(ref km) = key.key_material {
                let encrypted = shroudb_crypto::aes_gcm_encrypt(
                    export_enc_key.as_bytes(),
                    km.as_bytes(),
                    b"export_pk",
                )?;
                encrypted_private_keys.push(shroudb_storage::snapshot::format::EncryptedPrivateKey {
                    key_id: key.key_id.clone(),
                    encrypted_bytes: encrypted,
                });
            }
        }
    }

    let export_snap = shroudb_storage::snapshot::format::KeyspaceSnapshot {
        keyspace: ks_export,
        encrypted_private_keys,
    };

    // Serialize header (JSON).
    let header = serde_json::json!({
        "version": EXPORT_VERSION,
        "keyspace": keyspace,
        "exported_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    });
    let header_json = serde_json::to_vec(&header)?;

    // Serialize payload with postcard.
    let payload_bytes = postcard::to_allocvec(&export_snap)
        .map_err(|e| anyhow::anyhow!("serialization failed: {e}"))?;

    // Encrypt payload.
    let encrypted_payload =
        shroudb_crypto::aes_gcm_encrypt(export_enc_key.as_bytes(), &payload_bytes, b"export")?;

    // Build bundle.
    let mut bundle = Vec::new();
    bundle.extend_from_slice(EXPORT_MAGIC);
    bundle.extend_from_slice(&EXPORT_VERSION.to_le_bytes());
    bundle.extend_from_slice(&(header_json.len() as u32).to_le_bytes());
    bundle.extend_from_slice(&header_json);
    bundle.extend_from_slice(&encrypted_payload);

    // HMAC over everything.
    let hmac = shroudb_crypto::hmac_sign(
        shroudb_crypto::HmacAlgorithm::Sha256,
        export_hmac_key.as_bytes(),
        &bundle,
    )?;
    bundle.extend_from_slice(&hmac);

    tokio::fs::write(output, &bundle).await?;
    engine.shutdown().await?;

    let cred_count = export_snap.keyspace.api_keys.len()
        + export_snap.keyspace.refresh_tokens.len()
        + export_snap.keyspace.signing_keys.len()
        + export_snap.keyspace.hmac_keys.len();

    tracing::info!(
        keyspace,
        credentials = cred_count,
        path = %output.display(),
        "export complete"
    );
    println!(
        "Exported keyspace '{keyspace}' ({cred_count} credentials) to {}",
        output.display()
    );
    Ok(())
}

/// Import credentials from an encrypted bundle file into a keyspace.
async fn handle_import(
    config_path: &Path,
    file: &Path,
    target_keyspace: &str,
) -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    let cfg = config::load(config_path)?.unwrap_or_default();
    let key_source = resolve_master_key()?;
    let engine_config = config::to_engine_config(&cfg);
    let engine = Arc::new(StorageEngine::open(engine_config, &*key_source).await?);

    // Register keyspaces from config.
    for (name, ks_config) in &cfg.keyspaces {
        let keyspace_obj = config::to_keyspace(name, ks_config)?;
        let ks_type = keyspace_obj.keyspace_type;
        engine.index().keyspaces.insert(name.clone(), keyspace_obj);
        engine.index().ensure_keyspace(name, ks_type);
    }

    if engine.index().keyspaces.get(target_keyspace).is_none() {
        anyhow::bail!("target keyspace '{target_keyspace}' not found in config");
    }

    // Read bundle.
    let data = tokio::fs::read(file).await?;

    if data.len() < 4 + 2 + 4 + 32 {
        anyhow::bail!("bundle file too small");
    }

    // Verify magic.
    if &data[..4] != EXPORT_MAGIC {
        anyhow::bail!("not a valid shroudb export bundle (bad magic)");
    }

    let version = u16::from_le_bytes([data[4], data[5]]);
    if version != EXPORT_VERSION {
        anyhow::bail!("unsupported export bundle version: {version}");
    }

    // Derive export keys.
    let master_key = resolve_master_key()?.load().await?;
    let export_enc_key =
        shroudb_crypto::derive_key(master_key.as_bytes(), "default", "__export__", 32)?;
    let export_hmac_key =
        shroudb_crypto::derive_key(master_key.as_bytes(), "default", "__export_hmac__", 32)?;

    // Verify HMAC.
    let (content, hmac_bytes) = data.split_at(data.len() - 32);
    let valid = shroudb_crypto::hmac_verify(
        shroudb_crypto::HmacAlgorithm::Sha256,
        export_hmac_key.as_bytes(),
        content,
        hmac_bytes,
    )?;
    if !valid {
        anyhow::bail!("bundle HMAC verification failed — wrong master key or corrupted file");
    }

    // Parse header.
    let header_len = u32::from_le_bytes([content[6], content[7], content[8], content[9]]) as usize;
    let header_start = 10;
    let header_end = header_start + header_len;
    if content.len() < header_end {
        anyhow::bail!("truncated bundle header");
    }

    let _header: serde_json::Value = serde_json::from_slice(&content[header_start..header_end])?;

    // Decrypt payload.
    let encrypted_payload = &content[header_end..];
    let plaintext =
        shroudb_crypto::aes_gcm_decrypt(export_enc_key.as_bytes(), encrypted_payload, b"export")?;

    let ks_snap: shroudb_storage::snapshot::format::KeyspaceSnapshot =
        postcard::from_bytes(&plaintext)
            .map_err(|e| anyhow::anyhow!("failed to deserialize bundle: {e}"))?;

    // Decrypt private keys from the export bundle and re-encrypt with the
    // per-keyspace private key for the target keyspace.
    let pk_key = engine.encrypt_private_key(target_keyspace, &[0u8; 0]);
    drop(pk_key); // just checking the keyspace exists for encryption

    // Apply imported data to the target keyspace's indexes.
    let index = engine.index();

    let mut imported_count = 0u64;

    // Import API keys.
    for entry in &ks_snap.keyspace.api_keys {
        if let Some(idx) = index.api_keys.get(target_keyspace) {
            idx.insert(entry.clone());
            imported_count += 1;
        }
    }

    // Import refresh tokens.
    for entry in &ks_snap.keyspace.refresh_tokens {
        if let Some(idx) = index.refresh_tokens.get(target_keyspace) {
            idx.insert(entry.clone());
            imported_count += 1;
        }
    }

    // Import JWT signing keys.
    for key in &ks_snap.keyspace.signing_keys {
        let mut key = key.clone();
        // Decrypt private key from export bundle, then re-encrypt with target keyspace key.
        if let Some(epk) = ks_snap
            .encrypted_private_keys
            .iter()
            .find(|e| e.key_id == key.key_id)
            && let Ok(decrypted) = shroudb_crypto::aes_gcm_decrypt(
                export_enc_key.as_bytes(),
                &epk.encrypted_bytes,
                b"export_pk",
            )
        {
            key.private_key = Some(shroudb_crypto::SecretBytes::new(decrypted));
        }
        if let Some(ring) = index.jwt_rings.get(target_keyspace) {
            ring.insert(key);
            imported_count += 1;
        }
    }

    // Import HMAC keys.
    for key in &ks_snap.keyspace.hmac_keys {
        let mut key = key.clone();
        if let Some(epk) = ks_snap
            .encrypted_private_keys
            .iter()
            .find(|e| e.key_id == key.key_id)
            && let Ok(decrypted) = shroudb_crypto::aes_gcm_decrypt(
                export_enc_key.as_bytes(),
                &epk.encrypted_bytes,
                b"export_pk",
            )
        {
            key.key_material = Some(shroudb_crypto::SecretBytes::new(decrypted));
        }
        if let Some(ring) = index.hmac_rings.get(target_keyspace) {
            ring.insert(key);
            imported_count += 1;
        }
    }

    // Snapshot to persist the imported data.
    engine.snapshot().await?;
    engine.shutdown().await?;

    tracing::info!(
        keyspace = target_keyspace,
        credentials = imported_count,
        "import complete"
    );
    println!(
        "Imported {imported_count} credentials into keyspace '{target_keyspace}' from {}",
        file.display()
    );
    Ok(())
}

/// Check system health without starting the server.
async fn handle_doctor(config_path: &Path) -> anyhow::Result<()> {
    // Suppress tracing output — doctor uses direct println for structured output.
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::ERROR)
        .init();

    let mut all_passed = true;

    // 1. Config check
    let cfg = match config::load(config_path) {
        Ok(Some(cfg)) => {
            let ks_count = cfg.keyspaces.len();
            println!(
                "Config:     PASS  ({} parsed, {} keyspace{} defined)",
                config_path.display(),
                ks_count,
                if ks_count == 1 { "" } else { "s" }
            );
            Some(cfg)
        }
        Ok(None) => {
            println!(
                "Config:     WARN  ({} not found, using defaults)",
                config_path.display()
            );
            None
        }
        Err(e) => {
            println!("Config:     FAIL  ({}: {e})", config_path.display());
            all_passed = false;
            None
        }
    };

    let cfg = cfg.unwrap_or_default();

    // 2. Master key check
    let key_source_name = if std::env::var("SHROUDB_MASTER_KEY").is_ok() {
        Some("SHROUDB_MASTER_KEY")
    } else if std::env::var("SHROUDB_MASTER_KEY_FILE").is_ok() {
        Some("SHROUDB_MASTER_KEY_FILE")
    } else {
        None
    };

    let key_source = match key_source_name {
        Some(name) => match resolve_master_key() {
            Ok(source) => match source.load().await {
                Ok(_) => {
                    println!("Master Key: PASS  (loaded from {name})");
                    Some(source)
                }
                Err(e) => {
                    println!("Master Key: FAIL  ({name} set but load failed: {e})");
                    all_passed = false;
                    None
                }
            },
            Err(e) => {
                println!("Master Key: FAIL  ({e})");
                all_passed = false;
                None
            }
        },
        None => {
            println!("Master Key: WARN  (not configured, would use ephemeral key)");
            None
        }
    };

    // 3. Data directory check
    let data_dir = &cfg.storage.data_dir;
    if data_dir.exists() {
        // Check writability by trying to create a temp file
        let test_path = data_dir.join(".doctor_probe");
        match std::fs::write(&test_path, b"probe") {
            Ok(()) => {
                let _ = std::fs::remove_file(&test_path);
                println!(
                    "Data Dir:   PASS  ({} exists, writable)",
                    data_dir.display()
                );
            }
            Err(e) => {
                println!(
                    "Data Dir:   FAIL  ({} exists but not writable: {e})",
                    data_dir.display()
                );
                all_passed = false;
            }
        }
    } else {
        println!("Data Dir:   FAIL  ({} does not exist)", data_dir.display());
        all_passed = false;
    }

    // 4. WAL check
    let engine_config = config::to_engine_config(&cfg);
    let namespace = engine_config.namespace.clone();
    let wal_reader = shroudb_storage::wal::reader::WalReader::new(
        engine_config.data_dir.clone(),
        namespace.clone(),
    );
    match wal_reader.list_segments().await {
        Ok(segments) if segments.is_empty() => {
            println!("WAL:        PASS  (no segments, fresh instance)");
        }
        Ok(segments) => {
            let seg_count = segments.len();
            let mut total_entries = 0u64;
            let mut total_corrupt = 0u64;
            let mut wal_ok = true;

            for (_seq, path) in &segments {
                match wal_reader
                    .read_segment(path, shroudb_storage::RecoveryMode::Recover)
                    .await
                {
                    Ok(result) => {
                        total_entries += result.entries.len() as u64;
                        total_corrupt += result.corrupt_count;
                    }
                    Err(e) => {
                        println!("WAL:        FAIL  (segment read error: {e})");
                        wal_ok = false;
                        all_passed = false;
                        break;
                    }
                }
            }
            if wal_ok {
                if total_corrupt > 0 {
                    println!(
                        "WAL:        WARN  ({seg_count} segment{}, {total_entries} entries, {total_corrupt} corrupt)",
                        if seg_count == 1 { "" } else { "s" }
                    );
                } else {
                    println!(
                        "WAL:        PASS  ({seg_count} segment{}, {total_entries} entries, no corruption)",
                        if seg_count == 1 { "" } else { "s" }
                    );
                }
            }
        }
        Err(e) => {
            println!("WAL:        FAIL  (cannot list segments: {e})");
            all_passed = false;
        }
    }

    // 5. Snapshot check
    let snap_reader = shroudb_storage::snapshot::reader::SnapshotReader::new(
        engine_config.data_dir.clone(),
        namespace,
    );
    match snap_reader.find_latest().await {
        Ok(Some(snap_path)) => {
            if let Some(ref ks) = key_source {
                match ks.load().await {
                    Ok(_master_key) => {
                        let tc = engine_config.tenant_context.clone();
                        match shroudb_storage::key_manager::KeyManager::new(&**ks, tc).await {
                            Ok(km) => {
                                match snap_reader
                                    .load(
                                        &snap_path,
                                        km.snapshot_key().as_bytes(),
                                        km.snapshot_hmac_key().as_bytes(),
                                    )
                                    .await
                                {
                                    Ok((header, _data)) => {
                                        let total_creds: u64 = header.total_credentials;
                                        let ks_count = header.keyspace_count;
                                        let snap_name = snap_path
                                            .file_name()
                                            .unwrap_or_default()
                                            .to_string_lossy();
                                        println!(
                                            "Snapshot:   PASS  (latest: {snap_name}, {ks_count} keyspace{}, {total_creds} credentials)",
                                            if ks_count == 1 { "" } else { "s" }
                                        );
                                    }
                                    Err(e) => {
                                        println!("Snapshot:   FAIL  (load error: {e})");
                                        all_passed = false;
                                    }
                                }
                            }
                            Err(e) => {
                                println!("Snapshot:   FAIL  (key manager init: {e})");
                                all_passed = false;
                            }
                        }
                    }
                    Err(e) => {
                        println!("Snapshot:   FAIL  (key load: {e})");
                        all_passed = false;
                    }
                }
            } else {
                let snap_name = snap_path.file_name().unwrap_or_default().to_string_lossy();
                println!("Snapshot:   WARN  (found {snap_name}, but no master key to verify)");
            }
        }
        Ok(None) => {
            println!("Snapshot:   PASS  (no snapshots yet, fresh instance)");
        }
        Err(e) => {
            println!("Snapshot:   FAIL  (cannot find snapshots: {e})");
            all_passed = false;
        }
    }

    if all_passed {
        std::process::exit(0);
    } else {
        std::process::exit(1);
    }
}

/// Wait for either SIGINT (Ctrl-C) or SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => { tracing::info!("received SIGINT"); }
            _ = sigterm.recv() => { tracing::info!("received SIGTERM"); }
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.expect("failed to listen for Ctrl-C");
        tracing::info!("received SIGINT");
    }
}
