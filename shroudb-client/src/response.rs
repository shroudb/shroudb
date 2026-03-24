//! Parsed RESP3 response types and typed result structs.

use std::collections::HashMap;

use crate::error::ClientError;

// ---------------------------------------------------------------------------
// Raw RESP3 response
// ---------------------------------------------------------------------------

/// A parsed RESP3 response value.
#[derive(Debug, Clone)]
pub enum Response {
    /// Simple string or bulk string.
    String(String),
    /// Error response.
    Error(String),
    /// Integer response.
    Integer(i64),
    /// Null value.
    Null,
    /// Array of responses.
    Array(Vec<Response>),
    /// Map of key-value pairs.
    Map(Vec<(Response, Response)>),
}

impl Response {
    /// Return the string value, or `None` if this is not a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Response::String(s) => Some(s),
            _ => None,
        }
    }

    /// Return the integer value, or `None`.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Response::Integer(n) => Some(*n),
            _ => None,
        }
    }

    /// Return `true` if this is an error response.
    pub fn is_error(&self) -> bool {
        matches!(self, Response::Error(_))
    }

    /// Return `true` if this is null.
    pub fn is_null(&self) -> bool {
        matches!(self, Response::Null)
    }

    /// For display/debug: human-readable type name.
    pub fn type_name(&self) -> &'static str {
        match self {
            Response::String(_) => "String",
            Response::Error(_) => "Error",
            Response::Integer(_) => "Integer",
            Response::Null => "Null",
            Response::Array(_) => "Array",
            Response::Map(_) => "Map",
        }
    }

    /// Convert a response to a display string (for map keys, etc.).
    pub fn to_display_string(&self) -> String {
        match self {
            Response::String(s) => s.clone(),
            Response::Error(e) => format!("(error) {e}"),
            Response::Integer(n) => n.to_string(),
            Response::Null => "(nil)".to_string(),
            Response::Array(_) => "(array)".to_string(),
            Response::Map(_) => "(map)".to_string(),
        }
    }

    /// Convert to `serde_json::Value`.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Response::String(s) => serde_json::Value::String(s.clone()),
            Response::Error(e) => serde_json::json!({ "error": e }),
            Response::Integer(n) => serde_json::json!(n),
            Response::Null => serde_json::Value::Null,
            Response::Array(items) => {
                serde_json::Value::Array(items.iter().map(|r| r.to_json()).collect())
            }
            Response::Map(entries) => {
                let obj: serde_json::Map<String, serde_json::Value> = entries
                    .iter()
                    .map(|(k, v)| (k.to_display_string(), v.to_json()))
                    .collect();
                serde_json::Value::Object(obj)
            }
        }
    }

    /// Reconstruct the raw RESP3 wire format from a parsed Response.
    pub fn to_raw(&self) -> String {
        let mut buf = String::new();
        write_raw(self, &mut buf);
        buf
    }

    /// Print in human-readable format.
    pub fn print(&self, indent: usize) {
        let pad = "  ".repeat(indent);
        match self {
            Response::String(s) => println!("{pad}{s}"),
            Response::Error(e) => println!("{pad}(error) {e}"),
            Response::Integer(n) => println!("{pad}(integer) {n}"),
            Response::Null => println!("{pad}(nil)"),
            Response::Array(items) => {
                if items.is_empty() {
                    println!("{pad}(empty array)");
                } else {
                    for (i, item) in items.iter().enumerate() {
                        print!("{pad}{}. ", i + 1);
                        print_response_inline(item, indent + 1);
                    }
                }
            }
            Response::Map(entries) => {
                if entries.is_empty() {
                    println!("{pad}(empty map)");
                } else {
                    for (key, val) in entries {
                        let key_str = key.to_display_string();
                        match val {
                            Response::Map(_) | Response::Array(_) => {
                                println!("{pad}{key_str}:");
                                val.print(indent + 1);
                            }
                            _ => {
                                let val_str = response_to_inline_string(val);
                                println!("{pad}{key_str}: {val_str}");
                            }
                        }
                    }
                }
            }
        }
    }

    /// Look up a key in a map response, returning the value.
    fn get_field(&self, key: &str) -> Option<&Response> {
        match self {
            Response::Map(entries) => entries
                .iter()
                .find(|(k, _)| k.to_display_string() == key)
                .map(|(_, v)| v),
            _ => None,
        }
    }

    /// Get a string field from a map response.
    fn get_string_field(&self, key: &str) -> Option<String> {
        self.get_field(key)
            .and_then(|v| v.as_str().map(String::from))
    }

    /// Get an integer field from a map response.
    fn get_int_field(&self, key: &str) -> Option<i64> {
        self.get_field(key).and_then(|v| match v {
            Response::Integer(n) => Some(*n),
            Response::String(s) => s.parse().ok(),
            _ => None,
        })
    }
}

fn print_response_inline(resp: &Response, indent: usize) {
    match resp {
        Response::Map(_) | Response::Array(_) => {
            println!();
            resp.print(indent);
        }
        _ => {
            println!("{}", response_to_inline_string(resp));
        }
    }
}

fn response_to_inline_string(resp: &Response) -> String {
    match resp {
        Response::String(s) => s.clone(),
        Response::Error(e) => format!("(error) {e}"),
        Response::Integer(n) => format!("(integer) {n}"),
        Response::Null => "(nil)".to_string(),
        Response::Array(items) => format!("(array, {} items)", items.len()),
        Response::Map(entries) => format!("(map, {} entries)", entries.len()),
    }
}

fn write_raw(resp: &Response, buf: &mut String) {
    match resp {
        Response::String(s) => {
            buf.push_str(&format!("${}\r\n{s}\r\n", s.len()));
        }
        Response::Error(e) => {
            buf.push('-');
            buf.push_str(e);
            buf.push_str("\r\n");
        }
        Response::Integer(n) => {
            buf.push(':');
            buf.push_str(&n.to_string());
            buf.push_str("\r\n");
        }
        Response::Null => {
            buf.push_str("_\r\n");
        }
        Response::Array(items) => {
            buf.push_str(&format!("*{}\r\n", items.len()));
            for item in items {
                write_raw(item, buf);
            }
        }
        Response::Map(entries) => {
            buf.push_str(&format!("%{}\r\n", entries.len()));
            for (k, v) in entries {
                write_raw(k, buf);
                write_raw(v, buf);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Typed result structs
// ---------------------------------------------------------------------------

/// Result from an ISSUE or REFRESH command.
#[derive(Debug, Clone)]
pub struct IssueResult {
    /// The issued API key (for `api_key` keyspaces).
    pub api_key: Option<String>,
    /// The issued token (for `jwt` or `refresh_token` keyspaces).
    pub token: Option<String>,
    /// The credential ID.
    pub credential_id: Option<String>,
    /// The family ID (for `refresh_token` keyspaces).
    pub family_id: Option<String>,
    /// The HMAC signature (for `hmac` keyspaces).
    pub signature: Option<String>,
    /// The key ID used for signing.
    pub kid: Option<String>,
    /// Expiry timestamp in Unix seconds (for `jwt` keyspaces).
    pub expires_at: Option<i64>,
}

impl IssueResult {
    /// Parse an `IssueResult` from a RESP3 map response.
    pub fn from_response(resp: Response) -> Result<Self, ClientError> {
        if let Response::Error(e) = &resp {
            if e.contains("DENIED") {
                return Err(ClientError::AuthRequired);
            }
            return Err(ClientError::Server(e.clone()));
        }
        Ok(Self {
            api_key: resp.get_string_field("api_key"),
            token: resp.get_string_field("token"),
            credential_id: resp.get_string_field("credential_id"),
            family_id: resp.get_string_field("family_id"),
            signature: resp.get_string_field("signature"),
            kid: resp.get_string_field("kid"),
            expires_at: resp.get_int_field("expires_at"),
        })
    }
}

/// Result from a VERIFY or PASSWORD VERIFY command.
#[derive(Debug, Clone)]
pub struct VerifyResult {
    /// The credential ID of the verified credential.
    pub credential_id: Option<String>,
    /// Decoded JWT claims (for `jwt` keyspaces).
    pub claims: Option<serde_json::Value>,
    /// Credential metadata.
    pub metadata: Option<serde_json::Value>,
    /// Cache-until hint (Unix timestamp).
    pub cache_until: Option<i64>,
    /// Whether the credential is valid (for `password` keyspaces).
    pub valid: Option<bool>,
}

impl VerifyResult {
    /// Parse a `VerifyResult` from a RESP3 map response.
    pub fn from_response(resp: Response) -> Result<Self, ClientError> {
        if let Response::Error(e) = &resp {
            if e.contains("DENIED") {
                return Err(ClientError::AuthRequired);
            }
            return Err(ClientError::Server(e.clone()));
        }
        let claims = resp.get_field("claims").map(|v| v.to_json());
        let metadata = resp.get_field("metadata").map(|v| v.to_json());
        let valid = resp.get_string_field("valid").map(|v| v == "true");
        Ok(Self {
            credential_id: resp.get_string_field("credential_id"),
            claims,
            metadata,
            cache_until: resp.get_int_field("cache_until"),
            valid,
        })
    }

    /// Returns `true` if the verification succeeded (credential_id is present).
    pub fn is_ok(&self) -> bool {
        self.credential_id.is_some()
    }
}

/// Result from a HEALTH command.
#[derive(Debug, Clone)]
pub struct HealthResult {
    /// Server state (e.g. `"READY"`).
    pub state: String,
}

impl HealthResult {
    /// Parse a `HealthResult` from a RESP3 map response.
    pub fn from_response(resp: Response) -> Result<Self, ClientError> {
        if let Response::Error(e) = &resp {
            return Err(ClientError::Server(e.clone()));
        }
        let state = resp
            .get_string_field("state")
            .unwrap_or_else(|| "UNKNOWN".into());
        Ok(Self { state })
    }
}

/// Result from a KEYSTATE command.
#[derive(Debug, Clone)]
pub struct KeyStateResult {
    /// The list of keys in the keyspace.
    pub keys: Vec<KeyInfo>,
}

impl KeyStateResult {
    /// Parse a `KeyStateResult` from a RESP3 response.
    pub fn from_response(resp: Response) -> Result<Self, ClientError> {
        if let Response::Error(e) = &resp {
            return Err(ClientError::Server(e.clone()));
        }
        // KEYSTATE returns a map with a "keys" field containing an array of maps
        let keys_resp = resp.get_field("keys").ok_or_else(|| {
            ClientError::ResponseFormat("KEYSTATE response missing 'keys' field".into())
        })?;
        let mut keys = Vec::new();
        if let Response::Array(items) = keys_resp {
            for item in items {
                let key_id = item
                    .get_string_field("key_id")
                    .unwrap_or_else(|| "unknown".into());
                let state = item
                    .get_string_field("state")
                    .unwrap_or_else(|| "unknown".into());
                let algorithm = item.get_string_field("algorithm");
                let version = item.get_int_field("version");
                let created_at = item.get_int_field("created_at");
                keys.push(KeyInfo {
                    key_id,
                    state,
                    algorithm,
                    version,
                    created_at,
                });
            }
        } else {
            return Err(ClientError::ResponseFormat(format!(
                "expected Array for 'keys' field, got {}",
                keys_resp.type_name()
            )));
        }
        Ok(Self { keys })
    }
}

/// Information about a single signing key.
#[derive(Debug, Clone)]
pub struct KeyInfo {
    /// The key identifier.
    pub key_id: String,
    /// The key state (e.g. `"Active"`, `"Draining"`, `"Staged"`).
    pub state: String,
    /// The signing algorithm (e.g. `"ES256"`).
    pub algorithm: Option<String>,
    /// The key version.
    pub version: Option<i64>,
    /// When the key was created (Unix timestamp).
    pub created_at: Option<i64>,
}

/// Generic result for commands that return a map with status and optional fields.
#[derive(Debug, Clone)]
pub struct OkResult {
    /// All fields from the response map.
    pub fields: HashMap<String, serde_json::Value>,
}

impl OkResult {
    /// Parse an `OkResult` from a RESP3 map response.
    pub fn from_response(resp: Response) -> Result<Self, ClientError> {
        match &resp {
            Response::Error(e) => {
                if e.contains("DENIED") {
                    return Err(ClientError::AuthRequired);
                }
                Err(ClientError::Server(e.clone()))
            }
            Response::Map(entries) => {
                let mut fields = HashMap::new();
                for (k, v) in entries {
                    fields.insert(k.to_display_string(), v.to_json());
                }
                Ok(Self { fields })
            }
            _ => Err(ClientError::ResponseFormat(format!(
                "expected Map response, got {}",
                resp.type_name()
            ))),
        }
    }
}
