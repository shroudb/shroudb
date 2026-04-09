use shroudb_store::Store;

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle(
    store: &impl Store,
    ns: &str,
    prefix: Option<&[u8]>,
    cursor: Option<&str>,
    limit: usize,
) -> Result<ResponseMap, CommandError> {
    let page = store
        .list(ns, prefix, cursor, limit)
        .await
        .map_err(|e| match e {
            shroudb_store::StoreError::NamespaceNotFound(_) => {
                CommandError::NamespaceNotFound(ns.to_string())
            }
            shroudb_store::StoreError::InvalidCursor(msg) => CommandError::BadArg { message: msg },
            other => CommandError::Internal(format!("{ns}: {other}")),
        })?;

    let keys: Vec<ResponseValue> = page.keys.into_iter().map(ResponseValue::Bytes).collect();

    let mut resp = ResponseMap::ok().with("keys", ResponseValue::Array(keys));

    if let Some(cursor) = page.cursor {
        resp = resp.with("cursor", ResponseValue::String(cursor));
    }

    Ok(resp)
}
