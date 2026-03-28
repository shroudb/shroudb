use std::sync::Arc;

use shroudb_storage::{ConfigStore, StorageEngine};

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle_get(
    config_store: &ConfigStore,
    key: &str,
) -> Result<ResponseMap, CommandError> {
    match config_store.get(key) {
        Some(entry) => Ok(ResponseMap::ok()
            .with("key", ResponseValue::String(key.to_string()))
            .with("value", ResponseValue::String(entry.value))
            .with("source", ResponseValue::String(entry.source.to_string()))),
        None => Ok(ResponseMap::ok()
            .with("key", ResponseValue::String(key.to_string()))
            .with("value", ResponseValue::Null)),
    }
}

pub async fn handle_set(
    engine: &Arc<StorageEngine>,
    key: &str,
    value: &str,
) -> Result<ResponseMap, CommandError> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| CommandError::Internal("system clock error".into()))?
        .as_secs();

    // Write to WAL for persistence across restarts
    engine
        .apply(
            "__system__",
            shroudb_storage::OpType::ConfigChanged,
            shroudb_storage::WalPayload::ConfigChanged {
                key: key.to_string(),
                value: value.to_string(),
            },
        )
        .await
        .map_err(|e| CommandError::Internal(e.to_string()))?;

    // Update in-memory config store (validates against schema)
    engine
        .config_store()
        .set(
            key.to_string(),
            value.to_string(),
            timestamp,
            shroudb_storage::ConfigSource::Runtime,
        )
        .map_err(|e| CommandError::BadArg { message: e })?;

    Ok(ResponseMap::ok())
}
