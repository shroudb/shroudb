use shroudb_core::{Keyspace, KeyspaceType};
use shroudb_storage::StorageEngine;

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

/// Pagination/filter parameters for the KEYS command.
pub struct KeysParams {
    pub cursor: Option<String>,
    pub pattern: Option<String>,
    pub state_filter: Option<String>,
    pub count: Option<usize>,
}

pub async fn handle_keys(
    engine: &StorageEngine,
    keyspace: &Keyspace,
    params: &KeysParams,
) -> Result<ResponseMap, CommandError> {
    let ks_name = &keyspace.name;
    let page_size = params.count.unwrap_or(100);

    match keyspace.keyspace_type {
        KeyspaceType::ApiKey => {
            let idx =
                engine
                    .index()
                    .api_keys
                    .get(ks_name)
                    .ok_or_else(|| CommandError::NotFound {
                        entity: "index".into(),
                        id: ks_name.clone(),
                    })?;

            let mut entries: Vec<_> = idx.all_entries();

            // Sort by credential_id for stable cursor-based pagination
            entries.sort_by(|a, b| a.credential_id.as_str().cmp(b.credential_id.as_str()));

            // Pattern filter: substring match on credential_id
            if let Some(ref pattern) = params.pattern {
                entries.retain(|e| e.credential_id.as_str().contains(pattern.as_str()));
            }

            // State filter
            if let Some(ref state_filter) = params.state_filter {
                let lower = state_filter.to_lowercase();
                entries.retain(|e| e.state.to_string().to_lowercase() == lower);
            }

            // Cursor: skip entries up to and including the cursor value
            if let Some(ref cursor) = params.cursor {
                entries.retain(|e| e.credential_id.as_str() > cursor.as_str());
            }

            let total_remaining = entries.len();
            let page: Vec<_> = entries.into_iter().take(page_size).collect();
            let has_more = total_remaining > page_size;

            let next_cursor = if has_more {
                page.last()
                    .map(|e| ResponseValue::String(e.credential_id.as_str().to_string()))
                    .unwrap_or(ResponseValue::Null)
            } else {
                ResponseValue::Null
            };

            let result: Vec<ResponseValue> = page
                .iter()
                .map(|e| {
                    ResponseValue::Map(
                        ResponseMap::ok()
                            .with(
                                "credential_id",
                                ResponseValue::String(e.credential_id.as_str().to_string()),
                            )
                            .with("state", ResponseValue::String(e.state.to_string()))
                            .with("created_at", ResponseValue::Integer(e.created_at as i64)),
                    )
                })
                .collect();

            Ok(ResponseMap::ok()
                .with("keys", ResponseValue::Array(result))
                .with("count", ResponseValue::Integer(page.len() as i64))
                .with("cursor", next_cursor))
        }
        KeyspaceType::RefreshToken => {
            let idx = engine.index().refresh_tokens.get(ks_name).ok_or_else(|| {
                CommandError::NotFound {
                    entity: "index".into(),
                    id: ks_name.clone(),
                }
            })?;

            let mut entries: Vec<_> = idx.all_entries();

            // Sort by credential_id for stable cursor-based pagination
            entries.sort_by(|a, b| a.credential_id.as_str().cmp(b.credential_id.as_str()));

            // Pattern filter: substring match on credential_id
            if let Some(ref pattern) = params.pattern {
                entries.retain(|e| e.credential_id.as_str().contains(pattern.as_str()));
            }

            // State filter
            if let Some(ref state_filter) = params.state_filter {
                let lower = state_filter.to_lowercase();
                entries.retain(|e| e.state.to_string().to_lowercase() == lower);
            }

            // Cursor: skip entries up to and including the cursor value
            if let Some(ref cursor) = params.cursor {
                entries.retain(|e| e.credential_id.as_str() > cursor.as_str());
            }

            let total_remaining = entries.len();
            let page: Vec<_> = entries.into_iter().take(page_size).collect();
            let has_more = total_remaining > page_size;

            let next_cursor = if has_more {
                page.last()
                    .map(|e| ResponseValue::String(e.credential_id.as_str().to_string()))
                    .unwrap_or(ResponseValue::Null)
            } else {
                ResponseValue::Null
            };

            let result: Vec<ResponseValue> = page
                .iter()
                .map(|e| {
                    ResponseValue::Map(
                        ResponseMap::ok()
                            .with(
                                "credential_id",
                                ResponseValue::String(e.credential_id.as_str().to_string()),
                            )
                            .with(
                                "family_id",
                                ResponseValue::String(e.family_id.as_str().to_string()),
                            )
                            .with("state", ResponseValue::String(e.state.to_string()))
                            .with("created_at", ResponseValue::Integer(e.created_at as i64)),
                    )
                })
                .collect();

            Ok(ResponseMap::ok()
                .with("keys", ResponseValue::Array(result))
                .with("count", ResponseValue::Integer(page.len() as i64))
                .with("cursor", next_cursor))
        }
        _ => Err(CommandError::WrongType {
            keyspace: ks_name.clone(),
            actual: format!("{:?}", keyspace.keyspace_type),
            expected: "api_key or refresh_token".into(),
        }),
    }
}
