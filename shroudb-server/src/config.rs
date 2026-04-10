use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;
use shroudb_acl::{Scope, ServerAuthConfig, StaticTokenValidator, Token, TokenGrant};
use shroudb_storage::engine::CacheMemoryBudget;
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
    pub auth: Option<ServerAuthConfig>,
    #[serde(default)]
    pub webhooks: Vec<WebhookConfig>,
}

/// Configuration for a single webhook endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct WebhookConfig {
    /// URL to POST events to.
    pub url: String,
    /// HMAC-SHA256 secret for signing payloads. Sent in `X-ShrouDB-Signature-256` header.
    pub secret: String,
    /// Event types to deliver. Empty = all events.
    #[serde(default)]
    pub events: Vec<String>,
    /// Namespace patterns to match. Empty = all namespaces.
    /// Supports exact match and trailing `*` wildcard (e.g. `"myapp.*"`).
    #[serde(default)]
    pub namespaces: Vec<String>,
    /// Maximum delivery retries (default: 5).
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// HTTP request timeout in milliseconds (default: 5000).
    #[serde(default = "default_webhook_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_max_retries() -> u32 {
    5
}

fn default_webhook_timeout_ms() -> u64 {
    5000
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,
    #[serde(default)]
    pub tls: Option<shroudb_server_tcp::TlsConfig>,
    #[serde(default)]
    pub unix_socket: Option<PathBuf>,
    #[serde(default)]
    pub rate_limit_per_second: Option<u32>,
    #[serde(default)]
    pub metrics_bind: Option<SocketAddr>,
    /// OpenTelemetry OTLP endpoint (e.g. `http://localhost:4317`).
    #[serde(default)]
    pub otel_endpoint: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            tls: None,
            unix_socket: None,
            rate_limit_per_second: None,
            metrics_bind: None,
            otel_endpoint: None,
        }
    }
}

fn default_bind() -> SocketAddr {
    "0.0.0.0:6399".parse().expect("valid hardcoded address")
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
    #[serde(default)]
    pub cache: Option<CacheConfig>,
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
            cache: None,
        }
    }
}

/// Configuration for the bounded KV index cache.
///
/// ```toml
/// [storage.cache]
/// memory_budget = "256mb"   # explicit byte limit
/// # memory_budget = "70%"   # fraction of system memory
/// # (omit for auto: 50% of system RAM, capped at 4 GiB)
/// ```
///
/// When this section is absent, the cache is unlimited (all values stay in memory).
#[derive(Debug, Deserialize)]
pub struct CacheConfig {
    /// Memory budget: `"256mb"`, `"1gb"`, `"70%"`, or `"auto"`.
    /// Absent = unlimited (no eviction).
    #[serde(default)]
    pub memory_budget: Option<String>,
}

/// Parse a memory budget string into a `CacheMemoryBudget`.
fn parse_memory_budget(input: &str) -> anyhow::Result<CacheMemoryBudget> {
    let input = input.trim().to_lowercase();

    if input == "auto" {
        return Ok(CacheMemoryBudget::Auto);
    }

    if let Some(pct) = input.strip_suffix('%') {
        let frac: f64 = pct
            .trim()
            .parse()
            .with_context(|| format!("invalid percentage: {input}"))?;
        if !(0.0..=100.0).contains(&frac) {
            anyhow::bail!("percentage must be 0-100, got: {frac}");
        }
        return Ok(CacheMemoryBudget::Fractional(frac / 100.0));
    }

    // Parse byte sizes: "256mb", "1gb", "512kb", or plain number (bytes)
    let (num_str, multiplier) = if let Some(n) = input.strip_suffix("gb") {
        (n, 1024 * 1024 * 1024)
    } else if let Some(n) = input.strip_suffix("mb") {
        (n, 1024 * 1024)
    } else if let Some(n) = input.strip_suffix("kb") {
        (n, 1024)
    } else {
        (input.as_str(), 1)
    };

    let num: f64 = num_str
        .trim()
        .parse()
        .with_context(|| format!("invalid memory budget: {input}"))?;
    let bytes = (num * multiplier as f64) as usize;
    Ok(CacheMemoryBudget::Explicit(bytes))
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("./data")
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
    let cache_memory_budget = to_cache_budget(&config.storage);

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
        cache_memory_budget,
        ..Default::default()
    }
}

fn to_cache_budget(storage: &StorageConfig) -> CacheMemoryBudget {
    let Some(cache) = &storage.cache else {
        return CacheMemoryBudget::Unlimited;
    };
    let Some(budget_str) = &cache.memory_budget else {
        // Section present but no budget specified → auto
        return CacheMemoryBudget::Auto;
    };
    match parse_memory_budget(budget_str) {
        Ok(budget) => budget,
        Err(e) => {
            tracing::warn!(
                input = %budget_str,
                error = %e,
                "invalid cache.memory_budget, falling back to unlimited"
            );
            CacheMemoryBudget::Unlimited
        }
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
        for (raw_token, tc) in &auth.tokens {
            let grants: Vec<TokenGrant> = tc
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
                tenant: tc.tenant.clone(),
                actor: tc.actor.clone(),
                is_platform: tc.platform,
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
