use axum::Json;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::Value;

use keyva_protocol::{CommandResponse, ResponseMap, ResponseValue};

use crate::error::error_to_status;

// ---------------------------------------------------------------------------
// Request body structs
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct IssueBody {
    pub claims: Option<Value>,
    pub metadata: Option<Value>,
    pub ttl: Option<u64>,
    pub idempotency_key: Option<String>,
}

#[derive(Deserialize)]
pub struct VerifyBody {
    pub token: String,
    pub payload: Option<String>,
    pub check_revoked: Option<bool>,
}

#[derive(Deserialize)]
pub struct RevokeBody {
    pub credential_id: Option<String>,
    pub family_id: Option<String>,
    pub bulk: Option<Vec<String>>,
    pub ttl: Option<u64>,
}

#[derive(Deserialize)]
pub struct RefreshBody {
    pub token: String,
}

#[derive(Deserialize)]
pub struct UpdateBody {
    pub credential_id: String,
    pub metadata: Value,
}

#[derive(Deserialize)]
pub struct RotateBody {
    pub force: Option<bool>,
    pub nowait: Option<bool>,
    pub dryrun: Option<bool>,
}

#[derive(Deserialize)]
pub struct SuspendBody {
    pub credential_id: String,
}

#[derive(Deserialize)]
pub struct PasswordSetBody {
    pub user_id: String,
    pub password: String,
    pub metadata: Option<Value>,
}

#[derive(Deserialize)]
pub struct PasswordVerifyBody {
    pub user_id: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct PasswordChangeBody {
    pub user_id: String,
    pub old_password: String,
    pub new_password: String,
}

#[derive(Deserialize)]
pub struct PasswordImportBody {
    pub user_id: String,
    pub hash: String,
    pub metadata: Option<Value>,
}

// ---------------------------------------------------------------------------
// Query params
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct KeysQuery {
    pub cursor: Option<String>,
    pub pattern: Option<String>,
    pub state: Option<String>,
    pub count: Option<usize>,
}

// ---------------------------------------------------------------------------
// Response serialization
// ---------------------------------------------------------------------------

fn response_value_to_json(v: &ResponseValue) -> Value {
    match v {
        ResponseValue::String(s) => Value::String(s.clone()),
        ResponseValue::Integer(n) => serde_json::json!(*n),
        ResponseValue::Float(f) => serde_json::json!(*f),
        ResponseValue::Boolean(b) => Value::Bool(*b),
        ResponseValue::Bytes(b) => {
            // Encode bytes as base64 string for JSON transport.
            use serde_json::json;
            json!(base64_encode(b))
        }
        ResponseValue::Null => Value::Null,
        ResponseValue::Map(m) => response_map_to_value(m),
        ResponseValue::Array(arr) => Value::Array(arr.iter().map(response_value_to_json).collect()),
        ResponseValue::Json(v) => v.clone(),
    }
}

fn response_map_to_value(map: &ResponseMap) -> Value {
    let obj: serde_json::Map<String, Value> = map
        .fields
        .iter()
        .map(|(k, v)| (k.clone(), response_value_to_json(v)))
        .collect();
    Value::Object(obj)
}

fn command_response_to_json(resp: &CommandResponse) -> Value {
    match resp {
        CommandResponse::Success(map) => response_map_to_value(map),
        CommandResponse::Error(err) => {
            let code = error_code_string(err);
            serde_json::json!({ "error": code, "message": err.to_string() })
        }
        CommandResponse::Array(items) => {
            Value::Array(items.iter().map(command_response_to_json).collect())
        }
    }
}

/// Converts a `CommandResponse` into an HTTP status code and JSON body.
pub fn response_to_json(resp: &CommandResponse) -> (StatusCode, Json<Value>) {
    match resp {
        CommandResponse::Success(_) | CommandResponse::Array(_) => {
            (StatusCode::OK, Json(command_response_to_json(resp)))
        }
        CommandResponse::Error(err) => {
            let status = error_to_status(err);
            (status, Json(command_response_to_json(resp)))
        }
    }
}

/// Returns a short machine-readable error code for the variant.
fn error_code_string(err: &keyva_protocol::CommandError) -> &'static str {
    use keyva_protocol::CommandError;
    match err {
        CommandError::BadArg { .. } => "BAD_ARG",
        CommandError::ValidationError(_) => "VALIDATION_ERROR",
        CommandError::WrongType { .. } => "WRONG_TYPE",
        CommandError::NotFound { .. } => "NOT_FOUND",
        CommandError::Denied { .. } => "DENIED",
        CommandError::Expired { .. } => "EXPIRED",
        CommandError::Disabled { .. } => "DISABLED",
        CommandError::NotReady(_) => "NOT_READY",
        CommandError::ReuseDetected { .. } => "REUSE_DETECTED",
        CommandError::ChainLimit { .. } => "CHAIN_LIMIT",
        CommandError::StateError { .. } => "STATE_ERROR",
        CommandError::Locked { .. } => "LOCKED",
        CommandError::Storage(_) => "STORAGE",
        CommandError::Crypto(_) => "CRYPTO",
        CommandError::Internal(_) => "INTERNAL",
    }
}

/// Simple base64 encoding without pulling in an extra crate.
fn base64_encode(data: &[u8]) -> String {
    use std::fmt::Write;
    const CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        let _ = write!(out, "{}", CHARS[((triple >> 18) & 0x3F) as usize] as char);
        let _ = write!(out, "{}", CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            let _ = write!(out, "{}", CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            let _ = write!(out, "{}", CHARS[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}
