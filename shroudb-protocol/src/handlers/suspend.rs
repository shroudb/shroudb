use shroudb_core::{ApiKeyState, CredentialId, Keyspace, KeyspaceType};
use shroudb_storage::{OpType, StorageEngine, WalPayload};

use crate::error::CommandError;
use crate::response::ResponseMap;

pub async fn handle_suspend(
    engine: &StorageEngine,
    keyspace: &Keyspace,
    credential_id_str: &str,
) -> Result<ResponseMap, CommandError> {
    let ks_name = &keyspace.name;

    if keyspace.keyspace_type != KeyspaceType::ApiKey {
        return Err(CommandError::WrongType {
            keyspace: ks_name.clone(),
            actual: format!("{:?}", keyspace.keyspace_type),
            expected: "api_key".into(),
        });
    }

    let credential_id = CredentialId::from_string(credential_id_str.to_string());

    // Check current state
    let idx = engine
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

    if entry.state != ApiKeyState::Active {
        return Err(CommandError::StateError {
            from: entry.state.to_string(),
            to: "Suspended".into(),
        });
    }

    engine
        .apply(
            ks_name,
            OpType::ApiKeySuspended,
            WalPayload::ApiKeyStateChanged {
                credential_id,
                new_state: ApiKeyState::Suspended,
            },
        )
        .await?;

    Ok(ResponseMap::ok())
}

pub async fn handle_unsuspend(
    engine: &StorageEngine,
    keyspace: &Keyspace,
    credential_id_str: &str,
) -> Result<ResponseMap, CommandError> {
    let ks_name = &keyspace.name;

    if keyspace.keyspace_type != KeyspaceType::ApiKey {
        return Err(CommandError::WrongType {
            keyspace: ks_name.clone(),
            actual: format!("{:?}", keyspace.keyspace_type),
            expected: "api_key".into(),
        });
    }

    let credential_id = CredentialId::from_string(credential_id_str.to_string());

    let idx = engine
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

    if entry.state != ApiKeyState::Suspended {
        return Err(CommandError::StateError {
            from: entry.state.to_string(),
            to: "Active".into(),
        });
    }

    engine
        .apply(
            ks_name,
            OpType::ApiKeyUnsuspended,
            WalPayload::ApiKeyStateChanged {
                credential_id,
                new_state: ApiKeyState::Active,
            },
        )
        .await?;

    Ok(ResponseMap::ok())
}
