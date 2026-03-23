/// Command execution errors with machine-parseable code prefixes.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("DENIED {reason}")]
    Denied { reason: String },

    #[error("NOTFOUND {entity}: {id}")]
    NotFound { entity: String, id: String },

    #[error("BADARG {message}")]
    BadArg { message: String },

    #[error("VALIDATION_ERROR {0}")]
    ValidationError(String),

    #[error("WRONGTYPE keyspace={keyspace} type={actual} expected={expected}")]
    WrongType {
        keyspace: String,
        actual: String,
        expected: String,
    },

    #[error("STATE_ERROR from={from} to={to}")]
    StateError { from: String, to: String },

    #[error("EXPIRED {entity}: {id}")]
    Expired { entity: String, id: String },

    #[error("REUSE_DETECTED family={family_id}")]
    ReuseDetected { family_id: String },

    #[error("CHAIN_LIMIT family={family_id} limit={limit}")]
    ChainLimit { family_id: String, limit: u32 },

    #[error("DISABLED keyspace={keyspace}")]
    Disabled { keyspace: String },

    #[error("NOTREADY {0}")]
    NotReady(String),

    #[error("STORAGE {0}")]
    Storage(#[from] keyva_storage::StorageError),

    #[error("CRYPTO {0}")]
    Crypto(#[from] keyva_crypto::CryptoError),

    #[error("LOCKED account temporarily locked, retry_after={retry_after_secs}")]
    Locked { retry_after_secs: u64 },

    #[error("INTERNAL {0}")]
    Internal(String),
}
