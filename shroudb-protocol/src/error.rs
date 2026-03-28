/// Errors returned by command handlers.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("not found")]
    NotFound,

    #[error("namespace not found: {0}")]
    NamespaceNotFound(String),

    #[error("namespace already exists: {0}")]
    NamespaceExists(String),

    #[error("namespace not empty: {0}")]
    NamespaceNotEmpty(String),

    #[error("version not found: {0}")]
    VersionNotFound(u64),

    #[error("validation failed: {0}")]
    ValidationFailed(String),

    #[error("permission denied: {reason}")]
    Denied { reason: String },

    #[error("not authenticated")]
    NotAuthenticated,

    #[error("not ready: server is in {0} state")]
    NotReady(String),

    #[error("bad argument: {message}")]
    BadArg { message: String },

    #[error("pipeline aborted: command {index} failed: {reason}")]
    PipelineAborted { index: usize, reason: String },

    #[error("store error: {0}")]
    Store(#[from] shroudb_store::StoreError),

    #[error("internal error: {0}")]
    Internal(String),
}
