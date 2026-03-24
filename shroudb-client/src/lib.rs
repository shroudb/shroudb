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
//! // Issue an API key
//! let result = client.issue("my-keyspace").execute().await?;
//! let api_key = result.api_key.as_ref().unwrap();
//! println!("API key: {api_key}");
//!
//! // Verify it
//! let verify = client.verify("my-keyspace", api_key).await?;
//! println!("Valid: {}", verify.is_ok());
//!
//! // Health check
//! let health = client.health().await?;
//! println!("State: {}", health.state);
//! # Ok(())
//! # }
//! ```

pub mod builder;
pub mod connection;
pub mod error;
pub mod response;

pub use error::ClientError;
pub use response::{
    HealthResult, IssueResult, KeyInfo, KeyStateResult, OkResult, Response, VerifyResult,
};

use builder::IssueBuilder;
use connection::Connection;

/// Parsed components of a ShrouDB connection URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionConfig {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub auth_token: Option<String>,
    pub keyspace: Option<String>,
}

/// Parse a ShrouDB connection URI.
///
/// Format: `shroudb://[token@]host[:port][/keyspace]`
///         `shroudb+tls://[token@]host[:port][/keyspace]`
///
/// # Examples
///
/// ```
/// use shroudb_client::parse_uri;
///
/// let cfg = parse_uri("shroudb://localhost").unwrap();
/// assert_eq!(cfg.host, "localhost");
/// assert_eq!(cfg.port, 6399);
/// assert!(!cfg.tls);
///
/// let cfg = parse_uri("shroudb+tls://mytoken@prod.example.com:7000/sessions").unwrap();
/// assert!(cfg.tls);
/// assert_eq!(cfg.auth_token.as_deref(), Some("mytoken"));
/// assert_eq!(cfg.host, "prod.example.com");
/// assert_eq!(cfg.port, 7000);
/// assert_eq!(cfg.keyspace.as_deref(), Some("sessions"));
/// ```
pub fn parse_uri(uri: &str) -> Result<ConnectionConfig, ClientError> {
    let (tls, rest) = if let Some(rest) = uri.strip_prefix("shroudb+tls://") {
        (true, rest)
    } else if let Some(rest) = uri.strip_prefix("shroudb://") {
        (false, rest)
    } else {
        return Err(ClientError::Protocol(format!("invalid URI scheme: {uri}")));
    };

    // Parse token@host:port/keyspace
    let (auth_token, hostport_path) = if let Some(at_pos) = rest.find('@') {
        (Some(rest[..at_pos].to_string()), &rest[at_pos + 1..])
    } else {
        (None, rest)
    };

    let (hostport, keyspace) = if let Some(slash_pos) = hostport_path.find('/') {
        let ks = &hostport_path[slash_pos + 1..];
        let ks = if ks.is_empty() {
            None
        } else {
            Some(ks.to_string())
        };
        (&hostport_path[..slash_pos], ks)
    } else {
        (hostport_path, None)
    };

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
        keyspace,
    })
}

/// A client for interacting with a ShrouDB server.
pub struct ShrouDBClient {
    pub(crate) connection: Connection,
}

impl ShrouDBClient {
    /// Connect to a ShrouDB server at the given address (e.g. `"127.0.0.1:6399"`).
    pub async fn connect(addr: &str) -> Result<Self, ClientError> {
        let connection = Connection::connect(addr).await?;
        Ok(Self { connection })
    }

    /// Connect to a ShrouDB server over TLS at the given address (e.g. `"127.0.0.1:6399"`).
    ///
    /// Uses the system's native root certificate store for server verification.
    pub async fn connect_tls(addr: &str) -> Result<Self, ClientError> {
        let connection = Connection::connect_tls(addr).await?;
        Ok(Self { connection })
    }

    /// Connect using a URI string.
    ///
    /// Format: `shroudb://[token@]host[:port][/keyspace]`
    ///         `shroudb+tls://[token@]host[:port][/keyspace]`
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

    /// Authenticate the connection with a bearer token.
    pub async fn auth(&mut self, token: &str) -> Result<(), ClientError> {
        let resp = self.connection.send_command_strs(&["AUTH", token]).await?;
        check_ok_status(resp)
    }

    /// Start building an ISSUE command for the given keyspace.
    ///
    /// Use the returned [`IssueBuilder`] to set optional parameters, then call `.execute()`.
    pub fn issue(&mut self, keyspace: &str) -> IssueBuilder<'_> {
        IssueBuilder::new(self, keyspace)
    }

    /// Verify a credential (API key, JWT, or refresh token) in the given keyspace.
    pub async fn verify(
        &mut self,
        keyspace: &str,
        token: &str,
    ) -> Result<VerifyResult, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["VERIFY", keyspace, token])
            .await?;
        VerifyResult::from_response(resp)
    }

    /// Verify a credential with a payload (for HMAC keyspaces).
    pub async fn verify_with_payload(
        &mut self,
        keyspace: &str,
        token: &str,
        payload: &str,
    ) -> Result<VerifyResult, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["VERIFY", keyspace, token, "PAYLOAD", payload])
            .await?;
        VerifyResult::from_response(resp)
    }

    /// Revoke a credential by credential ID.
    pub async fn revoke(&mut self, keyspace: &str, credential_id: &str) -> Result<(), ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["REVOKE", keyspace, credential_id])
            .await?;
        check_ok_status(resp)
    }

    /// Revoke all credentials in a refresh token family.
    pub async fn revoke_family(
        &mut self,
        keyspace: &str,
        family_id: &str,
    ) -> Result<(), ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["REVOKE", keyspace, "FAMILY", family_id])
            .await?;
        check_ok_status(resp)
    }

    /// Refresh a token, consuming the old one and returning a new credential.
    pub async fn refresh(
        &mut self,
        keyspace: &str,
        token: &str,
    ) -> Result<IssueResult, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["REFRESH", keyspace, token])
            .await?;
        IssueResult::from_response(resp)
    }

    /// Update metadata on an existing credential.
    pub async fn update(
        &mut self,
        keyspace: &str,
        credential_id: &str,
        metadata: serde_json::Value,
    ) -> Result<(), ClientError> {
        let meta_str = serde_json::to_string(&metadata).unwrap();
        let resp = self
            .connection
            .send_command_strs(&["UPDATE", keyspace, credential_id, "META", &meta_str])
            .await?;
        check_ok_status(resp)
    }

    /// Inspect a credential by ID, returning all stored fields.
    pub async fn inspect(
        &mut self,
        keyspace: &str,
        credential_id: &str,
    ) -> Result<OkResult, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["INSPECT", keyspace, credential_id])
            .await?;
        OkResult::from_response(resp)
    }

    /// Trigger key rotation for the keyspace.
    pub async fn rotate(&mut self, keyspace: &str) -> Result<OkResult, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["ROTATE", keyspace])
            .await?;
        OkResult::from_response(resp)
    }

    /// Force key rotation regardless of rotation age.
    pub async fn rotate_force(&mut self, keyspace: &str) -> Result<OkResult, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["ROTATE", keyspace, "FORCE"])
            .await?;
        OkResult::from_response(resp)
    }

    /// Dry-run key rotation (preview without mutating).
    pub async fn rotate_dryrun(&mut self, keyspace: &str) -> Result<OkResult, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["ROTATE", keyspace, "FORCE", "DRYRUN"])
            .await?;
        OkResult::from_response(resp)
    }

    /// Get the JSON Web Key Set for a JWT keyspace.
    pub async fn jwks(&mut self, keyspace: &str) -> Result<OkResult, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["JWKS", keyspace])
            .await?;
        OkResult::from_response(resp)
    }

    /// Get the key ring state for a keyspace.
    pub async fn keystate(&mut self, keyspace: &str) -> Result<KeyStateResult, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["KEYSTATE", keyspace])
            .await?;
        KeyStateResult::from_response(resp)
    }

    /// Check server health.
    pub async fn health(&mut self) -> Result<HealthResult, ClientError> {
        let resp = self.connection.send_command_strs(&["HEALTH"]).await?;
        HealthResult::from_response(resp)
    }

    /// Check health for a specific keyspace.
    pub async fn health_keyspace(&mut self, keyspace: &str) -> Result<OkResult, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["HEALTH", keyspace])
            .await?;
        OkResult::from_response(resp)
    }

    /// Suspend a credential (temporarily disable verification).
    pub async fn suspend(
        &mut self,
        keyspace: &str,
        credential_id: &str,
    ) -> Result<(), ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["SUSPEND", keyspace, credential_id])
            .await?;
        check_ok_status(resp)
    }

    /// Unsuspend a previously suspended credential.
    pub async fn unsuspend(
        &mut self,
        keyspace: &str,
        credential_id: &str,
    ) -> Result<(), ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["UNSUSPEND", keyspace, credential_id])
            .await?;
        check_ok_status(resp)
    }

    /// Get the metadata schema for a keyspace.
    pub async fn schema(&mut self, keyspace: &str) -> Result<OkResult, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["SCHEMA", keyspace])
            .await?;
        OkResult::from_response(resp)
    }

    /// List credential IDs in a keyspace.
    pub async fn keys(&mut self, keyspace: &str) -> Result<OkResult, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["KEYS", keyspace])
            .await?;
        OkResult::from_response(resp)
    }

    /// Set a password for a user in a password keyspace.
    pub async fn password_set(
        &mut self,
        keyspace: &str,
        user_id: &str,
        password: &str,
    ) -> Result<OkResult, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["PASSWORD", "SET", keyspace, user_id, password])
            .await?;
        OkResult::from_response(resp)
    }

    /// Verify a password for a user in a password keyspace.
    pub async fn password_verify(
        &mut self,
        keyspace: &str,
        user_id: &str,
        password: &str,
    ) -> Result<VerifyResult, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["PASSWORD", "VERIFY", keyspace, user_id, password])
            .await?;
        VerifyResult::from_response(resp)
    }

    /// Change a password for a user in a password keyspace.
    pub async fn password_change(
        &mut self,
        keyspace: &str,
        user_id: &str,
        old_password: &str,
        new_password: &str,
    ) -> Result<OkResult, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&[
                "PASSWORD",
                "CHANGE",
                keyspace,
                user_id,
                old_password,
                new_password,
            ])
            .await?;
        OkResult::from_response(resp)
    }

    /// Import a pre-hashed password for migration from another system.
    ///
    /// Accepts hashes in argon2id/argon2i/argon2d (PHC format), bcrypt
    /// (modular crypt format), or scrypt (PHC format). On subsequent
    /// `password_verify`, imported hashes are automatically rehashed to
    /// the keyspace's configured argon2id parameters.
    pub async fn password_import(
        &mut self,
        keyspace: &str,
        user_id: &str,
        hash: &str,
    ) -> Result<OkResult, ClientError> {
        let resp = self
            .connection
            .send_command_strs(&["PASSWORD", "IMPORT", keyspace, user_id, hash])
            .await?;
        OkResult::from_response(resp)
    }

    /// Send an arbitrary command and return the raw RESP3 response.
    pub async fn raw_command(&mut self, args: &[&str]) -> Result<Response, ClientError> {
        self.connection.send_command_strs(args).await
    }
}

/// Check that a response indicates success (not an error).
fn check_ok_status(resp: Response) -> Result<(), ClientError> {
    match &resp {
        Response::Error(e) => {
            if e.contains("DENIED") {
                Err(ClientError::AuthRequired)
            } else {
                Err(ClientError::Server(e.clone()))
            }
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_uri_plain_host() {
        let cfg = parse_uri("shroudb://localhost").unwrap();
        assert_eq!(cfg.host, "localhost");
        assert_eq!(cfg.port, 6399);
        assert!(!cfg.tls);
        assert!(cfg.auth_token.is_none());
        assert!(cfg.keyspace.is_none());
    }

    #[test]
    fn parse_uri_with_port() {
        let cfg = parse_uri("shroudb://localhost:7000").unwrap();
        assert_eq!(cfg.host, "localhost");
        assert_eq!(cfg.port, 7000);
    }

    #[test]
    fn parse_uri_tls() {
        let cfg = parse_uri("shroudb+tls://prod.example.com").unwrap();
        assert!(cfg.tls);
        assert_eq!(cfg.host, "prod.example.com");
        assert_eq!(cfg.port, 6399);
    }

    #[test]
    fn parse_uri_with_auth() {
        let cfg = parse_uri("shroudb://mytoken@localhost:6399").unwrap();
        assert_eq!(cfg.auth_token.as_deref(), Some("mytoken"));
        assert_eq!(cfg.host, "localhost");
        assert_eq!(cfg.port, 6399);
    }

    #[test]
    fn parse_uri_with_keyspace() {
        let cfg = parse_uri("shroudb://localhost/sessions").unwrap();
        assert_eq!(cfg.keyspace.as_deref(), Some("sessions"));
        assert_eq!(cfg.host, "localhost");
        assert_eq!(cfg.port, 6399);
    }

    #[test]
    fn parse_uri_full_form() {
        let cfg = parse_uri("shroudb+tls://tok@host:7000/keys").unwrap();
        assert!(cfg.tls);
        assert_eq!(cfg.auth_token.as_deref(), Some("tok"));
        assert_eq!(cfg.host, "host");
        assert_eq!(cfg.port, 7000);
        assert_eq!(cfg.keyspace.as_deref(), Some("keys"));
    }

    #[test]
    fn parse_uri_trailing_slash_no_keyspace() {
        let cfg = parse_uri("shroudb://localhost/").unwrap();
        assert!(cfg.keyspace.is_none());
    }

    #[test]
    fn parse_uri_invalid_scheme() {
        assert!(parse_uri("redis://localhost").is_err());
        assert!(parse_uri("http://localhost").is_err());
    }

    #[test]
    fn parse_uri_default_port_on_invalid_port() {
        let cfg = parse_uri("shroudb://localhost:notaport").unwrap();
        assert_eq!(cfg.port, 6399);
    }
}
