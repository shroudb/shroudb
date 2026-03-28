use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle() -> Result<ResponseMap, CommandError> {
    Ok(ResponseMap::ok().with("message", ResponseValue::String("healthy".into())))
}
