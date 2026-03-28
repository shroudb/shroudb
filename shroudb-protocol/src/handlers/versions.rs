use shroudb_store::{EntryState, Store};

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle(
    store: &impl Store,
    ns: &str,
    key: &[u8],
    limit: usize,
    from_version: Option<u64>,
) -> Result<ResponseMap, CommandError> {
    let versions = store
        .versions(ns, key, limit, from_version)
        .await
        .map_err(|e| {
            CommandError::Internal(format!("{ns}/{}: {e}", String::from_utf8_lossy(key)))
        })?;

    let entries: Vec<ResponseValue> = versions
        .into_iter()
        .map(|v| {
            let state_str = match v.state {
                EntryState::Active => "active",
                EntryState::Deleted => "deleted",
            };
            ResponseValue::Map(
                ResponseMap::ok()
                    .with("version", ResponseValue::Integer(v.version as i64))
                    .with("state", ResponseValue::String(state_str.to_string()))
                    .with("updated_at", ResponseValue::Integer(v.updated_at as i64))
                    .with("actor", ResponseValue::String(v.actor)),
            )
        })
        .collect();

    Ok(ResponseMap::ok().with("versions", ResponseValue::Array(entries)))
}
