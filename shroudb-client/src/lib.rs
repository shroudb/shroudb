//! `shroudb-client` — typed Rust client library for ShrouDB.
//!
//! Provides a high-level async API for interacting with a ShrouDB server over TCP.
//! The RESP3 protocol is handled internally — callers never deal with raw frames.
//!
//! # Example
//!
//! ```no_run
//! use shroudb_client::ShrouDBClient;
//!
//! # async fn example() -> Result<(), shroudb_client::ClientError> {
//! let mut client = ShrouDBClient::connect("127.0.0.1:6399").await?;
//!
//! // Create a namespace
//! client.namespace_create("myapp.users").await?;
//!
//! // Store a value
//! let version = client.put("myapp.users", b"user:1", b"alice").await?;
//! println!("stored at version {version}");
//!
//! // Retrieve it
//! let entry = client.get("myapp.users", b"user:1").await?;
//! println!("value: {}", String::from_utf8_lossy(&entry.value));
//! # Ok(())
//! # }
//! ```

pub mod connection;
pub mod error;
pub mod response;

pub use error::ClientError;
pub use response::Response;

use connection::Connection;

/// Parsed components of a ShrouDB connection URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionConfig {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub auth_token: Option<String>,
}

/// Parse a ShrouDB connection URI.
///
/// Format: `shroudb://[token@]host[:port]`
///         `shroudb+tls://[token@]host[:port]`
pub fn parse_uri(uri: &str) -> Result<ConnectionConfig, ClientError> {
    let (tls, rest) = if let Some(rest) = uri.strip_prefix("shroudb+tls://") {
        (true, rest)
    } else if let Some(rest) = uri.strip_prefix("shroudb://") {
        (false, rest)
    } else {
        return Err(ClientError::Protocol(format!("invalid URI scheme: {uri}")));
    };

    let (auth_token, hostport) = if let Some(at_pos) = rest.find('@') {
        (Some(rest[..at_pos].to_string()), &rest[at_pos + 1..])
    } else {
        (None, rest)
    };

    // Strip trailing path if present
    let hostport = hostport.split('/').next().unwrap_or(hostport);

    let (host, port) = if let Some(colon_pos) = hostport.rfind(':') {
        let port_str = &hostport[colon_pos + 1..];
        match port_str.parse::<u16>() {
            Ok(p) => (hostport[..colon_pos].to_string(), p),
            Err(_) => (hostport.to_string(), 6399),
        }
    } else {
        (hostport.to_string(), 6399)
    };

    Ok(ConnectionConfig {
        host,
        port,
        tls,
        auth_token,
    })
}

/// A versioned entry returned by GET.
#[derive(Debug, Clone)]
pub struct GetResult {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub version: u64,
    pub metadata: Option<serde_json::Value>,
}

/// A version info entry returned by VERSIONS.
#[derive(Debug, Clone)]
pub struct VersionEntry {
    pub version: u64,
    pub state: String,
    pub updated_at: u64,
    pub actor: String,
}

/// Namespace info returned by NAMESPACE INFO.
#[derive(Debug, Clone)]
pub struct NamespaceInfo {
    pub name: String,
    pub key_count: u64,
    pub created_at: u64,
}

/// A page of results with optional cursor for pagination.
#[derive(Debug, Clone)]
pub struct PageResult {
    pub items: Vec<String>,
    pub cursor: Option<String>,
}

/// A client for interacting with a ShrouDB server.
pub struct ShrouDBClient {
    connection: Connection,
}

impl ShrouDBClient {
    /// Connect to a ShrouDB server at the given address (e.g. `"127.0.0.1:6399"`).
    pub async fn connect(addr: &str) -> Result<Self, ClientError> {
        let connection = Connection::connect(addr).await?;
        Ok(Self { connection })
    }

    /// Connect over TLS using the system's native root certificate store.
    pub async fn connect_tls(addr: &str) -> Result<Self, ClientError> {
        let connection = Connection::connect_tls(addr).await?;
        Ok(Self { connection })
    }

    /// Connect using a URI string.
    ///
    /// Format: `shroudb://[token@]host[:port]`
    ///         `shroudb+tls://[token@]host[:port]`
    pub async fn from_uri(uri: &str) -> Result<Self, ClientError> {
        let config = parse_uri(uri)?;
        let addr = format!("{}:{}", config.host, config.port);
        let mut client = if config.tls {
            Self::connect_tls(&addr).await?
        } else {
            Self::connect(&addr).await?
        };
        if let Some(token) = &config.auth_token {
            client.auth(token).await?;
        }
        Ok(client)
    }

    // ── Connection ──────��────────────────────────────────────────────

    /// Authenticate with a token.
    pub async fn auth(&mut self, token: &str) -> Result<(), ClientError> {
        let resp = self.connection.send_command_strs(&["AUTH", token]).await?;
        check_ok(&resp)
    }

    /// Ping the server.
    pub async fn ping(&mut self) -> Result<(), ClientError> {
        let resp = self.connection.send_command_strs(&["PING"]).await?;
        check_ok(&resp)
    }

    // ── Data operations ──────────────────────────────────────────────

    /// Store a value. Returns the new version number.
    pub async fn put(&mut self, ns: &str, key: &[u8], value: &[u8]) -> Result<u64, ClientError> {
        let key_str = String::from_utf8_lossy(key);
        let val_str = String::from_utf8_lossy(value);
        let resp = self
            .connection
            .send_command_strs(&["PUT", ns, &key_str, &val_str])
            .await?;
        check_ok(&resp)?;
        resp.get_int_field("version")
            .map(|v| v as u64)
            .ok_or_else(|| ClientError::ResponseFormat("missing version field".into()))
    }

    /// Store a value with metadata. Returns the new version number.
    pub async fn put_with_metadata(
        &mut self,
        ns: &str,
        key: &[u8],
        value: &[u8],
        metadata: serde_json::Value,
    ) -> Result<u64, ClientError> {
        let key_str = String::from_utf8_lossy(key);
        let val_str = String::from_utf8_lossy(value);
        let meta_str = serde_json::to_string(&metadata)
            .map_err(|e| ClientError::Serialization(e.to_string()))?;
        let resp = self
            .connection
            .send_command_strs(&["PUT", ns, &key_str, "VALUE", &val_str, "META", &meta_str])
            .await?;
        check_ok(&resp)?;
        resp.get_int_field("version")
            .map(|v| v as u64)
            .ok_or_else(|| ClientError::ResponseFormat("missing version field".into()))
    }

    /// Retrieve a value.
    pub async fn get(&mut self, ns: &str, key: &[u8]) -> Result<GetResult, ClientError> {
        let key_str = String::from_utf8_lossy(key);
        let resp = self
            .connection
            .send_command_strs(&["GET", ns, &key_str])
            .await?;
        check_ok(&resp)?;
        parse_get_result(&resp)
    }

    /// Retrieve a specific version.
    pub async fn get_version(
        &mut self,
        ns: &str,
        key: &[u8],
        version: u64,
    ) -> Result<GetResult, ClientError> {
        let key_str = String::from_utf8_lossy(key);
        let ver_str = version.to_string();
        let resp = self
            .connection
            .send_command_strs(&["GET", ns, &key_str, "VERSION", &ver_str])
            .await?;
        check_ok(&resp)?;
        parse_get_result(&resp)
    }

    /// Delete a key. Returns the tombstone version number.
    pub async fn delete(&mut self, ns: &str, key: &[u8]) -> Result<u64, ClientError> {
        let key_str = String::from_utf8_lossy(key);
        let resp = self
            .connection
            .send_command_strs(&["DELETE", ns, &key_str])
            .await?;
        check_ok(&resp)?;
        resp.get_int_field("version")
            .map(|v| v as u64)
            .ok_or_else(|| ClientError::ResponseFormat("missing version field".into()))
    }

    /// List active keys in a namespace.
    pub async fn list(&mut self, ns: &str) -> Result<PageResult, ClientError> {
        let resp = self.connection.send_command_strs(&["LIST", ns]).await?;
        check_ok(&resp)?;
        parse_key_list(&resp)
    }

    /// List with prefix filter.
    pub async fn list_prefix(&mut self, ns: &str, prefix: &str) -> Result<PageResult, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["LIST", ns, "PREFIX", prefix])
            .await?;
        check_ok(&resp)?;
        parse_key_list(&resp)
    }

    /// Get version history for a key.
    pub async fn versions(
        &mut self,
        ns: &str,
        key: &[u8],
    ) -> Result<Vec<VersionEntry>, ClientError> {
        let key_str = String::from_utf8_lossy(key);
        let resp = self
            .connection
            .send_command_strs(&["VERSIONS", ns, &key_str])
            .await?;
        check_ok(&resp)?;
        parse_versions(&resp)
    }

    // ── Namespace operations ─────────────────────────────────────────

    /// Create a namespace.
    pub async fn namespace_create(&mut self, name: &str) -> Result<(), ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["NAMESPACE", "CREATE", name])
            .await?;
        check_ok(&resp)
    }

    /// Drop a namespace.
    pub async fn namespace_drop(&mut self, name: &str, force: bool) -> Result<(), ClientError> {
        let args: Vec<&str> = if force {
            vec!["NAMESPACE", "DROP", name, "FORCE"]
        } else {
            vec!["NAMESPACE", "DROP", name]
        };
        let resp = self.connection.send_command_strs(&args).await?;
        check_ok(&resp)
    }

    /// List namespaces.
    pub async fn namespace_list(&mut self) -> Result<PageResult, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["NAMESPACE", "LIST"])
            .await?;
        check_ok(&resp)?;
        parse_namespace_list(&resp)
    }

    /// Get namespace info.
    pub async fn namespace_info(&mut self, name: &str) -> Result<NamespaceInfo, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["NAMESPACE", "INFO", name])
            .await?;
        check_ok(&resp)?;
        Ok(NamespaceInfo {
            name: resp.get_string_field("name").ok_or_else(|| {
                ClientError::ResponseFormat(
                    "missing 'name' field in NAMESPACE INFO response".into(),
                )
            })?,
            key_count: resp.get_int_field("key_count").ok_or_else(|| {
                ClientError::ResponseFormat(
                    "missing 'key_count' field in NAMESPACE INFO response".into(),
                )
            })? as u64,
            created_at: resp.get_int_field("created_at").ok_or_else(|| {
                ClientError::ResponseFormat(
                    "missing 'created_at' field in NAMESPACE INFO response".into(),
                )
            })? as u64,
        })
    }

    // ── Operational ──────────────────────────────────────────────────

    /// Health check.
    pub async fn health(&mut self) -> Result<(), ClientError> {
        let resp = self.connection.send_command_strs(&["HEALTH"]).await?;
        check_ok(&resp)
    }

    /// Get list of supported commands.
    pub async fn command_list(&mut self) -> Result<Vec<String>, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["COMMAND", "LIST"])
            .await?;
        check_ok(&resp)?;
        let commands_field = resp
            .get_field("commands")
            .ok_or_else(|| ClientError::ResponseFormat("missing commands field".into()))?;
        match commands_field {
            Response::Array(items) => Ok(items
                .iter()
                .filter_map(|r| r.as_str().map(String::from))
                .collect()),
            _ => Err(ClientError::ResponseFormat(
                "commands field is not an array".into(),
            )),
        }
    }

    /// Send a raw command.
    pub async fn raw_command(&mut self, args: &[&str]) -> Result<Response, ClientError> {
        self.connection.send_command_strs(args).await
    }
}

// ---------------------------------------------------------------------------
// Response parsing helpers
// ---------------------------------------------------------------------------

fn check_ok(resp: &Response) -> Result<(), ClientError> {
    match resp {
        Response::Error(e) => Err(ClientError::Server(e.clone())),
        Response::Null => Err(ClientError::ResponseFormat("unexpected null".into())),
        _ => Ok(()),
    }
}

fn parse_get_result(resp: &Response) -> Result<GetResult, ClientError> {
    let key = resp
        .get_string_field("key")
        .ok_or_else(|| ClientError::ResponseFormat("missing 'key' field in GET response".into()))?
        .into_bytes();
    let value = resp
        .get_string_field("value")
        .ok_or_else(|| ClientError::ResponseFormat("missing 'value' field in GET response".into()))?
        .into_bytes();
    let version = resp.get_int_field("version").ok_or_else(|| {
        ClientError::ResponseFormat("missing 'version' field in GET response".into())
    })? as u64;
    let metadata = resp.get_field("metadata").map(|v| v.to_json());

    Ok(GetResult {
        key,
        value,
        version,
        metadata,
    })
}

fn parse_key_list(resp: &Response) -> Result<PageResult, ClientError> {
    let keys_field = resp
        .get_field("keys")
        .ok_or_else(|| ClientError::ResponseFormat("missing keys field".into()))?;
    let items = match keys_field {
        Response::Array(items) => items
            .iter()
            .filter_map(|r| r.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    };
    let cursor = resp.get_string_field("cursor");
    Ok(PageResult { items, cursor })
}

fn parse_namespace_list(resp: &Response) -> Result<PageResult, ClientError> {
    let ns_field = resp
        .get_field("namespaces")
        .ok_or_else(|| ClientError::ResponseFormat("missing namespaces field".into()))?;
    let items = match ns_field {
        Response::Array(items) => items
            .iter()
            .filter_map(|r| r.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    };
    let cursor = resp.get_string_field("cursor");
    Ok(PageResult { items, cursor })
}

fn parse_versions(resp: &Response) -> Result<Vec<VersionEntry>, ClientError> {
    let versions_field = resp
        .get_field("versions")
        .ok_or_else(|| ClientError::ResponseFormat("missing versions field".into()))?;
    match versions_field {
        Response::Array(items) => {
            let mut entries = Vec::new();
            for (i, item) in items.iter().enumerate() {
                entries.push(VersionEntry {
                    version: item.get_int_field("version").ok_or_else(|| {
                        ClientError::ResponseFormat(format!("missing 'version' in versions[{i}]"))
                    })? as u64,
                    state: item.get_string_field("state").ok_or_else(|| {
                        ClientError::ResponseFormat(format!("missing 'state' in versions[{i}]"))
                    })?,
                    updated_at: item.get_int_field("updated_at").ok_or_else(|| {
                        ClientError::ResponseFormat(format!(
                            "missing 'updated_at' in versions[{i}]"
                        ))
                    })? as u64,
                    actor: item.get_string_field("actor").ok_or_else(|| {
                        ClientError::ResponseFormat(format!("missing 'actor' in versions[{i}]"))
                    })?,
                });
            }
            Ok(entries)
        }
        _ => Err(ClientError::ResponseFormat(
            "versions field is not an array".into(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_uri_plain() {
        let cfg = parse_uri("shroudb://localhost").unwrap();
        assert_eq!(cfg.host, "localhost");
        assert_eq!(cfg.port, 6399);
        assert!(!cfg.tls);
        assert!(cfg.auth_token.is_none());
    }

    #[test]
    fn parse_uri_with_port() {
        let cfg = parse_uri("shroudb://localhost:7000").unwrap();
        assert_eq!(cfg.port, 7000);
    }

    #[test]
    fn parse_uri_tls() {
        let cfg = parse_uri("shroudb+tls://prod.example.com").unwrap();
        assert!(cfg.tls);
        assert_eq!(cfg.host, "prod.example.com");
    }

    #[test]
    fn parse_uri_with_auth() {
        let cfg = parse_uri("shroudb://mytoken@localhost:6399").unwrap();
        assert_eq!(cfg.auth_token.as_deref(), Some("mytoken"));
    }

    #[test]
    fn parse_uri_full() {
        let cfg = parse_uri("shroudb+tls://tok@host:7000").unwrap();
        assert!(cfg.tls);
        assert_eq!(cfg.auth_token.as_deref(), Some("tok"));
        assert_eq!(cfg.host, "host");
        assert_eq!(cfg.port, 7000);
    }

    #[test]
    fn parse_uri_invalid_scheme() {
        assert!(parse_uri("redis://localhost").is_err());
    }
}
