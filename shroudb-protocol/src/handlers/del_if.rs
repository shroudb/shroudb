use shroudb_store::Store;

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle(
    store: &impl Store,
    ns: &str,
    key: &[u8],
    expected_version: u64,
) -> Result<ResponseMap, CommandError> {
    match store.delete_if_version(ns, key, expected_version).await {
        Ok(version) => {
            Ok(ResponseMap::ok().with("version", ResponseValue::Integer(version as i64)))
        }
        Err(shroudb_store::StoreError::VersionConflict { current }) => {
            Err(CommandError::VersionConflict { current })
        }
        Err(shroudb_store::StoreError::NotFound) => Err(CommandError::NotFound),
        Err(shroudb_store::StoreError::NamespaceNotFound(_)) => {
            Err(CommandError::NamespaceNotFound(ns.to_string()))
        }
        Err(other) => Err(CommandError::Internal(format!(
            "{ns}/{}: {other}",
            String::from_utf8_lossy(key)
        ))),
    }
}
