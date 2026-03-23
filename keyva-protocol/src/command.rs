/// Protocol-agnostic command representation.
/// Produced by RESP3 parser, REST deserializer, or gRPC deserializer.
#[derive(Debug, Clone)]
pub enum Command {
    // === Write commands ===
    Issue {
        keyspace: String,
        claims: Option<serde_json::Value>,
        metadata: Option<serde_json::Value>,
        ttl_secs: Option<u64>,
        idempotency_key: Option<String>,
    },
    Verify {
        keyspace: String,
        token: String,
        payload: Option<String>,
        check_revoked: bool,
    },
    Revoke {
        keyspace: String,
        target: RevokeTarget,
        ttl_secs: Option<u64>,
    },
    Refresh {
        keyspace: String,
        token: String,
    },
    Update {
        keyspace: String,
        credential_id: String,
        metadata: serde_json::Value,
    },
    Inspect {
        keyspace: String,
        credential_id: String,
    },

    // === Key management ===
    Rotate {
        keyspace: String,
        force: bool,
        nowait: bool,
        dryrun: bool,
    },
    Jwks {
        keyspace: String,
    },
    KeyState {
        keyspace: String,
    },

    // === Operational ===
    Health {
        keyspace: Option<String>,
    },
    Keys {
        keyspace: String,
        cursor: Option<String>,
        pattern: Option<String>,
        state_filter: Option<String>,
        count: Option<usize>,
    },
    Suspend {
        keyspace: String,
        credential_id: String,
    },
    Unsuspend {
        keyspace: String,
        credential_id: String,
    },
    Schema {
        keyspace: String,
    },
    ConfigGet {
        key: String,
    },
    ConfigSet {
        key: String,
        value: String,
    },
    Subscribe {
        channel: String,
    },

    // === Password commands ===
    PasswordSet {
        keyspace: String,
        user_id: String,
        plaintext: String,
        metadata: Option<serde_json::Value>,
    },
    PasswordVerify {
        keyspace: String,
        user_id: String,
        plaintext: String,
    },
    PasswordChange {
        keyspace: String,
        user_id: String,
        old_plaintext: String,
        new_plaintext: String,
    },
    /// Force-reset a password without requiring the old password.
    /// Authorization must be handled externally (e.g., a verified reset token).
    PasswordReset {
        keyspace: String,
        user_id: String,
        new_plaintext: String,
    },
    PasswordImport {
        keyspace: String,
        user_id: String,
        hash: String,
        metadata: Option<serde_json::Value>,
    },

    // === Auth ===
    Auth {
        token: String,
    },

    // === Pipeline ===
    Pipeline(Vec<Command>),
}

/// Target for revocation commands.
#[derive(Debug, Clone)]
pub enum RevokeTarget {
    Single(String),
    Family(String),
    Bulk(Vec<String>),
}

/// Behavior classification for replica deployment.
///
/// This is a design-time classification, not a runtime enforcement mechanism.
/// It documents intent for when replication is built.
///
/// Key design decisions:
/// - VERIFY on replicas: returns verification result but does NOT update
///   `last_verified_at` and does NOT check the local revocation set (which
///   may be stale on replicas). Consumers who need revocation freshness
///   should verify against the primary.
/// - ObservationalRead commands have side effects (tracking, metrics) that
///   are skipped on replicas to avoid WAL writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicaBehavior {
    /// Safe on replicas with no side effects.
    PureRead,
    /// Read with side effects (tracking, metrics). On replicas, side effects are skipped.
    ObservationalRead,
    /// Read on replicas (verify only), may write on primary (rehash). Used by PASSWORD VERIFY.
    ConditionalWrite,
    /// Must execute on primary only.
    WriteOnly,
}

impl Command {
    /// Returns the replica behavior classification for this command.
    pub fn replica_behavior(&self) -> ReplicaBehavior {
        match self {
            // Pure reads -- safe on replicas
            Command::Inspect { .. }
            | Command::Jwks { .. }
            | Command::KeyState { .. }
            | Command::Health { .. }
            | Command::Keys { .. }
            | Command::Schema { .. }
            | Command::ConfigGet { .. }
            | Command::Auth { .. } => ReplicaBehavior::PureRead,

            // Observational reads -- side effects skipped on replicas
            Command::Verify { .. } => ReplicaBehavior::ObservationalRead,

            // Conditional write -- verify on replicas, may rehash on primary
            Command::PasswordVerify { .. } => ReplicaBehavior::ConditionalWrite,

            // Writes -- primary only
            _ => ReplicaBehavior::WriteOnly,
        }
    }

    /// Returns true if this is a read-only command (no WAL write).
    pub fn is_read(&self) -> bool {
        matches!(
            self,
            Command::Verify { .. }
                | Command::PasswordVerify { .. }
                | Command::Inspect { .. }
                | Command::Jwks { .. }
                | Command::KeyState { .. }
                | Command::Health { .. }
                | Command::Keys { .. }
                | Command::Schema { .. }
                | Command::ConfigGet { .. }
                | Command::Auth { .. }
        )
    }

    /// Returns the keyspace name, if applicable.
    pub fn keyspace(&self) -> Option<&str> {
        match self {
            Command::Issue { keyspace, .. }
            | Command::Verify { keyspace, .. }
            | Command::Revoke { keyspace, .. }
            | Command::Refresh { keyspace, .. }
            | Command::Update { keyspace, .. }
            | Command::Inspect { keyspace, .. }
            | Command::Rotate { keyspace, .. }
            | Command::Jwks { keyspace, .. }
            | Command::KeyState { keyspace, .. }
            | Command::Keys { keyspace, .. }
            | Command::Suspend { keyspace, .. }
            | Command::Unsuspend { keyspace, .. }
            | Command::Schema { keyspace, .. }
            | Command::PasswordSet { keyspace, .. }
            | Command::PasswordVerify { keyspace, .. }
            | Command::PasswordChange { keyspace, .. }
            | Command::PasswordReset { keyspace, .. }
            | Command::PasswordImport { keyspace, .. } => Some(keyspace),
            Command::Health { keyspace, .. } => keyspace.as_deref(),
            _ => None,
        }
    }
}
