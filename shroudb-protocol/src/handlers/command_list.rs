use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle() -> Result<ResponseMap, CommandError> {
    let commands = vec![
        "AUTH",
        "PING",
        "PUT",
        "GET",
        "DELETE",
        "LIST",
        "VERSIONS",
        "NAMESPACE CREATE",
        "NAMESPACE DROP",
        "NAMESPACE LIST",
        "NAMESPACE INFO",
        "NAMESPACE ALTER",
        "NAMESPACE VALIDATE",
        "PIPELINE",
        "SUBSCRIBE",
        "UNSUBSCRIBE",
        "HEALTH",
        "CONFIG GET",
        "CONFIG SET",
        "COMMAND LIST",
    ];

    let values: Vec<ResponseValue> = commands
        .iter()
        .map(|c| ResponseValue::String((*c).into()))
        .collect();

    Ok(ResponseMap::ok()
        .with("count", ResponseValue::Integer(values.len() as i64))
        .with("commands", ResponseValue::Array(values)))
}
