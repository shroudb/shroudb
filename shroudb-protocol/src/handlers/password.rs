use std::time::{SystemTime, UNIX_EPOCH};

use metrics::counter;
use shroudb_core::{
    CredentialId, Keyspace, KeyspacePolicy, PasswordAlgorithm, PasswordEntry, PasswordState,
    metadata_from_json,
};
use shroudb_storage::{OpType, StorageEngine, WalPayload};

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

fn require_password_policy(keyspace: &Keyspace) -> Result<&KeyspacePolicy, CommandError> {
    match &keyspace.policy {
        p @ KeyspacePolicy::Password { .. } => Ok(p),
        _ => Err(CommandError::WrongType {
            keyspace: keyspace.name.clone(),
            actual: format!("{:?}", keyspace.keyspace_type),
            expected: "Password".into(),
        }),
    }
}

pub async fn handle_password_set(
    engine: &StorageEngine,
    keyspace: &Keyspace,
    user_id: &str,
    plaintext: &str,
    metadata_json: Option<serde_json::Value>,
) -> Result<ResponseMap, CommandError> {
    let policy = require_password_policy(keyspace)?;
    let (algorithm, hash_params) = match policy {
        KeyspacePolicy::Password {
            algorithm,
            hash_params,
            ..
        } => (*algorithm, hash_params.clone()),
        _ => {
            return Err(CommandError::Internal(
                "require_password_policy passed but policy is not Password".into(),
            ));
        }
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let ks_name = &keyspace.name;

    // Check if user already has a password
    if let Some(idx) = engine.index().passwords.get(ks_name)
        && idx.lookup_by_user_id(user_id).is_some()
    {
        return Err(CommandError::StateError {
            from: "exists".into(),
            to: "PASSWORD SET requires no existing password; use PASSWORD CHANGE".into(),
        });
    }

    let hash = shroudb_crypto::password_hash(
        plaintext.as_bytes(),
        hash_params.m_cost,
        hash_params.t_cost,
        hash_params.p_cost,
    )?;

    let credential_id = CredentialId::new();
    let mut meta = metadata_json
        .map(|v| metadata_from_json(v).map_err(CommandError::ValidationError))
        .transpose()?
        .unwrap_or_default();

    // Validate metadata against schema if present and enforced
    if let Some(schema) = &keyspace.meta_schema
        && schema.enforce
    {
        schema.validate(&mut meta).map_err(|errs| {
            CommandError::ValidationError(
                errs.iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        })?;
    }

    let entry = PasswordEntry {
        credential_id: credential_id.clone(),
        user_id: user_id.to_string(),
        hash,
        algorithm,
        params: hash_params,
        state: PasswordState::Active,
        metadata: meta,
        created_at: now,
        updated_at: now,
    };

    engine
        .apply(
            ks_name,
            OpType::PasswordSet,
            WalPayload::PasswordSet {
                entry,
                request_id: None,
            },
        )
        .await?;

    Ok(ResponseMap::ok()
        .with(
            "credential_id",
            ResponseValue::String(credential_id.as_str().to_string()),
        )
        .with("user_id", ResponseValue::String(user_id.to_string()))
        .with("algorithm", ResponseValue::String(algorithm.to_string()))
        .with("created_at", ResponseValue::Integer(now as i64)))
}

pub async fn handle_password_verify(
    engine: &StorageEngine,
    keyspace: &Keyspace,
    user_id: &str,
    plaintext: &str,
) -> Result<ResponseMap, CommandError> {
    let policy = require_password_policy(keyspace)?;
    let (hash_params, max_failed, lockout_secs) = match policy {
        KeyspacePolicy::Password {
            hash_params,
            max_failed_attempts,
            lockout_duration_secs,
            ..
        } => (
            hash_params.clone(),
            *max_failed_attempts,
            *lockout_duration_secs,
        ),
        _ => {
            return Err(CommandError::Internal(
                "require_password_policy passed but policy is not Password".into(),
            ));
        }
    };
    let ks_name = &keyspace.name;

    // Check rate limiting
    if let Some(limiter) = engine.index().password_rate_limiters.get(ks_name)
        && let Some(retry_after) = limiter.check_lockout(user_id, max_failed, lockout_secs)
    {
        counter!("shroudb_password_lockout_total", "keyspace" => ks_name.clone()).increment(1);
        return Err(CommandError::Locked {
            retry_after_secs: retry_after,
        });
    }

    let idx = engine
        .index()
        .passwords
        .get(ks_name)
        .ok_or_else(|| CommandError::NotFound {
            entity: "index".into(),
            id: ks_name.clone(),
        })?;

    let entry = idx
        .lookup_by_user_id(user_id)
        .ok_or_else(|| CommandError::NotFound {
            entity: "password".into(),
            id: user_id.to_string(),
        })?;

    if entry.state != PasswordState::Active {
        return Err(CommandError::StateError {
            from: entry.state.to_string(),
            to: "verification requires Active".into(),
        });
    }

    let valid = shroudb_crypto::password_verify(plaintext.as_bytes(), &entry.hash)?;

    if !valid {
        counter!("shroudb_password_verify_failed_total", "keyspace" => ks_name.clone())
            .increment(1);
        // Record failed attempt
        if let Some(limiter) = engine.index().password_rate_limiters.get(ks_name) {
            limiter.record_failure(user_id);
        }
        return Err(CommandError::Denied {
            reason: "invalid password".into(),
        });
    }

    // Clear rate limiter on success
    if let Some(limiter) = engine.index().password_rate_limiters.get(ks_name) {
        limiter.clear(user_id);
    }

    // Check if rehash is needed (async on primary, Option B from spec)
    let needs_rehash = shroudb_crypto::password_needs_rehash(
        &entry.hash,
        hash_params.m_cost,
        hash_params.t_cost,
        hash_params.p_cost,
    );

    if needs_rehash {
        counter!("shroudb_password_rehash_total", "keyspace" => ks_name.clone()).increment(1);
        // Fire-and-forget rehash: hash with new params and write WAL
        if let Ok(new_hash) = shroudb_crypto::password_hash(
            plaintext.as_bytes(),
            hash_params.m_cost,
            hash_params.t_cost,
            hash_params.p_cost,
        ) {
            let cred_id = entry.credential_id.clone();
            let uid = user_id.to_string();
            // Update index immediately
            idx.update_hash(
                &uid,
                new_hash.clone(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            );
            // Write WAL for durability (don't update index again)
            let _ = engine
                .apply_wal_only(
                    ks_name,
                    OpType::PasswordRehashed,
                    WalPayload::PasswordRehashed {
                        credential_id: cred_id,
                        user_id: uid,
                        new_hash,
                    },
                )
                .await;
        }
    }

    Ok(ResponseMap::ok()
        .with("valid", ResponseValue::Boolean(true))
        .with(
            "credential_id",
            ResponseValue::String(entry.credential_id.as_str().to_string()),
        )
        .with(
            "metadata",
            ResponseValue::Json(shroudb_core::metadata_to_json(&entry.metadata)),
        ))
}

pub async fn handle_password_change(
    engine: &StorageEngine,
    keyspace: &Keyspace,
    user_id: &str,
    old_plaintext: &str,
    new_plaintext: &str,
) -> Result<ResponseMap, CommandError> {
    let policy = require_password_policy(keyspace)?;
    let hash_params = match policy {
        KeyspacePolicy::Password { hash_params, .. } => hash_params.clone(),
        _ => {
            return Err(CommandError::Internal(
                "require_password_policy passed but policy is not Password".into(),
            ));
        }
    };
    let ks_name = &keyspace.name;

    let idx = engine
        .index()
        .passwords
        .get(ks_name)
        .ok_or_else(|| CommandError::NotFound {
            entity: "index".into(),
            id: ks_name.clone(),
        })?;

    let entry = idx
        .lookup_by_user_id(user_id)
        .ok_or_else(|| CommandError::NotFound {
            entity: "password".into(),
            id: user_id.to_string(),
        })?;

    if entry.state != PasswordState::Active {
        return Err(CommandError::StateError {
            from: entry.state.to_string(),
            to: "change requires Active".into(),
        });
    }

    // Verify old password
    let valid = shroudb_crypto::password_verify(old_plaintext.as_bytes(), &entry.hash)?;
    if !valid {
        return Err(CommandError::Denied {
            reason: "old password is incorrect".into(),
        });
    }

    // Hash new password
    let new_hash = shroudb_crypto::password_hash(
        new_plaintext.as_bytes(),
        hash_params.m_cost,
        hash_params.t_cost,
        hash_params.p_cost,
    )?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    engine
        .apply(
            ks_name,
            OpType::PasswordChanged,
            WalPayload::PasswordChanged {
                credential_id: entry.credential_id.clone(),
                user_id: user_id.to_string(),
                new_hash,
            },
        )
        .await?;

    Ok(ResponseMap::ok()
        .with(
            "credential_id",
            ResponseValue::String(entry.credential_id.as_str().to_string()),
        )
        .with("updated_at", ResponseValue::Integer(now as i64)))
}

/// Force-reset a user's password without requiring the old password.
/// The caller is responsible for authorization (e.g., a verified reset token).
pub async fn handle_password_reset(
    engine: &StorageEngine,
    keyspace: &Keyspace,
    user_id: &str,
    new_plaintext: &str,
) -> Result<ResponseMap, CommandError> {
    let policy = require_password_policy(keyspace)?;
    let hash_params = match policy {
        KeyspacePolicy::Password { hash_params, .. } => hash_params.clone(),
        _ => {
            return Err(CommandError::Internal(
                "require_password_policy passed but policy is not Password".into(),
            ));
        }
    };
    let ks_name = &keyspace.name;

    let idx = engine
        .index()
        .passwords
        .get(ks_name)
        .ok_or_else(|| CommandError::NotFound {
            entity: "index".into(),
            id: ks_name.clone(),
        })?;

    let entry = idx
        .lookup_by_user_id(user_id)
        .ok_or_else(|| CommandError::NotFound {
            entity: "password".into(),
            id: user_id.to_string(),
        })?;

    // Hash new password
    let new_hash = shroudb_crypto::password_hash(
        new_plaintext.as_bytes(),
        hash_params.m_cost,
        hash_params.t_cost,
        hash_params.p_cost,
    )?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    engine
        .apply(
            ks_name,
            OpType::PasswordChanged,
            WalPayload::PasswordChanged {
                credential_id: entry.credential_id.clone(),
                user_id: user_id.to_string(),
                new_hash,
            },
        )
        .await?;

    // Clear any brute-force lockout for this user after a successful reset
    if let Some(rl) = engine.index().password_rate_limiters.get(ks_name) {
        rl.clear(user_id);
    }

    Ok(ResponseMap::ok()
        .with(
            "credential_id",
            ResponseValue::String(entry.credential_id.as_str().to_string()),
        )
        .with("updated_at", ResponseValue::Integer(now as i64)))
}

pub async fn handle_password_import(
    engine: &StorageEngine,
    keyspace: &Keyspace,
    user_id: &str,
    hash: &str,
    metadata_json: Option<serde_json::Value>,
) -> Result<ResponseMap, CommandError> {
    let policy = require_password_policy(keyspace)?;
    let hash_params = match policy {
        KeyspacePolicy::Password { hash_params, .. } => hash_params.clone(),
        _ => {
            return Err(CommandError::Internal(
                "require_password_policy passed but policy is not Password".into(),
            ));
        }
    };

    let hash_bytes = hash.as_bytes();

    // Validate the hash format
    shroudb_crypto::validate_imported_hash(hash_bytes)?;

    // Detect algorithm from the hash
    let algorithm = match shroudb_crypto::detect_hash_algorithm(hash_bytes) {
        Some("argon2id") => PasswordAlgorithm::Argon2id,
        Some("argon2i") => PasswordAlgorithm::Argon2i,
        Some("argon2d") => PasswordAlgorithm::Argon2d,
        Some("bcrypt") => PasswordAlgorithm::Bcrypt,
        Some("scrypt") => PasswordAlgorithm::Scrypt,
        _ => {
            return Err(CommandError::ValidationError(
                "unrecognized hash algorithm".into(),
            ));
        }
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let ks_name = &keyspace.name;

    // Check if user already has a password
    if let Some(idx) = engine.index().passwords.get(ks_name)
        && idx.lookup_by_user_id(user_id).is_some()
    {
        return Err(CommandError::StateError {
            from: "exists".into(),
            to: "PASSWORD IMPORT requires no existing password; use PASSWORD CHANGE".into(),
        });
    }

    let credential_id = CredentialId::new();
    let mut meta = metadata_json
        .map(|v| metadata_from_json(v).map_err(CommandError::ValidationError))
        .transpose()?
        .unwrap_or_default();

    // Validate metadata against schema if present and enforced
    if let Some(schema) = &keyspace.meta_schema
        && schema.enforce
    {
        schema.validate(&mut meta).map_err(|errs| {
            CommandError::ValidationError(
                errs.iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        })?;
    }

    let entry = PasswordEntry {
        credential_id: credential_id.clone(),
        user_id: user_id.to_string(),
        hash: hash_bytes.to_vec(),
        algorithm,
        params: hash_params,
        state: PasswordState::Active,
        metadata: meta,
        created_at: now,
        updated_at: now,
    };

    engine
        .apply(
            ks_name,
            OpType::PasswordSet,
            WalPayload::PasswordSet {
                entry,
                request_id: None,
            },
        )
        .await?;

    Ok(ResponseMap::ok()
        .with(
            "credential_id",
            ResponseValue::String(credential_id.as_str().to_string()),
        )
        .with("user_id", ResponseValue::String(user_id.to_string()))
        .with("algorithm", ResponseValue::String(algorithm.to_string()))
        .with("created_at", ResponseValue::Integer(now as i64)))
}
