use std::time::Duration;

use shroudb_store::{PutOptions, Store, metadata_from_json};

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle(
    store: &impl Store,
    ns: &str,
    key: &[u8],
    value: &[u8],
    metadata_json: Option<serde_json::Value>,
    ttl_ms: Option<u64>,
) -> Result<ResponseMap, CommandError> {
    let metadata = match metadata_json {
        Some(json) => Some(metadata_from_json(json).map_err(|e| CommandError::BadArg {
            message: format!("invalid metadata: {e}"),
        })?),
        None => None,
    };

    let options = PutOptions {
        metadata,
        ttl: ttl_ms.map(Duration::from_millis),
        expected_version: None,
    };
    let version = store.put_with_options(ns, key, value, options).await?;

    Ok(ResponseMap::ok().with("version", ResponseValue::Integer(version as i64)))
}
