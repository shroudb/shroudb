use std::sync::Arc;

use shroudb_core::{
    HashParams, Keyspace, KeyspacePolicy, KeyspaceType, Namespace, PasswordAlgorithm,
    UnixTimestamp,
};
use shroudb_crypto::{HmacAlgorithm, JwtAlgorithm};
use shroudb_storage::{
    OpType, StorageEngine,
    wal::{KeyspaceCreatedPayload, WalPayload},
};

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

/// Handle KEYSPACE_CREATE: create a new keyspace at runtime.
///
/// The keyspace definition is persisted to WAL (OpType::KeyspaceCreated) and
/// restored on startup via recovery. This follows the same pattern as Mint's
/// CA_CREATE command.
pub async fn handle_keyspace_create(
    engine: &Arc<StorageEngine>,
    name: &str,
    keyspace_type_str: &str,
    algorithm: Option<&str>,
    rotation_days: Option<u32>,
    drain_days: Option<u32>,
    default_ttl_secs: Option<u64>,
) -> Result<ResponseMap, CommandError> {
    // Reject if keyspace already exists.
    if engine.index().keyspaces.contains_key(name) {
        return Err(CommandError::AlreadyExists {
            entity: "keyspace".into(),
            name: name.to_string(),
        });
    }

    // Parse keyspace type.
    let keyspace_type = match keyspace_type_str.to_ascii_lowercase().as_str() {
        "jwt" => KeyspaceType::Jwt,
        "api_key" => KeyspaceType::ApiKey,
        "hmac" => KeyspaceType::Hmac,
        "refresh_token" => KeyspaceType::RefreshToken,
        "password" => KeyspaceType::Password,
        other => {
            return Err(CommandError::BadArg {
                message: format!(
                    "unknown keyspace type: {other}. Valid: jwt, api_key, hmac, refresh_token, password"
                ),
            });
        }
    };

    // Build type-specific policy.
    let policy = build_policy(
        keyspace_type,
        algorithm,
        rotation_days,
        drain_days,
        default_ttl_secs,
    )?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let namespace = Namespace::default();
    let created_at = UnixTimestamp::from(now);

    // Write WAL entry first (crash safety).
    let payload = WalPayload::KeyspaceCreated(KeyspaceCreatedPayload {
        name: name.to_string(),
        namespace: namespace.clone(),
        keyspace_type,
        policy: policy.clone(),
        created_at,
    });
    engine
        .apply_wal_only(name, OpType::KeyspaceCreated, payload)
        .await?;

    // Create type-specific indexes.
    engine.index().ensure_keyspace(name, keyspace_type);

    // Insert the keyspace into the in-memory index.
    let keyspace = Keyspace {
        name: name.to_string(),
        namespace,
        keyspace_type,
        policy: policy.clone(),
        disabled: false,
        meta_schema: None,
        created_at,
        signing_keys: Vec::new(),
        api_keys: Vec::new(),
        hmac_keys: Vec::new(),
        refresh_tokens: Vec::new(),
        password_entries: Vec::new(),
    };
    engine.index().keyspaces.insert(name.to_string(), keyspace);

    tracing::info!(
        target: "shroudb::audit",
        op = "KEYSPACE_CREATE",
        resource = name,
        result = "ok",
        "keyspace created"
    );

    let mut resp = ResponseMap::ok()
        .with("name", ResponseValue::String(name.to_string()))
        .with(
            "type",
            ResponseValue::String(keyspace_type_str.to_lowercase()),
        );

    if let Some(alg) = algorithm {
        resp = resp.with("algorithm", ResponseValue::String(alg.to_string()));
    }

    Ok(resp)
}

fn build_policy(
    keyspace_type: KeyspaceType,
    algorithm: Option<&str>,
    rotation_days: Option<u32>,
    drain_days: Option<u32>,
    default_ttl_secs: Option<u64>,
) -> Result<KeyspacePolicy, CommandError> {
    match keyspace_type {
        KeyspaceType::Jwt => {
            let alg = parse_jwt_algorithm(algorithm.unwrap_or("ES256"))?;
            Ok(KeyspacePolicy::Jwt {
                algorithm: alg,
                rotation_days: rotation_days.unwrap_or(90),
                drain_days: drain_days.unwrap_or(30),
                pre_stage_days: 7,
                default_ttl_secs: default_ttl_secs.unwrap_or(900),
                required_claims: None,
                verify_cache_ttl_secs: None,
                leeway_secs: 30,
            })
        }
        KeyspaceType::ApiKey => Ok(KeyspacePolicy::ApiKey { prefix: None }),
        KeyspaceType::Hmac => {
            let alg = parse_hmac_algorithm(algorithm.unwrap_or("sha256"))?;
            Ok(KeyspacePolicy::Hmac {
                algorithm: alg,
                rotation_days: rotation_days.unwrap_or(180),
                drain_days: drain_days.unwrap_or(14),
            })
        }
        KeyspaceType::RefreshToken => Ok(KeyspacePolicy::RefreshToken {
            token_ttl_secs: default_ttl_secs.unwrap_or(30 * 86_400),
            max_chain_length: 100,
            family_ttl_secs: 90 * 86_400,
        }),
        KeyspaceType::Password => {
            let alg = parse_password_algorithm(algorithm.unwrap_or("argon2id"))?;
            Ok(KeyspacePolicy::Password {
                algorithm: alg,
                hash_params: HashParams::default(),
                max_failed_attempts: 5,
                lockout_duration_secs: 15 * 60,
            })
        }
    }
}

fn parse_jwt_algorithm(s: &str) -> Result<JwtAlgorithm, CommandError> {
    match s {
        "ES256" => Ok(JwtAlgorithm::ES256),
        "ES384" => Ok(JwtAlgorithm::ES384),
        "RS256" => Ok(JwtAlgorithm::RS256),
        "RS384" => Ok(JwtAlgorithm::RS384),
        "RS512" => Ok(JwtAlgorithm::RS512),
        "EdDSA" => Ok(JwtAlgorithm::EdDSA),
        other => Err(CommandError::BadArg {
            message: format!("unknown JWT algorithm: {other}. Valid: ES256, ES384, RS256, RS384, RS512, EdDSA"),
        }),
    }
}

fn parse_hmac_algorithm(s: &str) -> Result<HmacAlgorithm, CommandError> {
    match s.to_lowercase().as_str() {
        "sha256" => Ok(HmacAlgorithm::Sha256),
        "sha384" => Ok(HmacAlgorithm::Sha384),
        "sha512" => Ok(HmacAlgorithm::Sha512),
        other => Err(CommandError::BadArg {
            message: format!("unknown HMAC algorithm: {other}. Valid: sha256, sha384, sha512"),
        }),
    }
}

fn parse_password_algorithm(s: &str) -> Result<PasswordAlgorithm, CommandError> {
    match s.to_lowercase().as_str() {
        "argon2id" => Ok(PasswordAlgorithm::Argon2id),
        "bcrypt" => Ok(PasswordAlgorithm::Bcrypt),
        "scrypt" => Ok(PasswordAlgorithm::Scrypt),
        other => Err(CommandError::BadArg {
            message: format!("unknown password algorithm: {other}. Valid: argon2id, bcrypt, scrypt"),
        }),
    }
}
