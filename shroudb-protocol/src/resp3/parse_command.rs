use crate::command::{Command, RevokeTarget};
use crate::error::CommandError;

use super::Resp3Frame;

/// Convert a RESP3 frame (an array of bulk strings) into a `Command`.
pub fn parse_command(frame: Resp3Frame) -> Result<Command, CommandError> {
    let parts = match frame {
        Resp3Frame::Array(parts) => parts,
        _ => {
            return Err(CommandError::BadArg {
                message: "expected array frame".into(),
            });
        }
    };

    let strings: Vec<String> = parts
        .into_iter()
        .map(frame_to_string)
        .collect::<Result<_, _>>()?;

    if strings.is_empty() {
        return Err(CommandError::BadArg {
            message: "empty command".into(),
        });
    }

    let verb = strings[0].to_ascii_uppercase();
    let args = &strings[1..];

    match verb.as_str() {
        "ISSUE" => parse_issue(args),
        "VERIFY" => parse_verify(args),
        "REVOKE" => parse_revoke(args),
        "REFRESH" => parse_refresh(args),
        "UPDATE" => parse_update(args),
        "INSPECT" => parse_inspect(args),
        "ROTATE" => parse_rotate(args),
        "JWKS" => parse_jwks(args),
        "KEYSTATE" => parse_keystate(args),
        "HEALTH" => parse_health(args),
        "KEYS" => parse_keys(args),
        "SUSPEND" => parse_suspend(args),
        "UNSUSPEND" => parse_unsuspend(args),
        "SCHEMA" => parse_schema(args),
        "CONFIG" => parse_config(args),
        "SUBSCRIBE" => parse_subscribe(args),
        "PASSWORD" => parse_password(args),
        "KEYSPACE_CREATE" => parse_keyspace_create(args),
        "AUTH" => parse_auth(args),
        "PIPELINE" => parse_pipeline(&strings),
        _ => Err(CommandError::BadArg {
            message: format!("unknown command: {verb}"),
        }),
    }
}

fn frame_to_string(frame: Resp3Frame) -> Result<String, CommandError> {
    match frame {
        Resp3Frame::BulkString(data) => String::from_utf8(data).map_err(|_| CommandError::BadArg {
            message: "non-UTF-8 bulk string".into(),
        }),
        _ => Err(CommandError::BadArg {
            message: "expected bulk string element".into(),
        }),
    }
}

fn require_arg<'a>(args: &'a [String], index: usize, name: &str) -> Result<&'a str, CommandError> {
    args.get(index)
        .map(|s| s.as_str())
        .ok_or_else(|| CommandError::BadArg {
            message: format!("missing required argument: {name}"),
        })
}

/// Find a keyword in the args and return the value after it.
fn find_opt<'a>(args: &'a [String], keyword: &str) -> Option<&'a str> {
    args.windows(2).find_map(|w| {
        if w[0].eq_ignore_ascii_case(keyword) {
            Some(w[1].as_str())
        } else {
            None
        }
    })
}

/// Check if a keyword flag is present.
fn has_flag(args: &[String], keyword: &str) -> bool {
    args.iter().any(|a| a.eq_ignore_ascii_case(keyword))
}

// ISSUE <keyspace> [CLAIMS <json>] [META <json>] [TTL <secs>] [IDEMPOTENCY_KEY <key>]
fn parse_issue(args: &[String]) -> Result<Command, CommandError> {
    let keyspace = require_arg(args, 0, "keyspace")?.to_owned();
    let rest = &args[1..];

    let claims = find_opt(rest, "CLAIMS")
        .map(|s| {
            serde_json::from_str(s).map_err(|e| CommandError::BadArg {
                message: format!("invalid CLAIMS json: {e}"),
            })
        })
        .transpose()?;

    let metadata = find_opt(rest, "META")
        .map(|s| {
            serde_json::from_str(s).map_err(|e| CommandError::BadArg {
                message: format!("invalid META json: {e}"),
            })
        })
        .transpose()?;

    let ttl_secs = find_opt(rest, "TTL")
        .map(|s| {
            s.parse::<u64>().map_err(|e| CommandError::BadArg {
                message: format!("invalid TTL: {e}"),
            })
        })
        .transpose()?;

    let idempotency_key = find_opt(rest, "IDEMPOTENCY_KEY").map(|s| s.to_owned());

    Ok(Command::Issue {
        keyspace,
        claims,
        metadata,
        ttl_secs,
        idempotency_key,
    })
}

// VERIFY <keyspace> <token> [PAYLOAD <data>] [CHECKREV]
fn parse_verify(args: &[String]) -> Result<Command, CommandError> {
    let keyspace = require_arg(args, 0, "keyspace")?.to_owned();
    let token = require_arg(args, 1, "token")?.to_owned();
    let rest = &args[2..];

    let payload = find_opt(rest, "PAYLOAD").map(|s| s.to_owned());
    let check_revoked = has_flag(rest, "CHECKREV");

    Ok(Command::Verify {
        keyspace,
        token,
        payload,
        check_revoked,
    })
}

// REVOKE <keyspace> <id>
// REVOKE <keyspace> FAMILY <fid>
// REVOKE <keyspace> BULK <id1> <id2> ...
// optional trailing [TTL <secs>]
fn parse_revoke(args: &[String]) -> Result<Command, CommandError> {
    let keyspace = require_arg(args, 0, "keyspace")?.to_owned();
    let rest = &args[1..];

    if rest.is_empty() {
        return Err(CommandError::BadArg {
            message: "REVOKE requires a target".into(),
        });
    }

    let second = rest[0].to_ascii_uppercase();

    let (target, remaining) = if second == "FAMILY" {
        let fid = require_arg(rest, 1, "family_id")?.to_owned();
        (RevokeTarget::Family(fid), &rest[2..])
    } else if second == "BULK" {
        // Collect IDs until we hit TTL or end
        let mut ids = Vec::new();
        let mut i = 1;
        while i < rest.len() {
            if rest[i].eq_ignore_ascii_case("TTL") {
                break;
            }
            ids.push(rest[i].clone());
            i += 1;
        }
        if ids.is_empty() {
            return Err(CommandError::BadArg {
                message: "REVOKE BULK requires at least one id".into(),
            });
        }
        (RevokeTarget::Bulk(ids), &rest[i..])
    } else {
        (RevokeTarget::Single(rest[0].clone()), &rest[1..])
    };

    let ttl_secs = find_opt(remaining, "TTL")
        .map(|s| {
            s.parse::<u64>().map_err(|e| CommandError::BadArg {
                message: format!("invalid TTL: {e}"),
            })
        })
        .transpose()?;

    Ok(Command::Revoke {
        keyspace,
        target,
        ttl_secs,
    })
}

// REFRESH <keyspace> <token>
fn parse_refresh(args: &[String]) -> Result<Command, CommandError> {
    let keyspace = require_arg(args, 0, "keyspace")?.to_owned();
    let token = require_arg(args, 1, "token")?.to_owned();
    Ok(Command::Refresh { keyspace, token })
}

// UPDATE <keyspace> <credential_id> META <json>
fn parse_update(args: &[String]) -> Result<Command, CommandError> {
    let keyspace = require_arg(args, 0, "keyspace")?.to_owned();
    let credential_id = require_arg(args, 1, "credential_id")?.to_owned();
    let rest = &args[2..];

    let meta_str = find_opt(rest, "META").ok_or_else(|| CommandError::BadArg {
        message: "UPDATE requires META <json>".into(),
    })?;

    let metadata = serde_json::from_str(meta_str).map_err(|e| CommandError::BadArg {
        message: format!("invalid META json: {e}"),
    })?;

    Ok(Command::Update {
        keyspace,
        credential_id,
        metadata,
    })
}

// INSPECT <keyspace> <credential_id>
fn parse_inspect(args: &[String]) -> Result<Command, CommandError> {
    let keyspace = require_arg(args, 0, "keyspace")?.to_owned();
    let credential_id = require_arg(args, 1, "credential_id")?.to_owned();
    Ok(Command::Inspect {
        keyspace,
        credential_id,
    })
}

// ROTATE <keyspace> [FORCE] [NOWAIT] [DRYRUN]
fn parse_rotate(args: &[String]) -> Result<Command, CommandError> {
    let keyspace = require_arg(args, 0, "keyspace")?.to_owned();
    let rest = &args[1..];
    Ok(Command::Rotate {
        keyspace,
        force: has_flag(rest, "FORCE"),
        nowait: has_flag(rest, "NOWAIT"),
        dryrun: has_flag(rest, "DRYRUN"),
    })
}

// JWKS <keyspace>
fn parse_jwks(args: &[String]) -> Result<Command, CommandError> {
    let keyspace = require_arg(args, 0, "keyspace")?.to_owned();
    Ok(Command::Jwks { keyspace })
}

// KEYSTATE <keyspace>
fn parse_keystate(args: &[String]) -> Result<Command, CommandError> {
    let keyspace = require_arg(args, 0, "keyspace")?.to_owned();
    Ok(Command::KeyState { keyspace })
}

// HEALTH [<keyspace>]
fn parse_health(args: &[String]) -> Result<Command, CommandError> {
    let keyspace = args.first().map(|s| s.to_owned());
    Ok(Command::Health { keyspace })
}

// KEYS <keyspace> [CURSOR <c>] [MATCH <p>] [STATE <f>] [COUNT <n>]
fn parse_keys(args: &[String]) -> Result<Command, CommandError> {
    let keyspace = require_arg(args, 0, "keyspace")?.to_owned();
    let rest = &args[1..];

    let cursor = find_opt(rest, "CURSOR").map(|s| s.to_owned());
    let pattern = find_opt(rest, "MATCH").map(|s| s.to_owned());
    let state_filter = find_opt(rest, "STATE").map(|s| s.to_owned());
    let count = find_opt(rest, "COUNT")
        .map(|s| {
            s.parse::<usize>().map_err(|e| CommandError::BadArg {
                message: format!("invalid COUNT: {e}"),
            })
        })
        .transpose()?;

    Ok(Command::Keys {
        keyspace,
        cursor,
        pattern,
        state_filter,
        count,
    })
}

// SUSPEND <keyspace> <credential_id>
fn parse_suspend(args: &[String]) -> Result<Command, CommandError> {
    let keyspace = require_arg(args, 0, "keyspace")?.to_owned();
    let credential_id = require_arg(args, 1, "credential_id")?.to_owned();
    Ok(Command::Suspend {
        keyspace,
        credential_id,
    })
}

// UNSUSPEND <keyspace> <credential_id>
fn parse_unsuspend(args: &[String]) -> Result<Command, CommandError> {
    let keyspace = require_arg(args, 0, "keyspace")?.to_owned();
    let credential_id = require_arg(args, 1, "credential_id")?.to_owned();
    Ok(Command::Unsuspend {
        keyspace,
        credential_id,
    })
}

// SCHEMA <keyspace>
fn parse_schema(args: &[String]) -> Result<Command, CommandError> {
    let keyspace = require_arg(args, 0, "keyspace")?.to_owned();
    Ok(Command::Schema { keyspace })
}

// CONFIG GET <key> | CONFIG SET <key> <value>
fn parse_config(args: &[String]) -> Result<Command, CommandError> {
    let sub = require_arg(args, 0, "subcommand")?;
    match sub.to_ascii_uppercase().as_str() {
        "GET" => {
            let key = require_arg(args, 1, "key")?.to_owned();
            Ok(Command::ConfigGet { key })
        }
        "SET" => {
            let key = require_arg(args, 1, "key")?.to_owned();
            let value = require_arg(args, 2, "value")?.to_owned();
            Ok(Command::ConfigSet { key, value })
        }
        "LIST" => Ok(Command::ConfigList),
        other => Err(CommandError::BadArg {
            message: format!("unknown CONFIG subcommand: {other}"),
        }),
    }
}

// PASSWORD SET <keyspace> <user_id> <plaintext> [META <json>]
// PASSWORD VERIFY <keyspace> <user_id> <plaintext>
// PASSWORD CHANGE <keyspace> <user_id> <old_plaintext> <new_plaintext>
// PASSWORD IMPORT <keyspace> <user_id> <hash> [META <json>]
fn parse_password(args: &[String]) -> Result<Command, CommandError> {
    let sub = require_arg(args, 0, "subcommand")?;
    match sub.to_ascii_uppercase().as_str() {
        "SET" => {
            let keyspace = require_arg(args, 1, "keyspace")?.to_owned();
            let user_id = require_arg(args, 2, "user_id")?.to_owned();
            let plaintext = require_arg(args, 3, "plaintext")?.to_owned();
            let rest = &args[4..];
            let metadata = find_opt(rest, "META")
                .map(|s| {
                    serde_json::from_str(s).map_err(|e| CommandError::BadArg {
                        message: format!("invalid META json: {e}"),
                    })
                })
                .transpose()?;
            Ok(Command::PasswordSet {
                keyspace,
                user_id,
                plaintext,
                metadata,
            })
        }
        "VERIFY" => {
            let keyspace = require_arg(args, 1, "keyspace")?.to_owned();
            let user_id = require_arg(args, 2, "user_id")?.to_owned();
            let plaintext = require_arg(args, 3, "plaintext")?.to_owned();
            Ok(Command::PasswordVerify {
                keyspace,
                user_id,
                plaintext,
            })
        }
        "CHANGE" => {
            let keyspace = require_arg(args, 1, "keyspace")?.to_owned();
            let user_id = require_arg(args, 2, "user_id")?.to_owned();
            let old_plaintext = require_arg(args, 3, "old_plaintext")?.to_owned();
            let new_plaintext = require_arg(args, 4, "new_plaintext")?.to_owned();
            Ok(Command::PasswordChange {
                keyspace,
                user_id,
                old_plaintext,
                new_plaintext,
            })
        }
        "RESET" => {
            let keyspace = require_arg(args, 1, "keyspace")?.to_owned();
            let user_id = require_arg(args, 2, "user_id")?.to_owned();
            let new_plaintext = require_arg(args, 3, "new_plaintext")?.to_owned();
            Ok(Command::PasswordReset {
                keyspace,
                user_id,
                new_plaintext,
            })
        }
        "IMPORT" => {
            let keyspace = require_arg(args, 1, "keyspace")?.to_owned();
            let user_id = require_arg(args, 2, "user_id")?.to_owned();
            let hash = require_arg(args, 3, "hash")?.to_owned();
            let rest = &args[4..];
            let metadata = find_opt(rest, "META")
                .map(|s| {
                    serde_json::from_str(s).map_err(|e| CommandError::BadArg {
                        message: format!("invalid META json: {e}"),
                    })
                })
                .transpose()?;
            Ok(Command::PasswordImport {
                keyspace,
                user_id,
                hash,
                metadata,
            })
        }
        other => Err(CommandError::BadArg {
            message: format!("unknown PASSWORD subcommand: {other}"),
        }),
    }
}

// KEYSPACE_CREATE <name> TYPE <type> [ALGORITHM <alg>] [ROTATION_DAYS <n>] [DRAIN_DAYS <n>] [TTL <n>]
fn parse_keyspace_create(args: &[String]) -> Result<Command, CommandError> {
    let name = require_arg(args, 0, "name")?.to_owned();
    let rest = &args[1..];

    let keyspace_type = find_opt(rest, "TYPE")
        .ok_or_else(|| CommandError::BadArg {
            message: "KEYSPACE_CREATE requires TYPE".into(),
        })?
        .to_owned();

    let algorithm = find_opt(rest, "ALGORITHM").map(|s| s.to_owned());

    let rotation_days = find_opt(rest, "ROTATION_DAYS")
        .map(|s| {
            s.parse::<u32>().map_err(|e| CommandError::BadArg {
                message: format!("invalid ROTATION_DAYS: {e}"),
            })
        })
        .transpose()?;

    let drain_days = find_opt(rest, "DRAIN_DAYS")
        .map(|s| {
            s.parse::<u32>().map_err(|e| CommandError::BadArg {
                message: format!("invalid DRAIN_DAYS: {e}"),
            })
        })
        .transpose()?;

    let default_ttl_secs = find_opt(rest, "TTL")
        .map(|s| {
            s.parse::<u64>().map_err(|e| CommandError::BadArg {
                message: format!("invalid TTL: {e}"),
            })
        })
        .transpose()?;

    Ok(Command::KeyspaceCreate {
        name,
        keyspace_type,
        algorithm,
        rotation_days,
        drain_days,
        default_ttl_secs,
    })
}

// AUTH <token>
fn parse_auth(args: &[String]) -> Result<Command, CommandError> {
    let token = require_arg(args, 0, "token")?.to_owned();
    Ok(Command::Auth { token })
}

// SUBSCRIBE <channel>
fn parse_subscribe(args: &[String]) -> Result<Command, CommandError> {
    let channel = require_arg(args, 0, "channel")?.to_owned();
    Ok(Command::Subscribe { channel })
}

// PIPELINE ... END — accumulate commands between PIPELINE and END
fn parse_pipeline(all_strings: &[String]) -> Result<Command, CommandError> {
    // all_strings[0] is "PIPELINE", find "END"
    let end_idx = all_strings
        .iter()
        .position(|s| s.eq_ignore_ascii_case("END"))
        .ok_or_else(|| CommandError::BadArg {
            message: "PIPELINE without END".into(),
        })?;

    // Between PIPELINE and END, split on command boundaries.
    // Each sub-command is delimited by known verbs.
    let inner = &all_strings[1..end_idx];
    if inner.is_empty() {
        return Ok(Command::Pipeline(vec![]));
    }

    // Re-parse each sub-command by finding verb boundaries
    let verbs = [
        "ISSUE",
        "VERIFY",
        "REVOKE",
        "REFRESH",
        "UPDATE",
        "INSPECT",
        "ROTATE",
        "JWKS",
        "KEYSTATE",
        "HEALTH",
        "KEYS",
        "SUSPEND",
        "UNSUSPEND",
        "SCHEMA",
        "CONFIG",
        "PASSWORD",
        "SUBSCRIBE",
        "KEYSPACE_CREATE",
        "AUTH",
    ];

    let mut commands = Vec::new();
    let mut start = 0;

    for i in 1..=inner.len() {
        let is_boundary =
            i == inner.len() || verbs.contains(&inner[i].to_ascii_uppercase().as_str());
        if is_boundary {
            let slice = &inner[start..i];
            if !slice.is_empty() {
                let frame = Resp3Frame::Array(
                    slice
                        .iter()
                        .map(|s| Resp3Frame::BulkString(s.as_bytes().to_vec()))
                        .collect(),
                );
                commands.push(parse_command(frame)?);
            }
            start = i;
        }
    }

    Ok(Command::Pipeline(commands))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bs(s: &str) -> Resp3Frame {
        Resp3Frame::BulkString(s.as_bytes().to_vec())
    }

    fn cmd_array(parts: &[&str]) -> Resp3Frame {
        Resp3Frame::Array(parts.iter().map(|s| bs(s)).collect())
    }

    #[test]
    fn parse_health() {
        let frame = cmd_array(&["HEALTH"]);
        let cmd = parse_command(frame).unwrap();
        assert!(matches!(cmd, Command::Health { keyspace: None }));
    }

    #[test]
    fn parse_issue_jwt() {
        let frame = cmd_array(&["ISSUE", "auth", "CLAIMS", r#"{"sub":"u1"}"#]);
        let cmd = parse_command(frame).unwrap();
        match cmd {
            Command::Issue {
                keyspace, claims, ..
            } => {
                assert_eq!(keyspace, "auth");
                assert!(claims.is_some());
            }
            _ => panic!("expected Issue"),
        }
    }

    #[test]
    fn parse_verify() {
        let frame = cmd_array(&["VERIFY", "keys", "sk_abc"]);
        let cmd = parse_command(frame).unwrap();
        match cmd {
            Command::Verify {
                keyspace,
                token,
                check_revoked,
                ..
            } => {
                assert_eq!(keyspace, "keys");
                assert_eq!(token, "sk_abc");
                assert!(!check_revoked);
            }
            _ => panic!("expected Verify"),
        }
    }

    #[test]
    fn parse_revoke_family() {
        let frame = cmd_array(&["REVOKE", "sessions", "FAMILY", "fam-123"]);
        let cmd = parse_command(frame).unwrap();
        match cmd {
            Command::Revoke {
                keyspace, target, ..
            } => {
                assert_eq!(keyspace, "sessions");
                assert!(matches!(target, RevokeTarget::Family(ref id) if id == "fam-123"));
            }
            _ => panic!("expected Revoke"),
        }
    }

    #[test]
    fn parse_unknown_command() {
        let frame = cmd_array(&["BOGUS", "arg"]);
        let err = parse_command(frame).unwrap_err();
        assert!(matches!(err, CommandError::BadArg { .. }));
    }

    /// Round-trip: Command → to_wire_args → parse_command → assert match.
    /// Validates that all PASSWORD variants serialize and parse symmetrically,
    /// proving the remote auth NOTFOUND issue is not in this crate.
    fn roundtrip(cmd: &Command) -> Command {
        let wire = cmd.to_wire_args();
        let frame = Resp3Frame::Array(
            wire.iter()
                .map(|s| Resp3Frame::BulkString(s.as_bytes().to_vec()))
                .collect(),
        );
        parse_command(frame).unwrap()
    }

    #[test]
    fn roundtrip_password_set() {
        let cmd = Command::PasswordSet {
            keyspace: "default_passwords".into(),
            user_id: "alice".into(),
            plaintext: "s3cret".into(),
            metadata: None,
        };
        match roundtrip(&cmd) {
            Command::PasswordSet {
                keyspace,
                user_id,
                plaintext,
                metadata,
            } => {
                assert_eq!(keyspace, "default_passwords");
                assert_eq!(user_id, "alice");
                assert_eq!(plaintext, "s3cret");
                assert!(metadata.is_none());
            }
            other => panic!("expected PasswordSet, got {other:?}"),
        }
    }

    #[test]
    fn roundtrip_password_set_with_meta() {
        let meta = serde_json::json!({"role": "admin"});
        let cmd = Command::PasswordSet {
            keyspace: "pw".into(),
            user_id: "bob".into(),
            plaintext: "pass".into(),
            metadata: Some(meta.clone()),
        };
        match roundtrip(&cmd) {
            Command::PasswordSet {
                keyspace,
                user_id,
                metadata,
                ..
            } => {
                assert_eq!(keyspace, "pw");
                assert_eq!(user_id, "bob");
                assert_eq!(metadata.unwrap(), meta);
            }
            other => panic!("expected PasswordSet, got {other:?}"),
        }
    }

    #[test]
    fn roundtrip_password_verify() {
        let cmd = Command::PasswordVerify {
            keyspace: "default_passwords".into(),
            user_id: "alice".into(),
            plaintext: "s3cret".into(),
        };
        match roundtrip(&cmd) {
            Command::PasswordVerify {
                keyspace,
                user_id,
                plaintext,
            } => {
                assert_eq!(keyspace, "default_passwords");
                assert_eq!(user_id, "alice");
                assert_eq!(plaintext, "s3cret");
            }
            other => panic!("expected PasswordVerify, got {other:?}"),
        }
    }

    #[test]
    fn roundtrip_password_change() {
        let cmd = Command::PasswordChange {
            keyspace: "pw".into(),
            user_id: "alice".into(),
            old_plaintext: "old".into(),
            new_plaintext: "new".into(),
        };
        match roundtrip(&cmd) {
            Command::PasswordChange {
                keyspace,
                user_id,
                old_plaintext,
                new_plaintext,
            } => {
                assert_eq!(keyspace, "pw");
                assert_eq!(user_id, "alice");
                assert_eq!(old_plaintext, "old");
                assert_eq!(new_plaintext, "new");
            }
            other => panic!("expected PasswordChange, got {other:?}"),
        }
    }

    #[test]
    fn roundtrip_password_reset() {
        let cmd = Command::PasswordReset {
            keyspace: "pw".into(),
            user_id: "alice".into(),
            new_plaintext: "reset123".into(),
        };
        match roundtrip(&cmd) {
            Command::PasswordReset {
                keyspace,
                user_id,
                new_plaintext,
            } => {
                assert_eq!(keyspace, "pw");
                assert_eq!(user_id, "alice");
                assert_eq!(new_plaintext, "reset123");
            }
            other => panic!("expected PasswordReset, got {other:?}"),
        }
    }

    #[test]
    fn roundtrip_password_import() {
        let cmd = Command::PasswordImport {
            keyspace: "pw".into(),
            user_id: "alice".into(),
            hash: "$argon2id$v=19$m=65536,t=3,p=4$abc$def".into(),
            metadata: None,
        };
        match roundtrip(&cmd) {
            Command::PasswordImport {
                keyspace,
                user_id,
                hash,
                metadata,
            } => {
                assert_eq!(keyspace, "pw");
                assert_eq!(user_id, "alice");
                assert_eq!(hash, "$argon2id$v=19$m=65536,t=3,p=4$abc$def");
                assert!(metadata.is_none());
            }
            other => panic!("expected PasswordImport, got {other:?}"),
        }
    }
}
