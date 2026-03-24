use crate::error::CommandError;
use crate::response::ResponseMap;

/// Fallback handler for SUBSCRIBE when invoked outside a persistent TCP connection.
///
/// Real subscription mode is handled at the connection level in
/// `shroudb/src/connection.rs`, which enters a streaming loop. This handler
/// is only reached via non-streaming transports (REST, gRPC) where
/// server-push is not supported in the same way.
pub async fn handle_subscribe(_channel: &str) -> Result<ResponseMap, CommandError> {
    Err(CommandError::BadArg {
        message: "SUBSCRIBE is only supported on persistent TCP connections".into(),
    })
}
