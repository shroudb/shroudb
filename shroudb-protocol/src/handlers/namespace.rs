use shroudb_store::{MetaSchema, NamespaceConfig, Store};

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

fn parse_config(
    schema_json: Option<serde_json::Value>,
    max_versions: Option<u64>,
    tombstone_retention_secs: Option<u64>,
) -> Result<NamespaceConfig, CommandError> {
    let meta_schema = match schema_json {
        Some(json) => {
            let schema: MetaSchema =
                serde_json::from_value(json).map_err(|e| CommandError::BadArg {
                    message: format!("invalid schema: {e}"),
                })?;
            Some(schema)
        }
        None => None,
    };

    Ok(NamespaceConfig {
        meta_schema,
        max_versions,
        tombstone_retention_secs,
    })
}

pub async fn handle_create(
    store: &impl Store,
    name: &str,
    schema_json: Option<serde_json::Value>,
    max_versions: Option<u64>,
    tombstone_retention_secs: Option<u64>,
) -> Result<ResponseMap, CommandError> {
    let config = parse_config(schema_json, max_versions, tombstone_retention_secs)?;
    store.namespace_create(name, config).await?;
    Ok(ResponseMap::ok())
}

pub async fn handle_drop(
    store: &impl Store,
    name: &str,
    force: bool,
) -> Result<ResponseMap, CommandError> {
    store.namespace_drop(name, force).await?;
    Ok(ResponseMap::ok())
}

pub async fn handle_list(
    store: &impl Store,
    cursor: Option<&str>,
    limit: usize,
) -> Result<ResponseMap, CommandError> {
    let page = store.namespace_list(cursor, limit).await?;

    let names: Vec<ResponseValue> = page
        .keys
        .into_iter()
        .map(|k| ResponseValue::String(String::from_utf8_lossy(&k).into_owned()))
        .collect();

    let mut resp = ResponseMap::ok().with("namespaces", ResponseValue::Array(names));

    if let Some(cursor) = page.cursor {
        resp = resp.with("cursor", ResponseValue::String(cursor));
    }

    Ok(resp)
}

pub async fn handle_info(store: &impl Store, name: &str) -> Result<ResponseMap, CommandError> {
    let info = store.namespace_info(name).await?;

    Ok(ResponseMap::ok()
        .with("name", ResponseValue::String(info.name))
        .with("key_count", ResponseValue::Integer(info.key_count as i64))
        .with("created_at", ResponseValue::Integer(info.created_at as i64)))
}

pub async fn handle_alter(
    store: &impl Store,
    name: &str,
    schema_json: Option<serde_json::Value>,
    max_versions: Option<u64>,
    tombstone_retention_secs: Option<u64>,
) -> Result<ResponseMap, CommandError> {
    let config = parse_config(schema_json, max_versions, tombstone_retention_secs)?;
    store.namespace_alter(name, config).await?;
    Ok(ResponseMap::ok())
}

pub async fn handle_validate(store: &impl Store, name: &str) -> Result<ResponseMap, CommandError> {
    let reports = store.namespace_validate(name).await?;

    let entries: Vec<ResponseValue> = reports
        .into_iter()
        .map(|r| {
            let errors: Vec<ResponseValue> = r
                .errors
                .into_iter()
                .map(|e| ResponseValue::String(e.to_string()))
                .collect();
            ResponseValue::Map(
                ResponseMap::ok()
                    .with("key", ResponseValue::Bytes(r.key))
                    .with("version", ResponseValue::Integer(r.version as i64))
                    .with("errors", ResponseValue::Array(errors)),
            )
        })
        .collect();

    Ok(ResponseMap::ok()
        .with("count", ResponseValue::Integer(entries.len() as i64))
        .with("reports", ResponseValue::Array(entries)))
}
