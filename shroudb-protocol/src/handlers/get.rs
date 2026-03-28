use shroudb_store::{Store, metadata_to_json};

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle(
    store: &impl Store,
    ns: &str,
    key: &[u8],
    version: Option<u64>,
    include_meta: bool,
) -> Result<ResponseMap, CommandError> {
    let entry = store.get(ns, key, version).await.map_err(|e| match e {
        shroudb_store::StoreError::NotFound => CommandError::NotFound,
        shroudb_store::StoreError::NamespaceNotFound(_) => {
            CommandError::NamespaceNotFound(ns.to_string())
        }
        other => CommandError::Internal(format!("{ns}/{}: {other}", String::from_utf8_lossy(key))),
    })?;

    let mut resp = ResponseMap::ok()
        .with("key", ResponseValue::Bytes(entry.key))
        .with("value", ResponseValue::Bytes(entry.value))
        .with("version", ResponseValue::Integer(entry.version as i64));

    if include_meta {
        resp = resp.with(
            "metadata",
            ResponseValue::Json(metadata_to_json(&entry.metadata)),
        );
    }

    Ok(resp)
}
