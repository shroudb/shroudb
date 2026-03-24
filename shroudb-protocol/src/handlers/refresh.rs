use std::time::{SystemTime, UNIX_EPOCH};

use shroudb_core::{
    CredentialId, Keyspace, KeyspacePolicy, KeyspaceType, RefreshTokenEntry, RefreshTokenState,
};
use shroudb_storage::index::refresh_token::AtomicRefreshError;
use shroudb_storage::{OpType, StorageEngine, WalPayload};

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle_refresh(
    engine: &StorageEngine,
    keyspace: &Keyspace,
    token: &str,
) -> Result<ResponseMap, CommandError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let ks_name = &keyspace.name;

    if keyspace.keyspace_type != KeyspaceType::RefreshToken {
        return Err(CommandError::WrongType {
            keyspace: ks_name.clone(),
            actual: format!("{:?}", keyspace.keyspace_type),
            expected: "refresh_token".into(),
        });
    }

    let (token_ttl_secs, max_chain_length) = match &keyspace.policy {
        KeyspacePolicy::RefreshToken {
            token_ttl_secs,
            max_chain_length,
            ..
        } => (*token_ttl_secs, *max_chain_length),
        _ => {
            return Err(CommandError::Internal(
                "keyspace type is RefreshToken but policy variant does not match".into(),
            ));
        }
    };

    let token_hash = shroudb_crypto::sha256(token.as_bytes());

    // Generate new token material up front so we can build the entry before locking.
    let (new_token, _raw_bytes) = shroudb_crypto::generate_api_key(None)?;
    let new_hash = shroudb_crypto::sha256(new_token.as_bytes());
    let new_credential_id = CredentialId::new();
    let new_expires_at = now + token_ttl_secs;

    // Step 1: Peek to get fields needed for constructing the new entry.
    // The DashMap Ref from get() is dropped at the end of this block.
    let (peek_family_id, peek_metadata, peek_credential_id, peek_chain_index) =
        {
            let idx = engine.index().refresh_tokens.get(ks_name).ok_or_else(|| {
                CommandError::NotFound {
                    entity: "index".into(),
                    id: ks_name.clone(),
                }
            })?;

            let peek = idx
                .lookup_by_hash(&token_hash)
                .ok_or_else(|| CommandError::NotFound {
                    entity: "refresh_token".into(),
                    id: "(hash lookup)".into(),
                })?;

            (
                peek.family_id.clone(),
                peek.metadata.clone(),
                peek.credential_id.clone(),
                peek.chain_index,
            )
        };

    // Step 2: Build the new entry and do the atomic consume-and-issue.
    // The DashMap Ref is dropped at the end of this block.
    let atomic_result =
        {
            let idx = engine.index().refresh_tokens.get(ks_name).ok_or_else(|| {
                CommandError::NotFound {
                    entity: "index".into(),
                    id: ks_name.clone(),
                }
            })?;

            let new_entry = RefreshTokenEntry {
                credential_id: new_credential_id.clone(),
                token_hash: new_hash,
                family_id: peek_family_id.clone(),
                state: RefreshTokenState::Active,
                metadata: peek_metadata.clone(),
                created_at: now,
                expires_at: new_expires_at,
                parent_id: Some(peek_credential_id),
                chain_index: peek_chain_index + 1,
            };

            idx.atomic_consume_and_issue(&token_hash, new_entry)
        };

    // Step 3: Handle the result. DashMap refs are all dropped, so engine.apply()
    // can safely re-acquire them via apply_payload_to_index.
    match atomic_result {
        Ok(refresh_result) => {
            let old_entry = refresh_result.old_entry;

            // Check expiry (the index accepted it, but we still enforce TTL)
            if now > old_entry.expires_at {
                return Err(CommandError::Expired {
                    entity: "refresh_token".into(),
                    id: old_entry.credential_id.as_str().to_string(),
                });
            }

            // Check chain length
            if refresh_result.chain_len as u32 >= max_chain_length {
                return Err(CommandError::ChainLimit {
                    family_id: old_entry.family_id.as_str().to_string(),
                    limit: max_chain_length,
                });
            }

            // Write WAL entries for durability only — the index was already
            // updated atomically by atomic_consume_and_issue above.
            engine
                .apply_wal_only(
                    ks_name,
                    OpType::RefreshTokenConsumed,
                    WalPayload::RefreshTokenConsumed {
                        credential_id: old_entry.credential_id.clone(),
                    },
                )
                .await?;

            let wal_new_entry = RefreshTokenEntry {
                credential_id: new_credential_id.clone(),
                token_hash: new_hash,
                family_id: old_entry.family_id.clone(),
                state: RefreshTokenState::Active,
                metadata: old_entry.metadata.clone(),
                created_at: now,
                expires_at: new_expires_at,
                parent_id: Some(old_entry.credential_id.clone()),
                chain_index: old_entry.chain_index + 1,
            };

            engine
                .apply_wal_only(
                    ks_name,
                    OpType::RefreshTokenIssued,
                    WalPayload::RefreshTokenIssued {
                        entry: wal_new_entry,
                        request_id: None,
                    },
                )
                .await?;

            let mut resp = ResponseMap::ok()
                .with("token", ResponseValue::String(new_token))
                .with(
                    "credential_id",
                    ResponseValue::String(new_credential_id.as_str().to_string()),
                )
                .with(
                    "family_id",
                    ResponseValue::String(old_entry.family_id.as_str().to_string()),
                )
                .with("expires_at", ResponseValue::Integer(new_expires_at as i64));

            if !old_entry.metadata.is_empty() {
                let meta_map: serde_json::Map<String, serde_json::Value> = old_entry
                    .metadata
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_json()))
                    .collect();
                resp = resp.with(
                    "metadata",
                    ResponseValue::Json(serde_json::Value::Object(meta_map)),
                );
            }

            Ok(resp)
        }
        Err(AtomicRefreshError::Consumed(family_id)) => {
            // Reuse detection: revoke entire family
            engine
                .apply(
                    ks_name,
                    OpType::FamilyRevoked,
                    WalPayload::FamilyRevoked {
                        family_id: family_id.clone(),
                    },
                )
                .await?;

            Err(CommandError::ReuseDetected {
                family_id: family_id.as_str().to_string(),
            })
        }
        Err(AtomicRefreshError::Revoked) => Err(CommandError::Denied {
            reason: "token revoked".into(),
        }),
        Err(AtomicRefreshError::NotFound) => Err(CommandError::NotFound {
            entity: "refresh_token".into(),
            id: "(hash lookup)".into(),
        }),
    }
}
