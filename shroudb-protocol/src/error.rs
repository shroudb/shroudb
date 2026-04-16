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

    /// Compare-and-swap precondition failed. The wire format carries `current`
    /// so callers can retry without a second round-trip: `VERSIONCONFLICT current=5`.
    #[error("VERSIONCONFLICT current={current}")]
    VersionConflict { current: u64 },

    /// Prefix-delete matched more keys than the configured per-call cap.
    /// Wire format: `PREFIXTOOLARGE matched=N limit=M`.
    #[error("PREFIXTOOLARGE matched={matched} limit={limit}")]
    PrefixTooLarge { matched: u64, limit: u64 },

    #[error("store error: {0}")]
    Store(#[from] shroudb_store::StoreError),

    #[error("internal error: {0}")]
    Internal(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_conflict_wire_format() {
        let err = CommandError::VersionConflict { current: 5 };
        assert_eq!(err.to_string(), "VERSIONCONFLICT current=5");
    }

    #[test]
    fn prefix_too_large_wire_format() {
        let err = CommandError::PrefixTooLarge {
            matched: 150_000,
            limit: 100_000,
        };
        assert_eq!(
            err.to_string(),
            "PREFIXTOOLARGE matched=150000 limit=100000"
        );
    }
}
