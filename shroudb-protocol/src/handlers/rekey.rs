use std::sync::Arc;

use shroudb_storage::StorageEngine;

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

/// Decode a hex string to bytes.
fn decode_hex(hex: &str) -> Result<Vec<u8>, String> {
    if !hex.len().is_multiple_of(2) {
        return Err("odd number of hex characters".into());
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

pub async fn handle_rekey(
    engine: &Arc<StorageEngine>,
    new_key_hex: &str,
) -> Result<ResponseMap, CommandError> {
    let key_bytes = decode_hex(new_key_hex).map_err(|e| CommandError::BadArg {
        message: format!("invalid hex key: {e}"),
    })?;

    if key_bytes.len() != 32 {
        return Err(CommandError::BadArg {
            message: format!(
                "key must be 32 bytes (64 hex chars), got {}",
                key_bytes.len()
            ),
        });
    }

    Arc::clone(engine)
        .begin_rekey_from_bytes(key_bytes)
        .await
        .map_err(|e: shroudb_storage::StorageError| CommandError::Internal(e.to_string()))?;

    Ok(ResponseMap::ok().with("message", ResponseValue::String("rekey started".into())))
}

pub async fn handle_rekey_status(engine: &Arc<StorageEngine>) -> Result<ResponseMap, CommandError> {
    match engine.rekey_status() {
        Some(progress) => Ok(ResponseMap::ok()
            .with("in_progress", ResponseValue::Boolean(progress.in_progress))
            .with(
                "progress",
                ResponseValue::String(format!("{:.1}%", progress.progress * 100.0)),
            )
            .with(
                "segments_completed",
                ResponseValue::Integer(progress.segments_completed as i64),
            )
            .with(
                "total_segments",
                ResponseValue::Integer(progress.total_segments as i64),
            )
            .with(
                "started_at",
                ResponseValue::Integer(progress.started_at as i64),
            )),
        None => Ok(ResponseMap::ok()
            .with("in_progress", ResponseValue::Boolean(false))
            .with(
                "message",
                ResponseValue::String("no rekey in progress".into()),
            )),
    }
}
