use axum::http::StatusCode;
use keyva_protocol::CommandError;

/// Maps a `CommandError` variant to the appropriate HTTP status code.
pub fn error_to_status(err: &CommandError) -> StatusCode {
    match err {
        CommandError::BadArg { .. } | CommandError::WrongType { .. } => StatusCode::BAD_REQUEST,
        CommandError::ValidationError(_) => StatusCode::UNPROCESSABLE_ENTITY,
        CommandError::NotFound { .. } => StatusCode::NOT_FOUND,
        CommandError::Denied { .. } => StatusCode::FORBIDDEN,
        CommandError::Expired { .. } => StatusCode::GONE,
        CommandError::Disabled { .. } | CommandError::NotReady(_) => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        CommandError::Locked { .. } => StatusCode::TOO_MANY_REQUESTS,
        CommandError::ReuseDetected { .. }
        | CommandError::ChainLimit { .. }
        | CommandError::StateError { .. } => StatusCode::CONFLICT,
        CommandError::Storage(_) | CommandError::Crypto(_) | CommandError::Internal(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}
