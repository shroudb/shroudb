use crate::command::Command;
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
        "AUTH" => parse_auth(args),
        "PING" => Ok(Command::Ping),
        "PUT" => parse_put(args),
        "GET" => parse_get(args),
        "DELETE" => parse_delete(args),
        "LIST" => parse_list(args),
        "VERSIONS" => parse_versions(args),
        "NAMESPACE" => parse_namespace(args),
        "PIPELINE" => parse_pipeline(&strings),
        "SUBSCRIBE" => parse_subscribe(args),
        "UNSUBSCRIBE" => Ok(Command::Unsubscribe),
        "HEALTH" => Ok(Command::Health),
        "CONFIG" => parse_config(args),
        "COMMAND" => parse_command_sub(args),
        _ => Err(CommandError::BadArg {
            message: format!("unknown command: {verb}"),
        }),
    }
}

fn frame_to_string(frame: Resp3Frame) -> Result<String, CommandError> {
    match frame {
        Resp3Frame::BulkString(b) => String::from_utf8(b).map_err(|_| CommandError::BadArg {
            message: "invalid UTF-8 in argument".into(),
        }),
        Resp3Frame::SimpleString(s) => Ok(s),
        Resp3Frame::Integer(i) => Ok(i.to_string()),
        _ => Err(CommandError::BadArg {
            message: "expected string or bulk string argument".into(),
        }),
    }
}

fn require_args(args: &[String], min: usize, cmd: &str) -> Result<(), CommandError> {
    if args.len() < min {
        Err(CommandError::BadArg {
            message: format!("{cmd} requires at least {min} argument(s)"),
        })
    } else {
        Ok(())
    }
}

// ── AUTH <token> ─────────────────────────────────────────────────────

fn parse_auth(args: &[String]) -> Result<Command, CommandError> {
    require_args(args, 1, "AUTH")?;
    Ok(Command::Auth {
        token: args[0].clone(),
    })
}

// ── PUT <ns> <key> [VALUE <bytes>] [META <json>] ─────────────────────

fn parse_put(args: &[String]) -> Result<Command, CommandError> {
    require_args(args, 2, "PUT")?;
    let ns = args[0].clone();
    let key = args[1].as_bytes().to_vec();

    let mut value = Vec::new();
    let mut metadata = None;
    let mut i = 2;

    // Three-arg form: PUT ns key value (positional, no keywords)
    // Detect by checking if args[2] exists and is NOT followed by a keyword pair
    // Simple heuristic: if total args == 3, it's positional
    if args.len() == 3 {
        value = args[2].as_bytes().to_vec();
        return Ok(Command::Put {
            ns,
            key,
            value,
            metadata,
        });
    }

    while i < args.len() {
        let upper = args[i].to_ascii_uppercase();
        if upper == "VALUE" {
            i += 1;
            if i >= args.len() {
                return Err(CommandError::BadArg {
                    message: "VALUE requires a value".into(),
                });
            }
            value = args[i].as_bytes().to_vec();
        } else if upper == "META" {
            i += 1;
            if i >= args.len() {
                return Err(CommandError::BadArg {
                    message: "META requires a JSON value".into(),
                });
            }
            let json: serde_json::Value =
                serde_json::from_str(&args[i]).map_err(|e| CommandError::BadArg {
                    message: format!("invalid META JSON: {e}"),
                })?;
            metadata = Some(json);
        } else {
            return Err(CommandError::BadArg {
                message: format!("unexpected argument: {}", args[i]),
            });
        }
        i += 1;
    }

    Ok(Command::Put {
        ns,
        key,
        value,
        metadata,
    })
}

// ── GET <ns> <key> [VERSION <n>] [META] ──────────────────────────────

fn parse_get(args: &[String]) -> Result<Command, CommandError> {
    require_args(args, 2, "GET")?;
    let ns = args[0].clone();
    let key = args[1].as_bytes().to_vec();

    let mut version = None;
    let mut meta = false;
    let mut i = 2;

    while i < args.len() {
        match args[i].to_ascii_uppercase().as_str() {
            "VERSION" => {
                i += 1;
                if i >= args.len() {
                    return Err(CommandError::BadArg {
                        message: "VERSION requires a number".into(),
                    });
                }
                version = Some(args[i].parse::<u64>().map_err(|_| CommandError::BadArg {
                    message: "VERSION must be a positive integer".into(),
                })?);
            }
            "META" => {
                meta = true;
            }
            _ => {
                return Err(CommandError::BadArg {
                    message: format!("unexpected argument: {}", args[i]),
                });
            }
        }
        i += 1;
    }

    Ok(Command::Get {
        ns,
        key,
        version,
        meta,
    })
}

// ── DELETE <ns> <key> ────────────────────────────────────────────────

fn parse_delete(args: &[String]) -> Result<Command, CommandError> {
    require_args(args, 2, "DELETE")?;
    Ok(Command::Delete {
        ns: args[0].clone(),
        key: args[1].as_bytes().to_vec(),
    })
}

// ── LIST <ns> [PREFIX <prefix>] [CURSOR <cursor>] [LIMIT <n>] ───────

fn parse_list(args: &[String]) -> Result<Command, CommandError> {
    require_args(args, 1, "LIST")?;
    let ns = args[0].clone();

    let mut prefix = None;
    let mut cursor = None;
    let mut limit = None;
    let mut i = 1;

    while i < args.len() {
        match args[i].to_ascii_uppercase().as_str() {
            "PREFIX" => {
                i += 1;
                if i >= args.len() {
                    return Err(CommandError::BadArg {
                        message: "PREFIX requires a value".into(),
                    });
                }
                prefix = Some(args[i].as_bytes().to_vec());
            }
            "CURSOR" => {
                i += 1;
                if i >= args.len() {
                    return Err(CommandError::BadArg {
                        message: "CURSOR requires a value".into(),
                    });
                }
                cursor = Some(args[i].clone());
            }
            "LIMIT" => {
                i += 1;
                if i >= args.len() {
                    return Err(CommandError::BadArg {
                        message: "LIMIT requires a number".into(),
                    });
                }
                limit = Some(args[i].parse::<usize>().map_err(|_| CommandError::BadArg {
                    message: "LIMIT must be a positive integer".into(),
                })?);
            }
            _ => {
                return Err(CommandError::BadArg {
                    message: format!("unexpected argument: {}", args[i]),
                });
            }
        }
        i += 1;
    }

    Ok(Command::List {
        ns,
        prefix,
        cursor,
        limit,
    })
}

// ── VERSIONS <ns> <key> [LIMIT <n>] [FROM <version>] ────────────────

fn parse_versions(args: &[String]) -> Result<Command, CommandError> {
    require_args(args, 2, "VERSIONS")?;
    let ns = args[0].clone();
    let key = args[1].as_bytes().to_vec();

    let mut limit = None;
    let mut from = None;
    let mut i = 2;

    while i < args.len() {
        match args[i].to_ascii_uppercase().as_str() {
            "LIMIT" => {
                i += 1;
                if i >= args.len() {
                    return Err(CommandError::BadArg {
                        message: "LIMIT requires a number".into(),
                    });
                }
                limit = Some(args[i].parse::<usize>().map_err(|_| CommandError::BadArg {
                    message: "LIMIT must be a positive integer".into(),
                })?);
            }
            "FROM" => {
                i += 1;
                if i >= args.len() {
                    return Err(CommandError::BadArg {
                        message: "FROM requires a version number".into(),
                    });
                }
                from = Some(args[i].parse::<u64>().map_err(|_| CommandError::BadArg {
                    message: "FROM must be a positive integer".into(),
                })?);
            }
            _ => {
                return Err(CommandError::BadArg {
                    message: format!("unexpected argument: {}", args[i]),
                });
            }
        }
        i += 1;
    }

    Ok(Command::Versions {
        ns,
        key,
        limit,
        from,
    })
}

// ── NAMESPACE <subcommand> ... ───────────────────────────────────────

fn parse_namespace(args: &[String]) -> Result<Command, CommandError> {
    require_args(args, 1, "NAMESPACE")?;
    let sub = args[0].to_ascii_uppercase();
    let sub_args = &args[1..];

    match sub.as_str() {
        "CREATE" => {
            require_args(sub_args, 1, "NAMESPACE CREATE")?;
            let name = sub_args[0].clone();
            let mut schema = None;
            let mut max_versions = None;
            let mut tombstone_retention_secs = None;
            let mut i = 1;

            while i < sub_args.len() {
                match sub_args[i].to_ascii_uppercase().as_str() {
                    "SCHEMA" => {
                        i += 1;
                        if i >= sub_args.len() {
                            return Err(CommandError::BadArg {
                                message: "SCHEMA requires a JSON value".into(),
                            });
                        }
                        schema = Some(serde_json::from_str(&sub_args[i]).map_err(|e| {
                            CommandError::BadArg {
                                message: format!("invalid SCHEMA JSON: {e}"),
                            }
                        })?);
                    }
                    "MAX_VERSIONS" => {
                        i += 1;
                        if i >= sub_args.len() {
                            return Err(CommandError::BadArg {
                                message: "MAX_VERSIONS requires a number".into(),
                            });
                        }
                        max_versions =
                            Some(
                                sub_args[i]
                                    .parse::<u64>()
                                    .map_err(|_| CommandError::BadArg {
                                        message: "MAX_VERSIONS must be a positive integer".into(),
                                    })?,
                            );
                    }
                    "TOMBSTONE_RETENTION" => {
                        i += 1;
                        if i >= sub_args.len() {
                            return Err(CommandError::BadArg {
                                message: "TOMBSTONE_RETENTION requires a number (seconds)".into(),
                            });
                        }
                        tombstone_retention_secs = Some(sub_args[i].parse::<u64>().map_err(
                            |_| CommandError::BadArg {
                                message: "TOMBSTONE_RETENTION must be a positive integer".into(),
                            },
                        )?);
                    }
                    _ => {
                        return Err(CommandError::BadArg {
                            message: format!("unexpected argument: {}", sub_args[i]),
                        });
                    }
                }
                i += 1;
            }

            Ok(Command::NamespaceCreate {
                name,
                schema,
                max_versions,
                tombstone_retention_secs,
            })
        }
        "DROP" => {
            require_args(sub_args, 1, "NAMESPACE DROP")?;
            let name = sub_args[0].clone();
            let force = sub_args
                .get(1)
                .is_some_and(|s| s.eq_ignore_ascii_case("FORCE"));
            Ok(Command::NamespaceDrop { name, force })
        }
        "LIST" => {
            let mut cursor = None;
            let mut limit = None;
            let mut i = 0;
            while i < sub_args.len() {
                match sub_args[i].to_ascii_uppercase().as_str() {
                    "CURSOR" => {
                        i += 1;
                        cursor = sub_args.get(i).cloned();
                    }
                    "LIMIT" => {
                        i += 1;
                        limit = sub_args.get(i).and_then(|s| s.parse::<usize>().ok());
                    }
                    _ => {}
                }
                i += 1;
            }
            Ok(Command::NamespaceList { cursor, limit })
        }
        "INFO" => {
            require_args(sub_args, 1, "NAMESPACE INFO")?;
            Ok(Command::NamespaceInfo {
                name: sub_args[0].clone(),
            })
        }
        "ALTER" => {
            require_args(sub_args, 1, "NAMESPACE ALTER")?;
            let name = sub_args[0].clone();
            let mut schema = None;
            let mut max_versions = None;
            let mut tombstone_retention_secs = None;
            let mut i = 1;

            while i < sub_args.len() {
                match sub_args[i].to_ascii_uppercase().as_str() {
                    "SCHEMA" => {
                        i += 1;
                        schema = sub_args.get(i).and_then(|s| serde_json::from_str(s).ok());
                    }
                    "MAX_VERSIONS" => {
                        i += 1;
                        max_versions = sub_args.get(i).and_then(|s| s.parse().ok());
                    }
                    "TOMBSTONE_RETENTION" => {
                        i += 1;
                        tombstone_retention_secs = sub_args.get(i).and_then(|s| s.parse().ok());
                    }
                    _ => {}
                }
                i += 1;
            }

            Ok(Command::NamespaceAlter {
                name,
                schema,
                max_versions,
                tombstone_retention_secs,
            })
        }
        "VALIDATE" => {
            require_args(sub_args, 1, "NAMESPACE VALIDATE")?;
            Ok(Command::NamespaceValidate {
                name: sub_args[0].clone(),
            })
        }
        _ => Err(CommandError::BadArg {
            message: format!("unknown NAMESPACE subcommand: {sub}"),
        }),
    }
}

// ── PIPELINE <count> ─────────────────────────────────────────────────

fn parse_pipeline(strings: &[String]) -> Result<Command, CommandError> {
    require_args(&strings[1..], 1, "PIPELINE")?;
    let count = strings[1]
        .parse::<usize>()
        .map_err(|_| CommandError::BadArg {
            message: "PIPELINE requires a count".into(),
        })?;
    let _ = count; // count is informational; actual commands follow as separate frames
    Ok(Command::Pipeline(Vec::new()))
}

// ── SUBSCRIBE <ns> [KEY <key>] [EVENTS <PUT|DELETE|*>] ───────────────

fn parse_subscribe(args: &[String]) -> Result<Command, CommandError> {
    require_args(args, 1, "SUBSCRIBE")?;
    let ns = args[0].clone();
    let mut key = None;
    let mut events = Vec::new();
    let mut i = 1;

    while i < args.len() {
        match args[i].to_ascii_uppercase().as_str() {
            "KEY" => {
                i += 1;
                if i < args.len() {
                    key = Some(args[i].as_bytes().to_vec());
                }
            }
            "EVENTS" => {
                i += 1;
                while i < args.len() && !["KEY"].contains(&args[i].to_ascii_uppercase().as_str()) {
                    events.push(args[i].to_ascii_uppercase());
                    i += 1;
                }
                continue; // don't increment i again
            }
            _ => {}
        }
        i += 1;
    }

    Ok(Command::Subscribe { ns, key, events })
}

// ── CONFIG GET|SET ───────────────────────────────────────────────────

fn parse_config(args: &[String]) -> Result<Command, CommandError> {
    require_args(args, 1, "CONFIG")?;
    let sub = args[0].to_ascii_uppercase();
    match sub.as_str() {
        "GET" => {
            require_args(&args[1..], 1, "CONFIG GET")?;
            Ok(Command::ConfigGet {
                key: args[1].clone(),
            })
        }
        "SET" => {
            require_args(&args[1..], 2, "CONFIG SET")?;
            Ok(Command::ConfigSet {
                key: args[1].clone(),
                value: args[2].clone(),
            })
        }
        _ => Err(CommandError::BadArg {
            message: format!("unknown CONFIG subcommand: {sub}"),
        }),
    }
}

// ── COMMAND LIST ─────────────────────────────────────────────────────

fn parse_command_sub(args: &[String]) -> Result<Command, CommandError> {
    if args.is_empty() || args[0].eq_ignore_ascii_case("LIST") {
        Ok(Command::CommandList)
    } else {
        Err(CommandError::BadArg {
            message: format!("unknown COMMAND subcommand: {}", args[0]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(args: &[&str]) -> Resp3Frame {
        Resp3Frame::Array(
            args.iter()
                .map(|s| Resp3Frame::BulkString(s.as_bytes().to_vec()))
                .collect(),
        )
    }

    #[test]
    fn parse_ping() {
        let cmd = parse_command(frame(&["PING"])).unwrap();
        assert!(matches!(cmd, Command::Ping));
    }

    #[test]
    fn parse_auth() {
        let cmd = parse_command(frame(&["AUTH", "my-token"])).unwrap();
        match cmd {
            Command::Auth { token } => assert_eq!(token, "my-token"),
            _ => panic!("expected Auth"),
        }
    }

    #[test]
    fn parse_put_positional() {
        let cmd = parse_command(frame(&["PUT", "ns", "key", "value"])).unwrap();
        match cmd {
            Command::Put {
                ns,
                key,
                value,
                metadata,
            } => {
                assert_eq!(ns, "ns");
                assert_eq!(key, b"key");
                assert_eq!(value, b"value");
                assert!(metadata.is_none());
            }
            _ => panic!("expected Put"),
        }
    }

    #[test]
    fn parse_put_with_meta() {
        let cmd = parse_command(frame(&[
            "PUT",
            "ns",
            "key",
            "VALUE",
            "data",
            "META",
            r#"{"env":"prod"}"#,
        ]))
        .unwrap();
        match cmd {
            Command::Put { metadata, .. } => {
                assert!(metadata.is_some());
            }
            _ => panic!("expected Put"),
        }
    }

    #[test]
    fn parse_get_basic() {
        let cmd = parse_command(frame(&["GET", "ns", "key"])).unwrap();
        match cmd {
            Command::Get {
                ns,
                key,
                version,
                meta,
            } => {
                assert_eq!(ns, "ns");
                assert_eq!(key, b"key");
                assert!(version.is_none());
                assert!(!meta);
            }
            _ => panic!("expected Get"),
        }
    }

    #[test]
    fn parse_get_with_version_and_meta() {
        let cmd = parse_command(frame(&["GET", "ns", "key", "VERSION", "3", "META"])).unwrap();
        match cmd {
            Command::Get { version, meta, .. } => {
                assert_eq!(version, Some(3));
                assert!(meta);
            }
            _ => panic!("expected Get"),
        }
    }

    #[test]
    fn parse_delete() {
        let cmd = parse_command(frame(&["DELETE", "ns", "key"])).unwrap();
        assert!(matches!(cmd, Command::Delete { .. }));
    }

    #[test]
    fn parse_list_basic() {
        let cmd = parse_command(frame(&["LIST", "ns"])).unwrap();
        match cmd {
            Command::List {
                ns,
                prefix,
                cursor,
                limit,
            } => {
                assert_eq!(ns, "ns");
                assert!(prefix.is_none());
                assert!(cursor.is_none());
                assert!(limit.is_none());
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn parse_list_with_options() {
        let cmd = parse_command(frame(&["LIST", "ns", "PREFIX", "user:", "LIMIT", "50"])).unwrap();
        match cmd {
            Command::List { prefix, limit, .. } => {
                assert_eq!(prefix, Some(b"user:".to_vec()));
                assert_eq!(limit, Some(50));
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn parse_versions() {
        let cmd = parse_command(frame(&["VERSIONS", "ns", "key", "LIMIT", "5"])).unwrap();
        match cmd {
            Command::Versions {
                ns,
                key,
                limit,
                from,
            } => {
                assert_eq!(ns, "ns");
                assert_eq!(key, b"key");
                assert_eq!(limit, Some(5));
                assert!(from.is_none());
            }
            _ => panic!("expected Versions"),
        }
    }

    #[test]
    fn parse_namespace_create() {
        let cmd = parse_command(frame(&["NAMESPACE", "CREATE", "users"])).unwrap();
        match cmd {
            Command::NamespaceCreate { name, .. } => assert_eq!(name, "users"),
            _ => panic!("expected NamespaceCreate"),
        }
    }

    #[test]
    fn parse_namespace_drop_force() {
        let cmd = parse_command(frame(&["NAMESPACE", "DROP", "temp", "FORCE"])).unwrap();
        match cmd {
            Command::NamespaceDrop { name, force } => {
                assert_eq!(name, "temp");
                assert!(force);
            }
            _ => panic!("expected NamespaceDrop"),
        }
    }

    #[test]
    fn parse_namespace_info() {
        let cmd = parse_command(frame(&["NAMESPACE", "INFO", "users"])).unwrap();
        assert!(matches!(cmd, Command::NamespaceInfo { name } if name == "users"));
    }

    #[test]
    fn parse_config_get() {
        let cmd = parse_command(frame(&["CONFIG", "GET", "max_connections"])).unwrap();
        match cmd {
            Command::ConfigGet { key } => assert_eq!(key, "max_connections"),
            _ => panic!("expected ConfigGet"),
        }
    }

    #[test]
    fn parse_config_set() {
        let cmd = parse_command(frame(&["CONFIG", "SET", "max_connections", "100"])).unwrap();
        match cmd {
            Command::ConfigSet { key, value } => {
                assert_eq!(key, "max_connections");
                assert_eq!(value, "100");
            }
            _ => panic!("expected ConfigSet"),
        }
    }

    #[test]
    fn parse_command_list() {
        let cmd = parse_command(frame(&["COMMAND", "LIST"])).unwrap();
        assert!(matches!(cmd, Command::CommandList));
    }

    #[test]
    fn parse_health() {
        let cmd = parse_command(frame(&["HEALTH"])).unwrap();
        assert!(matches!(cmd, Command::Health));
    }

    #[test]
    fn parse_subscribe() {
        let cmd = parse_command(frame(&["SUBSCRIBE", "ns", "KEY", "mykey"])).unwrap();
        match cmd {
            Command::Subscribe { ns, key, events } => {
                assert_eq!(ns, "ns");
                assert_eq!(key, Some(b"mykey".to_vec()));
                assert!(events.is_empty());
            }
            _ => panic!("expected Subscribe"),
        }
    }

    #[test]
    fn unknown_command_errors() {
        let err = parse_command(frame(&["FOOBAR"])).unwrap_err();
        assert!(matches!(err, CommandError::BadArg { .. }));
    }

    #[test]
    fn empty_command_errors() {
        let err = parse_command(Resp3Frame::Array(vec![])).unwrap_err();
        assert!(matches!(err, CommandError::BadArg { .. }));
    }
}
