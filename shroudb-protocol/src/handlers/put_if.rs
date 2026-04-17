use shroudb_store::{Store, metadata_from_json};

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle(
    store: &impl Store,
    ns: &str,
    key: &[u8],
    value: &[u8],
    metadata_json: Option<serde_json::Value>,
    expected_version: u64,
) -> Result<ResponseMap, CommandError> {
    let metadata = match metadata_json {
        Some(json) => Some(metadata_from_json(json).map_err(|e| CommandError::BadArg {
            message: format!("invalid metadata: {e}"),
        })?),
        None => None,
    };

    match store
        .put_if_version(ns, key, value, metadata, expected_version)
        .await
    {
        Ok(version) => {
            Ok(ResponseMap::ok().with("version", ResponseValue::Integer(version as i64)))
        }
        Err(shroudb_store::StoreError::VersionConflict { current }) => {
            Err(CommandError::VersionConflict { current })
        }
        Err(shroudb_store::StoreError::NamespaceNotFound(_)) => {
            Err(CommandError::NamespaceNotFound(ns.to_string()))
        }
        Err(shroudb_store::StoreError::ValidationFailed(errors)) => {
            Err(CommandError::ValidationFailed(format!("{errors:?}")))
        }
        Err(other) => Err(CommandError::Internal(format!(
            "{ns}/{}: {other}",
            String::from_utf8_lossy(key)
        ))),
    }
}
