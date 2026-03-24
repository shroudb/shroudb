use std::time::{SystemTime, UNIX_EPOCH};

use shroudb_core::{
    ApiKeyEntry, CredentialId, FamilyId, Keyspace, KeyspacePolicy, RefreshTokenEntry,
    RefreshTokenState, metadata_from_json,
};
use shroudb_storage::{OpType, StorageEngine, WalPayload};

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle_issue(
    engine: &StorageEngine,
    keyspace: &Keyspace,
    claims: Option<serde_json::Value>,
    metadata: Option<serde_json::Value>,
    ttl_secs: Option<u64>,
    idempotency_key: Option<&str>,
) -> Result<ResponseMap, CommandError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let ks_name = &keyspace.name;

    match &keyspace.policy {
        KeyspacePolicy::Jwt {
            algorithm,
            default_ttl_secs,
            ..
        } => {
            let ring =
                engine
                    .index()
                    .jwt_rings
                    .get(ks_name)
                    .ok_or_else(|| CommandError::NotFound {
                        entity: "keyring".into(),
                        id: ks_name.clone(),
                    })?;
            let active_key = ring.active_key().ok_or_else(|| CommandError::NotFound {
                entity: "active_key".into(),
                id: ks_name.clone(),
            })?;
            let private_key = active_key.private_key.as_ref().ok_or_else(|| {
                CommandError::Internal(
                    "private key not available (not decrypted after recovery)".into(),
                )
            })?;

            let mut claims = claims.unwrap_or(serde_json::json!({}));
            // Set exp if not present
            if claims.get("exp").is_none() {
                let ttl = ttl_secs.unwrap_or(*default_ttl_secs);
                let exp = now + ttl;
                claims["exp"] = serde_json::json!(exp);
            }
            // Set iat if not present
            if claims.get("iat").is_none() {
                claims["iat"] = serde_json::json!(now);
            }

            let kid = active_key.key_id.as_str();
            let token = shroudb_crypto::sign_jwt(private_key.as_bytes(), *algorithm, &claims, kid)?;

            let expires_at = claims
                .get("exp")
                .and_then(|v| v.as_u64())
                .unwrap_or(now + ttl_secs.unwrap_or(*default_ttl_secs));

            Ok(ResponseMap::ok()
                .with("token", ResponseValue::String(token))
                .with("kid", ResponseValue::String(kid.to_string()))
                .with("expires_at", ResponseValue::Integer(expires_at as i64)))
        }

        KeyspacePolicy::ApiKey { prefix } => {
            let (api_key, _raw_bytes) = shroudb_crypto::generate_api_key(prefix.as_deref())?;
            let key_hash = shroudb_crypto::sha256(api_key.as_bytes());
            let credential_id = CredentialId::new();

            let expires_at = ttl_secs.map(|ttl| now + ttl);

            let meta_json = metadata.unwrap_or(serde_json::json!({}));
            let mut meta = metadata_from_json(meta_json).map_err(CommandError::ValidationError)?;

            // Validate metadata if schema is present and enforced
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

            let entry = ApiKeyEntry {
                credential_id: credential_id.clone(),
                key_hash,
                prefix: prefix.clone(),
                state: shroudb_core::ApiKeyState::Active,
                metadata: meta,
                created_at: now,
                expires_at,
                last_verified_at: None,
            };

            engine
                .apply(
                    ks_name,
                    OpType::ApiKeyIssued,
                    WalPayload::ApiKeyIssued {
                        entry,
                        request_id: idempotency_key.map(String::from),
                    },
                )
                .await?;

            Ok(ResponseMap::ok()
                .with("api_key", ResponseValue::String(api_key))
                .with(
                    "credential_id",
                    ResponseValue::String(credential_id.as_str().to_string()),
                ))
        }

        KeyspacePolicy::Hmac { algorithm, .. } => {
            let ring =
                engine
                    .index()
                    .hmac_rings
                    .get(ks_name)
                    .ok_or_else(|| CommandError::NotFound {
                        entity: "keyring".into(),
                        id: ks_name.clone(),
                    })?;
            let active_key = ring.active_key().ok_or_else(|| CommandError::NotFound {
                entity: "active_key".into(),
                id: ks_name.clone(),
            })?;
            let key_material = active_key.key_material.as_ref().ok_or_else(|| {
                CommandError::Internal(
                    "HMAC key material not available (not decrypted after recovery)".into(),
                )
            })?;

            let payload_bytes = claims
                .as_ref()
                .map(|c| serde_json::to_vec(c).unwrap_or_default())
                .unwrap_or_default();

            let signature =
                shroudb_crypto::hmac_sign(*algorithm, key_material.as_bytes(), &payload_bytes)?;
            let sig_hex = hex::encode(&signature);
            let kid = active_key.key_id.as_str().to_string();

            Ok(ResponseMap::ok()
                .with("signature", ResponseValue::String(sig_hex))
                .with("kid", ResponseValue::String(kid)))
        }

        KeyspacePolicy::RefreshToken { token_ttl_secs, .. } => {
            let (token, _raw_bytes) = shroudb_crypto::generate_api_key(None)?;
            let token_hash = shroudb_crypto::sha256(token.as_bytes());
            let credential_id = CredentialId::new();
            let family_id = FamilyId::new();
            let expires_at = now + ttl_secs.unwrap_or(*token_ttl_secs);
            let meta_json = metadata.unwrap_or(serde_json::json!({}));
            let meta = metadata_from_json(meta_json).map_err(CommandError::ValidationError)?;

            let entry = RefreshTokenEntry {
                credential_id: credential_id.clone(),
                token_hash,
                family_id: family_id.clone(),
                state: RefreshTokenState::Active,
                metadata: meta,
                created_at: now,
                expires_at,
                parent_id: None,
                chain_index: 0,
            };

            engine
                .apply(
                    ks_name,
                    OpType::RefreshTokenIssued,
                    WalPayload::RefreshTokenIssued {
                        entry,
                        request_id: idempotency_key.map(String::from),
                    },
                )
                .await?;

            Ok(ResponseMap::ok()
                .with("token", ResponseValue::String(token))
                .with(
                    "credential_id",
                    ResponseValue::String(credential_id.as_str().to_string()),
                )
                .with(
                    "family_id",
                    ResponseValue::String(family_id.as_str().to_string()),
                ))
        }

        KeyspacePolicy::Password { .. } => Err(CommandError::WrongType {
            keyspace: keyspace.name.clone(),
            actual: "Password".into(),
            expected: "use PASSWORD SET for password keyspaces".into(),
        }),
    }
}
