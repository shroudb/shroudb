use keyva_core::Keyspace;

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle_schema(keyspace: &Keyspace) -> Result<ResponseMap, CommandError> {
    match &keyspace.meta_schema {
        Some(schema) => {
            let schema_json = serde_json::to_value(schema)
                .map_err(|e| CommandError::Internal(format!("failed to serialize schema: {e}")))?;
            Ok(ResponseMap::ok()
                .with("schema", ResponseValue::Json(schema_json))
                .with("enforce", ResponseValue::Boolean(schema.enforce)))
        }
        None => Ok(ResponseMap::ok().with("schema", ResponseValue::Null).with(
            "message",
            ResponseValue::String("no metadata schema defined — all metadata accepted".into()),
        )),
    }
}
