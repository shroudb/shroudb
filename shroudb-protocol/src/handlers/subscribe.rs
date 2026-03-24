use crate::error::CommandError;
use crate::response::ResponseMap;

/// Fallback handler for SUBSCRIBE when invoked outside a persistent TCP connection.
///
/// Real subscription mode is handled at the connection level in
/// `shroudb/src/connection.rs`, which enters a streaming loop. This handler
/// is only reached when dispatched outside the connection-level streaming
/// context (e.g., via pipeline or non-TCP transports).
pub async fn handle_subscribe(_channel: &str) -> Result<ResponseMap, CommandError> {
    Err(CommandError::BadArg {
        message: "SUBSCRIBE is only supported on persistent TCP connections".into(),
    })
}
