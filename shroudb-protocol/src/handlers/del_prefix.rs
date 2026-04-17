use shroudb_store::Store;

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle(
    store: &impl Store,
    ns: &str,
    prefix: &[u8],
) -> Result<ResponseMap, CommandError> {
    match store.delete_prefix(ns, prefix).await {
        Ok(deleted) => {
            Ok(ResponseMap::ok().with("deleted", ResponseValue::Integer(deleted as i64)))
        }
        Err(shroudb_store::StoreError::PrefixTooLarge { matched, limit }) => {
            Err(CommandError::PrefixTooLarge { matched, limit })
        }
        Err(shroudb_store::StoreError::NamespaceNotFound(_)) => {
            Err(CommandError::NamespaceNotFound(ns.to_string()))
        }
        Err(shroudb_store::StoreError::Storage(msg)) if msg.contains("empty prefix") => {
            Err(CommandError::BadArg { message: msg })
        }
        Err(other) => Err(CommandError::Internal(format!("{ns}: {other}"))),
    }
}
