use shroudb_store::{Store, metadata_from_json};

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle(
    store: &impl Store,
    ns: &str,
    key: &[u8],
    value: &[u8],
    metadata_json: Option<serde_json::Value>,
) -> Result<ResponseMap, CommandError> {
    let metadata = match metadata_json {
        Some(json) => Some(metadata_from_json(json).map_err(|e| CommandError::BadArg {
            message: format!("invalid metadata: {e}"),
        })?),
        None => None,
    };

    let version = store.put(ns, key, value, metadata).await?;

    Ok(ResponseMap::ok().with("version", ResponseValue::Integer(version as i64)))
}
