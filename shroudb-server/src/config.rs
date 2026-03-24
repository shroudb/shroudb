use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;
use shroudb_core::{
    FieldDef, FieldType, HashParams, Keyspace, KeyspacePolicy, KeyspaceType, MetaSchema,
    MetadataValue, Namespace, PasswordAlgorithm, UnixTimestamp,
};
use shroudb_crypto::{HmacAlgorithm, JwtAlgorithm};
use shroudb_protocol::auth::{AuthPolicy, AuthRegistry};
use shroudb_storage::{StorageEngineConfig, wal::writer::FsyncMode};

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
    pub keyspaces: HashMap<String, KeyspaceConfig>,
    #[serde(default)]
    pub auth: Option<AuthConfig>,
    #[serde(default)]
    pub webhooks: Option<Vec<WebhookEndpointConfig>>,
}

/// Webhook endpoint configuration (parsed from TOML).
#[derive(Debug, Clone, Deserialize)]
pub struct WebhookEndpointConfig {
    /// Target URL to deliver webhook events to.
    pub url: String,
    /// HMAC secret used to sign the payload.
    pub secret: String,
    /// Event types to subscribe to. Empty = all events.
    #[serde(default)]
    pub events: Vec<String>,
    /// Maximum delivery retry attempts.
    #[serde(default = "default_webhook_max_retries")]
    pub max_retries: u32,
}

fn default_webhook_max_retries() -> u32 {
    3
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,
    #[serde(default)]
    pub tls_cert: Option<PathBuf>,
    #[serde(default)]
    pub tls_key: Option<PathBuf>,
    #[serde(default)]
    pub unix_socket: Option<PathBuf>,
    /// CA certificate for verifying client certificates (enables mTLS).
    #[serde(default)]
    pub tls_client_ca: Option<PathBuf>,
    /// REST API bind address (not yet implemented; accepted for forward-compatibility).
    #[serde(default)]
    pub rest_bind: Option<SocketAddr>,
    /// gRPC bind address (not yet implemented; accepted for forward-compatibility).
    #[serde(default)]
    pub grpc_bind: Option<SocketAddr>,
    /// Per-connection rate limit in commands per second. None = no limit.
    #[serde(default)]
    pub rate_limit: Option<u32>,
    /// Prometheus metrics scrape endpoint. Default `0.0.0.0:9090`.
    #[serde(default = "default_metrics_bind")]
    pub metrics_bind: Option<SocketAddr>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            tls_cert: None,
            tls_key: None,
            unix_socket: None,
            tls_client_ca: None,
            rest_bind: None,
            grpc_bind: None,
            rate_limit: None,
            metrics_bind: default_metrics_bind(),
        }
    }
}

fn default_metrics_bind() -> Option<SocketAddr> {
    Some("0.0.0.0:9090".parse().unwrap())
}

#[derive(Debug, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_fsync_mode")]
    pub wal_fsync_mode: String,
    #[serde(default = "default_fsync_interval")]
    pub wal_fsync_interval_ms: u64,
    #[serde(default = "default_segment_size")]
    pub wal_segment_max_bytes: u64,
    #[serde(default = "default_snapshot_entries")]
    pub snapshot_interval_entries: u64,
    #[serde(default = "default_snapshot_minutes")]
    pub snapshot_interval_minutes: u64,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            wal_fsync_mode: default_fsync_mode(),
            wal_fsync_interval_ms: default_fsync_interval(),
            wal_segment_max_bytes: default_segment_size(),
            snapshot_interval_entries: default_snapshot_entries(),
            snapshot_interval_minutes: default_snapshot_minutes(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct KeyspaceConfig {
    #[serde(rename = "type")]
    pub ks_type: String,
    pub algorithm: Option<String>,
    pub rotation_days: Option<u32>,
    pub drain_days: Option<u32>,
    pub pre_stage_days: Option<u32>,
    pub default_ttl: Option<String>,
    pub required_claims: Option<serde_json::Value>,
    pub verify_cache_ttl: Option<String>,
    pub prefix: Option<String>,
    pub hash_algorithm: Option<String>,
    pub token_ttl: Option<String>,
    pub max_chain_length: Option<u32>,
    pub family_ttl: Option<String>,
    pub leeway: Option<u64>,
    pub disabled: Option<bool>,
    pub meta_schema: Option<MetaSchemaConfig>,
    // Password keyspace settings
    pub max_failed_attempts: Option<u32>,
    pub lockout_duration: Option<String>,
    pub m_cost: Option<u32>,
    pub t_cost: Option<u32>,
    pub p_cost: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct MetaSchemaConfig {
    #[serde(default)]
    pub enforce: bool,
    #[serde(default)]
    pub fields: HashMap<String, FieldDefConfig>,
}

#[derive(Debug, Deserialize)]
pub struct FieldDefConfig {
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    #[serde(default, rename = "enum")]
    pub enum_values: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub immutable: bool,
    #[serde(default)]
    pub items: Option<String>,
}

// ---------------------------------------------------------------------------
// Serde defaults
// ---------------------------------------------------------------------------

fn default_bind() -> SocketAddr {
    "0.0.0.0:6399".parse().unwrap()
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("./data")
}

fn default_fsync_mode() -> String {
    "batched".to_string()
}

fn default_fsync_interval() -> u64 {
    10
}

fn default_segment_size() -> u64 {
    64 * 1024 * 1024
}

fn default_snapshot_entries() -> u64 {
    100_000
}

fn default_snapshot_minutes() -> u64 {
    60
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Load config from a TOML file. Returns None if the file doesn't exist.
///
/// All `${VAR}` patterns in the TOML are expanded from environment variables
/// before parsing, so env-var interpolation works for every config value.
pub fn load(path: &Path) -> anyhow::Result<Option<ShrouDBConfig>> {
    if !path.exists() {
        return Ok(None);
    }
    let contents =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let expanded = expand_env_vars(&contents);
    let config: ShrouDBConfig =
        toml::from_str(&expanded).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(config))
}

/// Expand all `${VAR}` patterns in a string with environment variable values.
/// If a variable is not set, the original `${VAR}` text is preserved.
fn expand_env_vars(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let mut var_name = String::new();
            let mut found_closing = false;
            for c in chars.by_ref() {
                if c == '}' {
                    found_closing = true;
                    break;
                }
                var_name.push(c);
            }
            if !found_closing {
                // Unclosed `${` — output the original characters as-is
                result.push_str("${");
                result.push_str(&var_name);
            } else {
                match std::env::var(&var_name) {
                    Ok(val) => result.push_str(&val),
                    Err(_) => {
                        // Leave the original ${VAR} if not set
                        result.push_str("${");
                        result.push_str(&var_name);
                        result.push('}');
                    }
                }
            }
        } else {
            result.push(ch);
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

pub fn to_engine_config(config: &ShrouDBConfig) -> StorageEngineConfig {
    let fsync_mode = to_fsync_mode(
        &config.storage.wal_fsync_mode,
        config.storage.wal_fsync_interval_ms,
    );
    StorageEngineConfig {
        data_dir: config.storage.data_dir.clone(),
        fsync_mode,
        max_segment_bytes: config.storage.wal_segment_max_bytes,
        snapshot_entry_threshold: config.storage.snapshot_interval_entries,
        snapshot_time_threshold_secs: config.storage.snapshot_interval_minutes * 60,
        ..StorageEngineConfig::default()
    }
}

pub fn to_fsync_mode(mode: &str, interval_ms: u64) -> FsyncMode {
    match mode {
        "per_write" => FsyncMode::PerWrite,
        "batched" => FsyncMode::Batched { interval_ms },
        "periodic" => FsyncMode::Periodic { interval_ms },
        _ => {
            tracing::warn!(mode, "unknown wal_fsync_mode, defaulting to batched");
            FsyncMode::Batched { interval_ms }
        }
    }
}

pub fn to_keyspace(name: &str, ks_config: &KeyspaceConfig) -> anyhow::Result<Keyspace> {
    let (keyspace_type, policy) = match ks_config.ks_type.as_str() {
        "jwt" => {
            let algorithm = parse_jwt_algorithm(ks_config.algorithm.as_deref().unwrap_or("ES256"))?;
            let default_ttl_secs = ks_config
                .default_ttl
                .as_deref()
                .map(parse_duration)
                .transpose()?
                .unwrap_or(900);
            let verify_cache_ttl_secs = ks_config
                .verify_cache_ttl
                .as_deref()
                .map(parse_duration)
                .transpose()?;

            (
                KeyspaceType::Jwt,
                KeyspacePolicy::Jwt {
                    algorithm,
                    rotation_days: ks_config.rotation_days.unwrap_or(90),
                    drain_days: ks_config.drain_days.unwrap_or(30),
                    pre_stage_days: ks_config.pre_stage_days.unwrap_or(7),
                    default_ttl_secs,
                    required_claims: ks_config
                        .required_claims
                        .as_ref()
                        .map(|v| shroudb_core::metadata_from_json(v.clone()))
                        .transpose()
                        .map_err(|e| anyhow::anyhow!("invalid required_claims: {e}"))?,
                    verify_cache_ttl_secs,
                    leeway_secs: ks_config.leeway.unwrap_or(30),
                },
            )
        }
        "api_key" => (
            KeyspaceType::ApiKey,
            KeyspacePolicy::ApiKey {
                prefix: ks_config.prefix.clone(),
            },
        ),
        "hmac" => {
            let algorithm =
                parse_hmac_algorithm(ks_config.algorithm.as_deref().unwrap_or("sha256"))?;
            (
                KeyspaceType::Hmac,
                KeyspacePolicy::Hmac {
                    algorithm,
                    rotation_days: ks_config.rotation_days.unwrap_or(180),
                    drain_days: ks_config.drain_days.unwrap_or(14),
                },
            )
        }
        "refresh_token" => {
            let token_ttl_secs = ks_config
                .token_ttl
                .as_deref()
                .map(parse_duration)
                .transpose()?
                .unwrap_or(30 * 86400);
            let family_ttl_secs = ks_config
                .family_ttl
                .as_deref()
                .map(parse_duration)
                .transpose()?
                .unwrap_or(90 * 86400);
            (
                KeyspaceType::RefreshToken,
                KeyspacePolicy::RefreshToken {
                    token_ttl_secs,
                    max_chain_length: ks_config.max_chain_length.unwrap_or(100),
                    family_ttl_secs,
                },
            )
        }
        "password" => {
            let lockout_secs = ks_config
                .lockout_duration
                .as_deref()
                .map(parse_duration)
                .transpose()?
                .unwrap_or(900);
            (
                KeyspaceType::Password,
                KeyspacePolicy::Password {
                    algorithm: PasswordAlgorithm::Argon2id,
                    hash_params: HashParams {
                        m_cost: ks_config.m_cost.unwrap_or(19456),
                        t_cost: ks_config.t_cost.unwrap_or(2),
                        p_cost: ks_config.p_cost.unwrap_or(1),
                    },
                    max_failed_attempts: ks_config.max_failed_attempts.unwrap_or(5),
                    lockout_duration_secs: lockout_secs,
                },
            )
        }
        other => bail!("unknown keyspace type: {other}"),
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let meta_schema = ks_config
        .meta_schema
        .as_ref()
        .map(to_meta_schema)
        .transpose()
        .with_context(|| format!("invalid meta_schema for keyspace '{name}'"))?;

    Ok(Keyspace {
        name: name.to_string(),
        namespace: Namespace::default(),
        keyspace_type,
        policy,
        disabled: ks_config.disabled.unwrap_or(false),
        meta_schema,
        created_at: now as UnixTimestamp,
        signing_keys: Vec::new(),
        api_keys: Vec::new(),
        hmac_keys: Vec::new(),
        refresh_tokens: Vec::new(),
        password_entries: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// Meta-schema config conversion
// ---------------------------------------------------------------------------

pub fn to_meta_schema(config: &MetaSchemaConfig) -> anyhow::Result<MetaSchema> {
    let mut fields = Vec::new();
    for (name, field_cfg) in &config.fields {
        let field_type = parse_field_type(&field_cfg.field_type)?;
        let items = field_cfg
            .items
            .as_deref()
            .map(parse_field_type)
            .transpose()?;

        // Validate schema definition
        if field_type != FieldType::Array && items.is_some() {
            bail!("field '{name}': 'items' is only valid on array fields");
        }
        if field_type == FieldType::Array && items.is_none() {
            bail!("field '{name}': array fields must specify 'items' element type");
        }

        // Validate default value type matches field type
        if let Some(ref default) = field_cfg.default {
            validate_default_type(name, field_type, default)?;
        }

        // Validate enum values type matches
        if let Some(ref enum_vals) = field_cfg.enum_values {
            for val in enum_vals {
                validate_default_type(name, field_type, val)?;
            }
        }

        fields.push(FieldDef {
            name: name.clone(),
            field_type,
            required: field_cfg.required,
            default: field_cfg
                .default
                .as_ref()
                .map(|v| MetadataValue::from_json(v.clone())),
            enum_values: field_cfg.enum_values.as_ref().map(|vals| {
                vals.iter()
                    .map(|v| MetadataValue::from_json(v.clone()))
                    .collect()
            }),
            min: field_cfg.min,
            max: field_cfg.max,
            immutable: field_cfg.immutable,
            items,
        });
    }
    Ok(MetaSchema {
        enforce: config.enforce,
        fields,
    })
}

fn parse_field_type(s: &str) -> anyhow::Result<FieldType> {
    match s {
        "string" => Ok(FieldType::String),
        "integer" => Ok(FieldType::Integer),
        "float" => Ok(FieldType::Float),
        "boolean" => Ok(FieldType::Boolean),
        "array" => Ok(FieldType::Array),
        _ => bail!("unknown field type: '{s}' (expected string/integer/float/boolean/array)"),
    }
}

fn validate_default_type(
    field_name: &str,
    field_type: FieldType,
    value: &serde_json::Value,
) -> anyhow::Result<()> {
    let valid = match field_type {
        FieldType::String => value.is_string(),
        FieldType::Integer => value.is_i64() || value.is_u64(),
        FieldType::Float => value.is_f64(),
        FieldType::Boolean => value.is_boolean(),
        FieldType::Array => value.is_array(),
    };
    if !valid {
        bail!(
            "field '{field_name}': default/enum value type doesn't match field type '{field_type:?}'"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Duration parsing: "15m", "30d", "1h", "90s", "24h", etc.
// ---------------------------------------------------------------------------

pub fn parse_duration(s: &str) -> anyhow::Result<u64> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty duration string");
    }

    let (num_part, suffix) = s.split_at(s.len() - 1);
    let value: u64 = num_part
        .parse()
        .with_context(|| format!("invalid duration number in '{s}'"))?;

    let secs = match suffix {
        "s" => value,
        "m" => value * 60,
        "h" => value * 3600,
        "d" => value * 86400,
        _ => bail!("unknown duration suffix '{suffix}' in '{s}' (expected s/m/h/d)"),
    };

    Ok(secs)
}

// ---------------------------------------------------------------------------
// Auth configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct AuthConfig {
    #[serde(default = "default_auth_method")]
    pub method: String,
    #[serde(default)]
    pub policies: HashMap<String, AuthPolicyConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthPolicyConfig {
    pub token: String,
    pub keyspaces: Vec<String>,
    pub commands: Vec<String>,
}

fn default_auth_method() -> String {
    "token".to_string()
}

pub fn build_auth_registry(auth: &Option<AuthConfig>) -> AuthRegistry {
    match auth {
        None => AuthRegistry::permissive(),
        Some(cfg) if cfg.method == "none" => AuthRegistry::permissive(),
        Some(cfg) => {
            let mut policies = HashMap::new();
            for (name, policy_cfg) in &cfg.policies {
                policies.insert(
                    policy_cfg.token.clone(),
                    AuthPolicy {
                        name: name.clone(),
                        keyspaces: policy_cfg.keyspaces.clone(),
                        commands: policy_cfg.commands.clone(),
                    },
                );
            }
            AuthRegistry::new(policies, true)
        }
    }
}

fn parse_jwt_algorithm(s: &str) -> anyhow::Result<JwtAlgorithm> {
    match s {
        "ES256" => Ok(JwtAlgorithm::ES256),
        "ES384" => Ok(JwtAlgorithm::ES384),
        "RS256" => Ok(JwtAlgorithm::RS256),
        "RS384" => Ok(JwtAlgorithm::RS384),
        "RS512" => Ok(JwtAlgorithm::RS512),
        "EdDSA" => Ok(JwtAlgorithm::EdDSA),
        _ => bail!("unknown JWT algorithm: {s}"),
    }
}

fn parse_hmac_algorithm(s: &str) -> anyhow::Result<HmacAlgorithm> {
    match s.to_lowercase().as_str() {
        "sha256" => Ok(HmacAlgorithm::Sha256),
        "sha384" => Ok(HmacAlgorithm::Sha384),
        "sha512" => Ok(HmacAlgorithm::Sha512),
        _ => bail!("unknown HMAC algorithm: {s}"),
    }
}

/// Convert config-level webhook endpoint configs to protocol-level configs.
pub fn to_webhook_configs(
    configs: &Option<Vec<WebhookEndpointConfig>>,
) -> Vec<shroudb_protocol::webhooks::WebhookConfig> {
    configs
        .as_ref()
        .map(|cfgs| {
            cfgs.iter()
                .map(|c| shroudb_protocol::webhooks::WebhookConfig {
                    url: c.url.clone(),
                    secret: c.secret.clone(),
                    events: c.events.clone(),
                    max_retries: c.max_retries,
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_minutes() {
        assert_eq!(parse_duration("15m").unwrap(), 900);
    }

    #[test]
    fn parse_duration_days() {
        assert_eq!(parse_duration("30d").unwrap(), 2_592_000);
    }

    #[test]
    fn parse_duration_hours() {
        assert_eq!(parse_duration("1h").unwrap(), 3600);
    }

    #[test]
    fn parse_duration_seconds() {
        assert_eq!(parse_duration("90s").unwrap(), 90);
    }

    #[test]
    fn parse_duration_invalid() {
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("").is_err());
        assert!(parse_duration("10x").is_err());
    }

    #[test]
    fn default_config_parses() {
        let cfg = ShrouDBConfig::default();
        assert_eq!(cfg.server.bind, default_bind());
        assert_eq!(cfg.storage.data_dir, default_data_dir());
        assert!(cfg.keyspaces.is_empty());
    }

    #[test]
    fn minimal_toml_parses() {
        let toml_str = r#"
[keyspaces.tokens]
type = "jwt"
"#;
        let cfg: ShrouDBConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.server.bind, default_bind());
        assert!(cfg.keyspaces.contains_key("tokens"));
    }

    #[test]
    fn expand_env_vars_works() {
        // SAFETY: test-only; no other threads rely on this variable.
        unsafe {
            std::env::set_var("TEST_EXPAND", "hello");
        }
        assert_eq!(
            expand_env_vars("prefix_${TEST_EXPAND}_suffix"),
            "prefix_hello_suffix"
        );
        assert_eq!(expand_env_vars("no_vars_here"), "no_vars_here");
        assert_eq!(
            expand_env_vars("${NONEXISTENT_VAR_XYZ}"),
            "${NONEXISTENT_VAR_XYZ}"
        );
        // SAFETY: test-only cleanup.
        unsafe {
            std::env::remove_var("TEST_EXPAND");
        }
    }

    #[test]
    fn meta_schema_config_parses() {
        let toml_str = r#"
[keyspaces.test]
type = "api_key"

[keyspaces.test.meta_schema]
enforce = true

[keyspaces.test.meta_schema.fields.org_id]
type = "string"
required = true
immutable = true

[keyspaces.test.meta_schema.fields.plan]
type = "string"
required = true
enum = ["free", "pro"]

[keyspaces.test.meta_schema.fields.tags]
type = "array"
items = "string"
min = 1
"#;
        let cfg: ShrouDBConfig = toml::from_str(toml_str).unwrap();
        let ks = to_keyspace("test", &cfg.keyspaces["test"]).unwrap();
        let schema = ks.meta_schema.unwrap();
        assert!(schema.enforce);
        assert_eq!(schema.fields.len(), 3);
    }

    #[test]
    fn meta_schema_invalid_items_on_non_array() {
        let toml_str = r#"
[keyspaces.test]
type = "api_key"

[keyspaces.test.meta_schema]
enforce = true

[keyspaces.test.meta_schema.fields.name]
type = "string"
items = "string"
"#;
        let cfg: ShrouDBConfig = toml::from_str(toml_str).unwrap();
        assert!(to_keyspace("test", &cfg.keyspaces["test"]).is_err());
    }

    #[test]
    fn meta_schema_invalid_default_type() {
        let toml_str = r#"
[keyspaces.test]
type = "api_key"

[keyspaces.test.meta_schema]
enforce = true

[keyspaces.test.meta_schema.fields.count]
type = "integer"
default = "not a number"
"#;
        let cfg: ShrouDBConfig = toml::from_str(toml_str).unwrap();
        assert!(to_keyspace("test", &cfg.keyspaces["test"]).is_err());
    }
}
