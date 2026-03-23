use keyva_core::{CredentialId, Keyspace, KeyspaceType, Metadata, metadata_from_json};
use keyva_storage::{OpType, StorageEngine, WalPayload};

use crate::error::CommandError;
use crate::response::ResponseMap;

pub async fn handle_update(
    engine: &StorageEngine,
    keyspace: &Keyspace,
    credential_id_str: &str,
    metadata_patch: serde_json::Value,
) -> Result<ResponseMap, CommandError> {
    let ks_name = &keyspace.name;
    let credential_id = CredentialId::from_string(credential_id_str.to_string());

    let patch = metadata_from_json(metadata_patch).map_err(CommandError::ValidationError)?;

    match keyspace.keyspace_type {
        KeyspaceType::ApiKey => {
            let idx =
                engine
                    .index()
                    .api_keys
                    .get(ks_name)
                    .ok_or_else(|| CommandError::NotFound {
                        entity: "index".into(),
                        id: ks_name.clone(),
                    })?;
            let entry = idx
                .lookup_by_id(&credential_id)
                .ok_or_else(|| CommandError::NotFound {
                    entity: "api_key".into(),
                    id: credential_id_str.to_string(),
                })?;

            // Check not revoked
            if entry.state == keyva_core::ApiKeyState::Revoked {
                return Err(CommandError::Denied {
                    reason: "credential is revoked".into(),
                });
            }

            // Validate update if schema is present
            let merged = if let Some(schema) = &keyspace.meta_schema {
                if schema.enforce {
                    schema
                        .validate_update(&entry.metadata, &patch)
                        .map_err(|errs| {
                            CommandError::ValidationError(
                                errs.iter()
                                    .map(|e| e.to_string())
                                    .collect::<Vec<_>>()
                                    .join("; "),
                            )
                        })?
                } else {
                    merge_metadata(&entry.metadata, &patch)
                }
            } else {
                merge_metadata(&entry.metadata, &patch)
            };

            engine
                .apply(
                    ks_name,
                    OpType::ApiKeyUpdated,
                    WalPayload::ApiKeyUpdated {
                        credential_id: credential_id.clone(),
                        metadata: merged,
                    },
                )
                .await?;

            Ok(ResponseMap::ok())
        }
        KeyspaceType::RefreshToken => {
            let idx = engine.index().refresh_tokens.get(ks_name).ok_or_else(|| {
                CommandError::NotFound {
                    entity: "index".into(),
                    id: ks_name.clone(),
                }
            })?;
            let entry = idx
                .lookup_by_id(&credential_id)
                .ok_or_else(|| CommandError::NotFound {
                    entity: "refresh_token".into(),
                    id: credential_id_str.to_string(),
                })?;

            if entry.state == keyva_core::RefreshTokenState::Revoked {
                return Err(CommandError::Denied {
                    reason: "credential is revoked".into(),
                });
            }

            let merged = if let Some(schema) = &keyspace.meta_schema {
                if schema.enforce {
                    schema
                        .validate_update(&entry.metadata, &patch)
                        .map_err(|errs| {
                            CommandError::ValidationError(
                                errs.iter()
                                    .map(|e| e.to_string())
                                    .collect::<Vec<_>>()
                                    .join("; "),
                            )
                        })?
                } else {
                    merge_metadata(&entry.metadata, &patch)
                }
            } else {
                merge_metadata(&entry.metadata, &patch)
            };

            engine
                .apply(
                    ks_name,
                    OpType::RefreshTokenUpdated,
                    WalPayload::RefreshTokenUpdated {
                        credential_id: credential_id.clone(),
                        metadata: merged,
                    },
                )
                .await?;

            Ok(ResponseMap::ok())
        }
        _ => Err(CommandError::WrongType {
            keyspace: ks_name.clone(),
            actual: format!("{:?}", keyspace.keyspace_type),
            expected: "api_key or refresh_token".into(),
        }),
    }
}

/// Simple metadata merge: patch fields overwrite existing. Null removes.
fn merge_metadata(existing: &Metadata, patch: &Metadata) -> Metadata {
    let mut merged = existing.clone();
    for (key, value) in patch {
        if value.is_null() {
            merged.remove(key);
        } else {
            merged.insert(key.clone(), value.clone());
        }
    }
    merged
}
