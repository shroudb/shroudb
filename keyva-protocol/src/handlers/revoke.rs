use std::time::Duration;

use keyva_core::{ApiKeyState, CredentialId, FamilyId, Keyspace, KeyspaceType};
use keyva_storage::{OpType, StorageEngine, WalPayload};

use crate::command::RevokeTarget;
use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle_revoke(
    engine: &StorageEngine,
    keyspace: &Keyspace,
    target: &RevokeTarget,
    ttl_secs: Option<u64>,
) -> Result<ResponseMap, CommandError> {
    let ks_name = &keyspace.name;
    let revocation_ttl = Duration::from_secs(ttl_secs.unwrap_or(86400)); // default 24h

    match target {
        RevokeTarget::Single(credential_id_str) => {
            let credential_id = CredentialId::from_string(credential_id_str.clone());
            revoke_single(engine, keyspace, &credential_id, revocation_ttl).await?;
            Ok(ResponseMap::ok().with("revoked", ResponseValue::Integer(1)))
        }
        RevokeTarget::Family(family_id_str) => {
            let family_id = FamilyId::from_string(family_id_str.clone());
            if keyspace.keyspace_type != KeyspaceType::RefreshToken {
                return Err(CommandError::WrongType {
                    keyspace: ks_name.clone(),
                    actual: format!("{:?}", keyspace.keyspace_type),
                    expected: "refresh_token".into(),
                });
            }

            // WAL write for family revocation
            engine
                .apply(
                    ks_name,
                    OpType::FamilyRevoked,
                    WalPayload::FamilyRevoked {
                        family_id: family_id.clone(),
                    },
                )
                .await?;

            // Insert into revocation set for each revoked credential
            if let Some(idx) = engine.index().refresh_tokens.get(ks_name) {
                let revoked_ids = idx.revoke_family(&family_id);
                if let Some(rev_set) = engine.index().revocations.get(ks_name) {
                    for id in &revoked_ids {
                        rev_set.insert(id.clone(), revocation_ttl);
                    }
                }
                return Ok(ResponseMap::ok()
                    .with("revoked", ResponseValue::Integer(revoked_ids.len() as i64)));
            }

            Ok(ResponseMap::ok().with("revoked", ResponseValue::Integer(0)))
        }
        RevokeTarget::Bulk(credential_ids) => {
            let mut count = 0i64;
            for id_str in credential_ids {
                let credential_id = CredentialId::from_string(id_str.clone());
                if revoke_single(engine, keyspace, &credential_id, revocation_ttl)
                    .await
                    .is_ok()
                {
                    count += 1;
                }
            }
            Ok(ResponseMap::ok().with("revoked", ResponseValue::Integer(count)))
        }
    }
}

async fn revoke_single(
    engine: &StorageEngine,
    keyspace: &Keyspace,
    credential_id: &CredentialId,
    revocation_ttl: Duration,
) -> Result<(), CommandError> {
    let ks_name = &keyspace.name;

    match keyspace.keyspace_type {
        KeyspaceType::ApiKey => {
            engine
                .apply(
                    ks_name,
                    OpType::ApiKeyRevoked,
                    WalPayload::ApiKeyStateChanged {
                        credential_id: credential_id.clone(),
                        new_state: ApiKeyState::Revoked,
                    },
                )
                .await?;
        }
        KeyspaceType::RefreshToken => {
            engine
                .apply(
                    ks_name,
                    OpType::RefreshTokenRevoked,
                    WalPayload::RefreshTokenRevoked {
                        credential_id: credential_id.clone(),
                    },
                )
                .await?;
        }
        _ => {
            return Err(CommandError::WrongType {
                keyspace: ks_name.clone(),
                actual: format!("{:?}", keyspace.keyspace_type),
                expected: "api_key or refresh_token".into(),
            });
        }
    }

    // Insert into revocation set with TTL
    if let Some(rev_set) = engine.index().revocations.get(ks_name) {
        rev_set.insert(credential_id.clone(), revocation_ttl);
    }

    Ok(())
}
