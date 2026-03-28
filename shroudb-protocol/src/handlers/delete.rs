use shroudb_store::Store;

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle(store: &impl Store, ns: &str, key: &[u8]) -> Result<ResponseMap, CommandError> {
    let version = store.delete(ns, key).await.map_err(|e| match e {
        shroudb_store::StoreError::NotFound => CommandError::NotFound,
        shroudb_store::StoreError::NamespaceNotFound(_) => {
            CommandError::NamespaceNotFound(ns.to_string())
        }
        other => CommandError::Internal(format!("{ns}/{}: {other}", String::from_utf8_lossy(key))),
    })?;

    Ok(ResponseMap::ok().with("version", ResponseValue::Integer(version as i64)))
}
