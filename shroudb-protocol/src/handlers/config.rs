use shroudb_storage::StorageEngine;

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle_config_get(
    engine: &StorageEngine,
    key: &str,
) -> Result<ResponseMap, CommandError> {
    match key {
        "fsync_mode" => Ok(ResponseMap::ok().with(
            "value",
            ResponseValue::String(format!("{:?}", engine.fsync_mode())),
        )),
        "snapshot_entry_threshold" => Ok(ResponseMap::ok().with(
            "value",
            ResponseValue::Integer(engine.snapshot_entry_threshold() as i64),
        )),
        "snapshot_time_threshold_secs" => Ok(ResponseMap::ok().with(
            "value",
            ResponseValue::Integer(engine.snapshot_time_threshold_secs() as i64),
        )),
        "health" => {
            Ok(ResponseMap::ok().with("value", ResponseValue::String(engine.health().to_string())))
        }
        _ => Err(CommandError::NotFound {
            entity: "config".into(),
            id: key.into(),
        }),
    }
}

pub async fn handle_config_set(_key: &str, _value: &str) -> Result<ResponseMap, CommandError> {
    Err(CommandError::BadArg {
        message: "runtime config changes not supported — update config file and restart".into(),
    })
}
