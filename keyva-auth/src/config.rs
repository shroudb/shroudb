//! Configuration for the Keyva Auth server.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;

use keyva_core::{
    HashParams, Keyspace, KeyspacePolicy, KeyspaceType, Namespace, PasswordAlgorithm, UnixTimestamp,
};
use keyva_crypto::JwtAlgorithm;
use keyva_storage::StorageEngineConfig;

// ---------------------------------------------------------------------------
// TOML config structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AuthServerConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub auth: AuthSettings,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub keyspaces: HashMap<String, AuthKeyspaceConfig>,
}

impl Default for AuthServerConfig {
    fn default() -> Self {
        let mut keyspaces = HashMap::new();
        keyspaces.insert(
            "default".to_string(),
            AuthKeyspaceConfig {
                algorithm: "ES256".to_string(),
                password_algorithm: "argon2id".to_string(),
                max_failed_attempts: 5,
                lockout_duration: "15m".to_string(),
            },
        );
        Self {
            server: ServerConfig::default(),
            auth: AuthSettings::default(),
            storage: StorageConfig::default(),
            keyspaces,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
        }
    }
}

fn default_bind() -> SocketAddr {
    "0.0.0.0:4001".parse().unwrap()
}

#[derive(Debug, Deserialize)]
pub struct AuthSettings {
    #[serde(default = "default_access_ttl")]
    pub access_ttl: String,
    #[serde(default = "default_refresh_ttl")]
    pub refresh_ttl: String,
    #[serde(default = "default_cookie_name")]
    pub cookie_name: String,
    #[serde(default)]
    pub cookie_domain: String,
    #[serde(default = "default_cookie_secure")]
    pub cookie_secure: bool,
    #[serde(default = "default_cors_origins")]
    pub cors_origins: Vec<String>,
    /// Max burst for per-IP login/signup rate limit (default: 10).
    #[serde(default)]
    pub login_rate_limit_burst: Option<u32>,
    /// Sustained requests/sec for per-IP login/signup rate limit (default: 2).
    #[serde(default)]
    pub login_rate_limit_per_sec: Option<u32>,
}

impl Default for AuthSettings {
    fn default() -> Self {
        Self {
            access_ttl: default_access_ttl(),
            refresh_ttl: default_refresh_ttl(),
            cookie_name: default_cookie_name(),
            cookie_domain: String::new(),
            cookie_secure: default_cookie_secure(),
            cors_origins: default_cors_origins(),
            login_rate_limit_burst: None,
            login_rate_limit_per_sec: None,
        }
    }
}

fn default_access_ttl() -> String {
    "15m".to_string()
}
fn default_refresh_ttl() -> String {
    "30d".to_string()
}
fn default_cookie_name() -> String {
    "keyva".to_string()
}
fn default_cookie_secure() -> bool {
    true
}
fn default_cors_origins() -> Vec<String> {
    vec!["*".to_string()]
}

#[derive(Debug, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
        }
    }
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("./auth-data")
}

#[derive(Debug, Deserialize)]
pub struct AuthKeyspaceConfig {
    #[serde(default = "default_algorithm")]
    pub algorithm: String,
    #[serde(default = "default_password_algorithm")]
    #[allow(dead_code)]
    pub password_algorithm: String,
    #[serde(default = "default_max_failed_attempts")]
    pub max_failed_attempts: u32,
    #[serde(default = "default_lockout_duration")]
    pub lockout_duration: String,
}

fn default_algorithm() -> String {
    "ES256".to_string()
}
fn default_password_algorithm() -> String {
    "argon2id".to_string()
}
fn default_max_failed_attempts() -> u32 {
    5
}
fn default_lockout_duration() -> String {
    "15m".to_string()
}

// ---------------------------------------------------------------------------
// Resolved runtime config (computed from TOML)
// ---------------------------------------------------------------------------

/// Runtime config passed to route handlers.
#[derive(Debug, Clone)]
pub struct RuntimeAuthConfig {
    pub access_ttl_secs: u64,
    pub refresh_ttl_secs: u64,
    pub cookie_name: String,
    pub cookie_domain: String,
    pub cookie_secure: bool,
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

pub fn load(path: &Path) -> anyhow::Result<Option<AuthServerConfig>> {
    if !path.exists() {
        return Ok(None);
    }
    let contents =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let expanded = expand_env_vars(&contents);
    let config: AuthServerConfig =
        toml::from_str(&expanded).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(config))
}

fn expand_env_vars(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek() == Some(&'{') {
            chars.next();
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
                result.push_str("${");
                result.push_str(&var_name);
            } else {
                match std::env::var(&var_name) {
                    Ok(val) => result.push_str(&val),
                    Err(_) => {
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

pub fn to_engine_config(config: &AuthServerConfig) -> StorageEngineConfig {
    StorageEngineConfig {
        data_dir: config.storage.data_dir.clone(),
        ..StorageEngineConfig::default()
    }
}

pub fn to_runtime_config(config: &AuthSettings) -> anyhow::Result<RuntimeAuthConfig> {
    Ok(RuntimeAuthConfig {
        access_ttl_secs: parse_duration(&config.access_ttl)?,
        refresh_ttl_secs: parse_duration(&config.refresh_ttl)?,
        cookie_name: config.cookie_name.clone(),
        cookie_domain: config.cookie_domain.clone(),
        cookie_secure: config.cookie_secure,
    })
}

/// Build the three internal keyspaces for a single auth keyspace.
pub fn build_auth_keyspaces(
    name: &str,
    ks_config: &AuthKeyspaceConfig,
    access_ttl_secs: u64,
    refresh_ttl_secs: u64,
) -> anyhow::Result<Vec<Keyspace>> {
    let algorithm = parse_jwt_algorithm(&ks_config.algorithm)?;
    let lockout_secs = parse_duration(&ks_config.lockout_duration)?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let passwords_ks = Keyspace {
        name: format!("{name}_passwords"),
        namespace: Namespace::default(),
        keyspace_type: KeyspaceType::Password,
        policy: KeyspacePolicy::Password {
            algorithm: PasswordAlgorithm::Argon2id,
            hash_params: HashParams::default(),
            max_failed_attempts: ks_config.max_failed_attempts,
            lockout_duration_secs: lockout_secs,
        },
        disabled: false,
        meta_schema: None,
        created_at: now as UnixTimestamp,
        signing_keys: Vec::new(),
        api_keys: Vec::new(),
        hmac_keys: Vec::new(),
        refresh_tokens: Vec::new(),
        password_entries: Vec::new(),
    };

    let access_ks = Keyspace {
        name: format!("{name}_access"),
        namespace: Namespace::default(),
        keyspace_type: KeyspaceType::Jwt,
        policy: KeyspacePolicy::Jwt {
            algorithm,
            rotation_days: 90,
            drain_days: 30,
            pre_stage_days: 7,
            default_ttl_secs: access_ttl_secs,
            required_claims: None,
            verify_cache_ttl_secs: None,
            leeway_secs: 30,
        },
        disabled: false,
        meta_schema: None,
        created_at: now as UnixTimestamp,
        signing_keys: Vec::new(),
        api_keys: Vec::new(),
        hmac_keys: Vec::new(),
        refresh_tokens: Vec::new(),
        password_entries: Vec::new(),
    };

    let refresh_ks = Keyspace {
        name: format!("{name}_refresh"),
        namespace: Namespace::default(),
        keyspace_type: KeyspaceType::RefreshToken,
        policy: KeyspacePolicy::RefreshToken {
            token_ttl_secs: refresh_ttl_secs,
            max_chain_length: 100,
            family_ttl_secs: refresh_ttl_secs * 3,
        },
        disabled: false,
        meta_schema: None,
        created_at: now as UnixTimestamp,
        signing_keys: Vec::new(),
        api_keys: Vec::new(),
        hmac_keys: Vec::new(),
        refresh_tokens: Vec::new(),
        password_entries: Vec::new(),
    };

    Ok(vec![passwords_ks, access_ks, refresh_ks])
}

// ---------------------------------------------------------------------------
// Duration parsing
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
