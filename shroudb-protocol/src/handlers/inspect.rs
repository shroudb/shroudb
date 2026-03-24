use shroudb_core::{CredentialId, Keyspace, KeyspaceType};
use shroudb_storage::StorageEngine;

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle_inspect(
    engine: &StorageEngine,
    keyspace: &Keyspace,
    credential_id_str: &str,
) -> Result<ResponseMap, CommandError> {
    let ks_name = &keyspace.name;
    let credential_id = CredentialId::from_string(credential_id_str.to_string());

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

            Ok(ResponseMap::ok()
                .with(
                    "credential_id",
                    ResponseValue::String(entry.credential_id.as_str().to_string()),
                )
                .with("state", ResponseValue::String(entry.state.to_string()))
                .with(
                    "metadata",
                    ResponseValue::Json(shroudb_core::metadata_to_json(&entry.metadata)),
                )
                .with(
                    "created_at",
                    ResponseValue::Integer(entry.created_at as i64),
                )
                .with(
                    "expires_at",
                    match entry.expires_at {
                        Some(ts) => ResponseValue::Integer(ts as i64),
                        None => ResponseValue::Null,
                    },
                ))
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

            Ok(ResponseMap::ok()
                .with(
                    "credential_id",
                    ResponseValue::String(entry.credential_id.as_str().to_string()),
                )
                .with(
                    "family_id",
                    ResponseValue::String(entry.family_id.as_str().to_string()),
                )
                .with("state", ResponseValue::String(entry.state.to_string()))
                .with(
                    "metadata",
                    ResponseValue::Json(shroudb_core::metadata_to_json(&entry.metadata)),
                )
                .with(
                    "created_at",
                    ResponseValue::Integer(entry.created_at as i64),
                )
                .with(
                    "expires_at",
                    ResponseValue::Integer(entry.expires_at as i64),
                )
                .with(
                    "chain_index",
                    ResponseValue::Integer(entry.chain_index as i64),
                ))
        }
        _ => Err(CommandError::WrongType {
            keyspace: ks_name.clone(),
            actual: format!("{:?}", keyspace.keyspace_type),
            expected: "api_key or refresh_token".into(),
        }),
    }
}
