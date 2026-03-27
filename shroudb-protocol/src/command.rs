/// Protocol-agnostic command representation.
/// Produced by the RESP3 parser (or any future protocol adapter).
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
    ConfigList,
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

    // === Keyspace management ===
    KeyspaceCreate {
        name: String,
        keyspace_type: String,
        algorithm: Option<String>,
        rotation_days: Option<u32>,
        drain_days: Option<u32>,
        default_ttl_secs: Option<u64>,
    },

    // === Auth ===
    Auth {
        token: String,
    },

    // === Introspection ===
    /// Simple connectivity check — returns PONG.
    Ping,
    /// List all supported commands.
    CommandList,

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
            | Command::ConfigList
            | Command::Auth { .. }
            | Command::Ping
            | Command::CommandList => ReplicaBehavior::PureRead,

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
                | Command::ConfigList
                | Command::Auth { .. }
                | Command::Ping
                | Command::CommandList
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
            Command::KeyspaceCreate { .. }
            | Command::Ping
            | Command::CommandList => None,
            _ => None,
        }
    }

    /// Serialize this command to RESP3 wire arguments (array of strings).
    ///
    /// This is the inverse of `parse_command` — given a `Command` enum, produce
    /// the string arguments that would be sent over the wire.
    pub fn to_wire_args(&self) -> Vec<String> {
        match self {
            Command::Issue {
                keyspace,
                claims,
                metadata,
                ttl_secs,
                idempotency_key,
            } => {
                let mut args = vec!["ISSUE".into(), keyspace.clone()];
                if let Some(c) = claims {
                    args.extend(["CLAIMS".into(), c.to_string()]);
                }
                if let Some(m) = metadata {
                    args.extend(["META".into(), m.to_string()]);
                }
                if let Some(t) = ttl_secs {
                    args.extend(["TTL".into(), t.to_string()]);
                }
                if let Some(k) = idempotency_key {
                    args.extend(["IDEMPOTENCY_KEY".into(), k.clone()]);
                }
                args
            }
            Command::Verify {
                keyspace,
                token,
                payload,
                check_revoked,
            } => {
                let mut args = vec!["VERIFY".into(), keyspace.clone(), token.clone()];
                if let Some(p) = payload {
                    args.extend(["PAYLOAD".into(), p.clone()]);
                }
                if *check_revoked {
                    args.push("CHECKREV".into());
                }
                args
            }
            Command::Revoke {
                keyspace,
                target,
                ttl_secs,
            } => {
                let mut args = vec!["REVOKE".into(), keyspace.clone()];
                match target {
                    RevokeTarget::Single(id) => args.push(id.clone()),
                    RevokeTarget::Family(fid) => args.extend(["FAMILY".into(), fid.clone()]),
                    RevokeTarget::Bulk(ids) => {
                        args.push("BULK".into());
                        args.extend(ids.iter().cloned());
                    }
                }
                if let Some(t) = ttl_secs {
                    args.extend(["TTL".into(), t.to_string()]);
                }
                args
            }
            Command::Refresh { keyspace, token } => {
                vec!["REFRESH".into(), keyspace.clone(), token.clone()]
            }
            Command::Update {
                keyspace,
                credential_id,
                metadata,
            } => {
                vec![
                    "UPDATE".into(),
                    keyspace.clone(),
                    credential_id.clone(),
                    "META".into(),
                    metadata.to_string(),
                ]
            }
            Command::Inspect {
                keyspace,
                credential_id,
            } => {
                vec!["INSPECT".into(), keyspace.clone(), credential_id.clone()]
            }
            Command::Rotate {
                keyspace,
                force,
                nowait,
                dryrun,
            } => {
                let mut args = vec!["ROTATE".into(), keyspace.clone()];
                if *force {
                    args.push("FORCE".into());
                }
                if *nowait {
                    args.push("NOWAIT".into());
                }
                if *dryrun {
                    args.push("DRYRUN".into());
                }
                args
            }
            Command::Jwks { keyspace } => vec!["JWKS".into(), keyspace.clone()],
            Command::KeyState { keyspace } => vec!["KEYSTATE".into(), keyspace.clone()],
            Command::Health { keyspace } => {
                let mut args = vec!["HEALTH".into()];
                if let Some(ks) = keyspace {
                    args.push(ks.clone());
                }
                args
            }
            Command::Keys {
                keyspace,
                cursor,
                pattern,
                state_filter,
                count,
            } => {
                let mut args = vec!["KEYS".into(), keyspace.clone()];
                if let Some(c) = cursor {
                    args.extend(["CURSOR".into(), c.clone()]);
                }
                if let Some(p) = pattern {
                    args.extend(["MATCH".into(), p.clone()]);
                }
                if let Some(s) = state_filter {
                    args.extend(["STATE".into(), s.clone()]);
                }
                if let Some(n) = count {
                    args.extend(["COUNT".into(), n.to_string()]);
                }
                args
            }
            Command::Suspend {
                keyspace,
                credential_id,
            } => {
                vec!["SUSPEND".into(), keyspace.clone(), credential_id.clone()]
            }
            Command::Unsuspend {
                keyspace,
                credential_id,
            } => {
                vec!["UNSUSPEND".into(), keyspace.clone(), credential_id.clone()]
            }
            Command::Schema { keyspace } => vec!["SCHEMA".into(), keyspace.clone()],
            Command::ConfigGet { key } => vec!["CONFIG".into(), "GET".into(), key.clone()],
            Command::ConfigSet { key, value } => {
                vec!["CONFIG".into(), "SET".into(), key.clone(), value.clone()]
            }
            Command::ConfigList => vec!["CONFIG".into(), "LIST".into()],
            Command::Subscribe { channel } => vec!["SUBSCRIBE".into(), channel.clone()],
            Command::PasswordSet {
                keyspace,
                user_id,
                plaintext,
                metadata,
            } => {
                let mut args = vec![
                    "PASSWORD".into(),
                    "SET".into(),
                    keyspace.clone(),
                    user_id.clone(),
                    plaintext.clone(),
                ];
                if let Some(m) = metadata {
                    args.extend(["META".into(), m.to_string()]);
                }
                args
            }
            Command::PasswordVerify {
                keyspace,
                user_id,
                plaintext,
            } => {
                vec![
                    "PASSWORD".into(),
                    "VERIFY".into(),
                    keyspace.clone(),
                    user_id.clone(),
                    plaintext.clone(),
                ]
            }
            Command::PasswordChange {
                keyspace,
                user_id,
                old_plaintext,
                new_plaintext,
            } => {
                vec![
                    "PASSWORD".into(),
                    "CHANGE".into(),
                    keyspace.clone(),
                    user_id.clone(),
                    old_plaintext.clone(),
                    new_plaintext.clone(),
                ]
            }
            Command::PasswordReset {
                keyspace,
                user_id,
                new_plaintext,
            } => {
                vec![
                    "PASSWORD".into(),
                    "RESET".into(),
                    keyspace.clone(),
                    user_id.clone(),
                    new_plaintext.clone(),
                ]
            }
            Command::PasswordImport {
                keyspace,
                user_id,
                hash,
                metadata,
            } => {
                let mut args = vec![
                    "PASSWORD".into(),
                    "IMPORT".into(),
                    keyspace.clone(),
                    user_id.clone(),
                    hash.clone(),
                ];
                if let Some(m) = metadata {
                    args.extend(["META".into(), m.to_string()]);
                }
                args
            }
            Command::KeyspaceCreate {
                name,
                keyspace_type,
                algorithm,
                rotation_days,
                drain_days,
                default_ttl_secs,
            } => {
                let mut args = vec![
                    "KEYSPACE_CREATE".into(),
                    name.clone(),
                    "TYPE".into(),
                    keyspace_type.clone(),
                ];
                if let Some(a) = algorithm {
                    args.extend(["ALGORITHM".into(), a.clone()]);
                }
                if let Some(r) = rotation_days {
                    args.extend(["ROTATION_DAYS".into(), r.to_string()]);
                }
                if let Some(d) = drain_days {
                    args.extend(["DRAIN_DAYS".into(), d.to_string()]);
                }
                if let Some(t) = default_ttl_secs {
                    args.extend(["TTL".into(), t.to_string()]);
                }
                args
            }
            Command::Auth { token } => vec!["AUTH".into(), token.clone()],
            Command::Ping => vec!["PING".into()],
            Command::CommandList => vec!["COMMAND".into(), "LIST".into()],
            Command::Pipeline(commands) => {
                let mut args = vec!["PIPELINE".into()];
                for cmd in commands {
                    args.extend(cmd.to_wire_args());
                }
                args.push("END".into());
                args
            }
        }
    }
}
