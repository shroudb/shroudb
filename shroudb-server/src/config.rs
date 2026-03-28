use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;
use shroudb_acl::{Scope, StaticTokenValidator, Token, TokenGrant};
use shroudb_storage::{StorageEngineConfig, wal::writer::FsyncMode};
use shroudb_store::Namespace;

// ---------------------------------------------------------------------------
// TOML config structs
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct ShrouDBConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub auth: Option<AuthConfig>,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,
    #[serde(default)]
    pub tls_cert: Option<PathBuf>,
    #[serde(default)]
    pub tls_key: Option<PathBuf>,
    #[serde(default)]
    pub tls_client_ca: Option<PathBuf>,
    #[serde(default)]
    pub unix_socket: Option<PathBuf>,
    #[serde(default)]
    pub rate_limit_per_second: Option<u32>,
    #[serde(default)]
    pub metrics_bind: Option<SocketAddr>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            tls_cert: None,
            tls_key: None,
            tls_client_ca: None,
            unix_socket: None,
            rate_limit_per_second: None,
            metrics_bind: None,
        }
    }
}

fn default_bind() -> SocketAddr {
    "0.0.0.0:6399".parse().unwrap()
}

#[derive(Debug, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub fsync_mode: Option<String>,
    #[serde(default)]
    pub fsync_interval_ms: Option<u64>,
    #[serde(default)]
    pub max_segment_bytes: Option<u64>,
    #[serde(default)]
    pub max_segment_entries: Option<u64>,
    #[serde(default)]
    pub snapshot_interval_entries: Option<u64>,
    #[serde(default)]
    pub snapshot_interval_minutes: Option<u64>,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            namespace: None,
            fsync_mode: None,
            fsync_interval_ms: None,
            max_segment_bytes: None,
            max_segment_entries: None,
            snapshot_interval_entries: None,
            snapshot_interval_minutes: None,
        }
    }
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("./data")
}

#[derive(Debug, Deserialize)]
pub struct AuthConfig {
    /// Auth method. Currently only "token" is supported.
    #[serde(default)]
    pub method: Option<String>,
    /// Token-to-policy mappings.
    #[serde(default)]
    pub tokens: HashMap<String, TokenConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenConfig {
    /// Tenant ID this token is scoped to.
    pub tenant: String,
    /// Human-readable actor name for audit trail.
    #[serde(default = "default_actor")]
    pub actor: String,
    /// Whether this is a platform/superuser token.
    #[serde(default)]
    pub platform: bool,
    /// Namespace-scoped grants.
    #[serde(default)]
    pub grants: Vec<GrantConfig>,
}

fn default_actor() -> String {
    "anonymous".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct GrantConfig {
    pub namespace: String,
    #[serde(default)]
    pub scopes: Vec<String>,
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

/// Load and parse a TOML config file with environment variable expansion.
pub fn load_config(path: &Path) -> anyhow::Result<ShrouDBConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file: {}", path.display()))?;
    let expanded = expand_env_vars(&raw);
    let config: ShrouDBConfig =
        toml::from_str(&expanded).with_context(|| "failed to parse config file")?;
    Ok(config)
}

/// Expand `${VAR}` patterns in a string with environment variable values.
fn expand_env_vars(input: &str) -> String {
    let mut result = input.to_string();
    while let Some(start) = result.find("${") {
        if let Some(end) = result[start..].find('}') {
            let var_name = &result[start + 2..start + end];
            let replacement = std::env::var(var_name).unwrap_or_default();
            result = format!(
                "{}{}{}",
                &result[..start],
                replacement,
                &result[start + end + 1..]
            );
        } else {
            break;
        }
    }
    result
}

/// Build a StorageEngineConfig from the parsed TOML.
pub fn to_engine_config(config: &ShrouDBConfig) -> StorageEngineConfig {
    let fsync_mode = to_fsync_mode(&config.storage);

    StorageEngineConfig {
        data_dir: config.storage.data_dir.clone(),
        namespace: Namespace::new(config.storage.namespace.as_deref().unwrap_or("default")),
        fsync_mode,
        max_segment_bytes: config.storage.max_segment_bytes.unwrap_or(64 * 1024 * 1024),
        max_segment_entries: config.storage.max_segment_entries.unwrap_or(100_000),
        snapshot_entry_threshold: config.storage.snapshot_interval_entries.unwrap_or(100_000),
        snapshot_time_threshold_secs: config
            .storage
            .snapshot_interval_minutes
            .map(|m| m * 60)
            .unwrap_or(3600),
        ..Default::default()
    }
}

fn to_fsync_mode(storage: &StorageConfig) -> FsyncMode {
    match storage.fsync_mode.as_deref() {
        Some("per_write") => FsyncMode::PerWrite,
        Some("batched") => FsyncMode::Batched {
            interval_ms: storage.fsync_interval_ms.unwrap_or(10),
        },
        Some("periodic") => FsyncMode::Periodic {
            interval_ms: storage.fsync_interval_ms.unwrap_or(1000),
        },
        _ => FsyncMode::default(),
    }
}

/// Build a StaticTokenValidator from the auth config.
pub fn build_token_validator(config: &ShrouDBConfig) -> StaticTokenValidator {
    let mut validator = StaticTokenValidator::new();

    if let Some(auth) = &config.auth {
        for (raw_token, token_config) in &auth.tokens {
            let grants: Vec<TokenGrant> = token_config
                .grants
                .iter()
                .map(|g| {
                    let scopes: Vec<Scope> = g
                        .scopes
                        .iter()
                        .filter_map(|s| match s.to_lowercase().as_str() {
                            "read" => Some(Scope::Read),
                            "write" => Some(Scope::Write),
                            _ => {
                                tracing::warn!(scope = %s, "unknown scope in token config, ignoring");
                                None
                            }
                        })
                        .collect();
                    TokenGrant {
                        namespace: g.namespace.clone(),
                        scopes,
                    }
                })
                .collect();

            let token = Token {
                tenant: token_config.tenant.clone(),
                actor: token_config.actor.clone(),
                is_platform: token_config.platform,
                grants,
                expires_at: None,
            };

            validator.register(raw_token.clone(), token);
        }
    }

    validator
}

/// Whether auth is required (method = "token" and at least one token defined).
pub fn auth_required(config: &ShrouDBConfig) -> bool {
    config
        .auth
        .as_ref()
        .is_some_and(|auth| auth.method.as_deref() == Some("token") && !auth.tokens.is_empty())
}
