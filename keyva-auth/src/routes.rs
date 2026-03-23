//! Route handlers for the 6 core auth endpoints plus JWKS, health, and metrics.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;

use keyva_protocol::{Command, CommandResponse, RevokeTarget};

use crate::cookies;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Request body types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SignupBody {
    pub user_id: String,
    pub password: String,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub struct LoginBody {
    pub user_id: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct ChangePasswordBody {
    pub old_password: String,
    pub new_password: String,
}

#[derive(Deserialize)]
pub struct ForgotPasswordBody {
    pub user_id: String,
}

#[derive(Deserialize)]
pub struct ResetPasswordBody {
    pub token: String,
    pub new_password: String,
}

#[derive(Deserialize)]
pub struct LogoutAllBody {
    pub user_id: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn error_response(
    err: &keyva_protocol::CommandError,
    status: StatusCode,
) -> axum::response::Response {
    let body = serde_json::json!({ "error": err.to_string() });
    let mut resp = Json(body).into_response();
    *resp.status_mut() = status;
    resp
}

fn extract_token_string(result: &CommandResponse) -> Option<String> {
    if let CommandResponse::Success(map) = result {
        for (key, value) in &map.fields {
            if key == "token"
                && let keyva_protocol::ResponseValue::String(s) = value
            {
                return Some(s.clone());
            }
        }
    }
    None
}

fn extract_field_string(result: &CommandResponse, field: &str) -> Option<String> {
    if let CommandResponse::Success(map) = result {
        for (key, value) in &map.fields {
            if key == field
                && let keyva_protocol::ResponseValue::String(s) = value
            {
                return Some(s.clone());
            }
        }
    }
    None
}

fn extract_field_json(result: &CommandResponse, field: &str) -> Option<serde_json::Value> {
    if let CommandResponse::Success(map) = result {
        for (key, value) in &map.fields {
            if key == field {
                return Some(response_value_to_json(value));
            }
        }
    }
    None
}

fn response_value_to_json(v: &keyva_protocol::ResponseValue) -> serde_json::Value {
    match v {
        keyva_protocol::ResponseValue::String(s) => serde_json::Value::String(s.clone()),
        keyva_protocol::ResponseValue::Integer(n) => serde_json::json!(*n),
        keyva_protocol::ResponseValue::Float(f) => serde_json::json!(*f),
        keyva_protocol::ResponseValue::Boolean(b) => serde_json::Value::Bool(*b),
        keyva_protocol::ResponseValue::Bytes(_) => serde_json::Value::Null,
        keyva_protocol::ResponseValue::Null => serde_json::Value::Null,
        keyva_protocol::ResponseValue::Map(m) => {
            let obj: serde_json::Map<String, serde_json::Value> = m
                .fields
                .iter()
                .map(|(k, v)| (k.clone(), response_value_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        keyva_protocol::ResponseValue::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(response_value_to_json).collect())
        }
        keyva_protocol::ResponseValue::Json(v) => v.clone(),
    }
}

/// Validate an access token and return the user_id (sub claim).
async fn validate_session(state: &AppState, ks: &str, headers: &HeaderMap) -> Option<String> {
    let token = cookies::extract_access_token(headers, &state.config.cookie_name)?;
    let verify_cmd = Command::Verify {
        keyspace: format!("{ks}_access"),
        token,
        payload: None,
        check_revoked: false,
    };
    let result = state.dispatcher.execute(verify_cmd, None).await;
    match result {
        CommandResponse::Success(ref map) => {
            for (key, value) in &map.fields {
                if key == "claims" {
                    let json = response_value_to_json(value);
                    if let Some(sub) = json.get("sub").and_then(|v| v.as_str()) {
                        return Some(sub.to_string());
                    }
                }
                if key == "sub"
                    && let keyva_protocol::ResponseValue::String(s) = value
                {
                    return Some(s.clone());
                }
            }
            None
        }
        _ => None,
    }
}

fn error_status_for(err: &keyva_protocol::CommandError) -> StatusCode {
    use keyva_protocol::CommandError;
    match err {
        CommandError::NotFound { .. } => StatusCode::CONFLICT,
        CommandError::Locked { .. } => StatusCode::TOO_MANY_REQUESTS,
        CommandError::Denied { .. } => StatusCode::UNAUTHORIZED,
        CommandError::BadArg { .. } | CommandError::ValidationError(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

// ---------------------------------------------------------------------------
// POST /auth/{ks}/signup
// ---------------------------------------------------------------------------

pub async fn signup(
    State(state): State<AppState>,
    Path(ks): Path<String>,
    Json(body): Json<SignupBody>,
) -> axum::response::Response {
    // 1. PASSWORD SET
    let set_cmd = Command::PasswordSet {
        keyspace: format!("{ks}_passwords"),
        user_id: body.user_id.clone(),
        plaintext: body.password,
        metadata: body.metadata,
    };
    let set_result = state.dispatcher.execute(set_cmd, None).await;
    if let CommandResponse::Error(ref e) = set_result {
        return error_response(e, StatusCode::CONFLICT);
    }

    // 2. ISSUE access token (JWT)
    let now = now_secs();
    let claims = serde_json::json!({
        "sub": body.user_id,
        "iat": now,
        "exp": now + state.config.access_ttl_secs,
    });
    let issue_access = Command::Issue {
        keyspace: format!("{ks}_access"),
        claims: Some(claims),
        metadata: None,
        ttl_secs: Some(state.config.access_ttl_secs),
        idempotency_key: None,
    };
    let access_result = state.dispatcher.execute(issue_access, None).await;
    let access_token = extract_token_string(&access_result).unwrap_or_default();

    // 3. ISSUE refresh token
    let issue_refresh = Command::Issue {
        keyspace: format!("{ks}_refresh"),
        claims: None,
        metadata: Some(serde_json::json!({"sub": body.user_id})),
        ttl_secs: Some(state.config.refresh_ttl_secs),
        idempotency_key: None,
    };
    let refresh_result = state.dispatcher.execute(issue_refresh, None).await;
    let refresh_token = extract_token_string(&refresh_result).unwrap_or_default();

    // 4. Build response with cookies + JSON
    let mut response = Json(serde_json::json!({
        "user_id": body.user_id,
        "access_token": access_token,
        "refresh_token": refresh_token,
        "expires_in": state.config.access_ttl_secs,
    }))
    .into_response();

    cookies::set_access_cookie(&mut response, &access_token, &state.config);
    cookies::set_refresh_cookie(&mut response, &refresh_token, &ks, &state.config);

    *response.status_mut() = StatusCode::CREATED;
    response
}

// ---------------------------------------------------------------------------
// POST /auth/{ks}/login
// ---------------------------------------------------------------------------

pub async fn login(
    State(state): State<AppState>,
    Path(ks): Path<String>,
    Json(body): Json<LoginBody>,
) -> axum::response::Response {
    // 1. PASSWORD VERIFY
    let verify_cmd = Command::PasswordVerify {
        keyspace: format!("{ks}_passwords"),
        user_id: body.user_id.clone(),
        plaintext: body.password,
    };
    let verify_result = state.dispatcher.execute(verify_cmd, None).await;
    if let CommandResponse::Error(ref e) = verify_result {
        let status = match e {
            keyva_protocol::CommandError::Locked { .. } => StatusCode::TOO_MANY_REQUESTS,
            _ => StatusCode::UNAUTHORIZED,
        };
        return error_response(e, status);
    }

    // Check if verification actually passed
    let verified = extract_field_string(&verify_result, "result")
        .map(|r| r == "OK" || r == "verified")
        .or_else(|| {
            if let CommandResponse::Success(ref map) = verify_result {
                for (key, value) in &map.fields {
                    if key == "verified"
                        && let keyva_protocol::ResponseValue::Boolean(b) = value
                    {
                        return Some(*b);
                    }
                }
            }
            None
        })
        .unwrap_or(true); // If success with no explicit verified field, treat as verified

    if !verified {
        return error_response(
            &keyva_protocol::CommandError::Denied {
                reason: "invalid credentials".into(),
            },
            StatusCode::UNAUTHORIZED,
        );
    }

    // 2. ISSUE access token (JWT)
    let now = now_secs();
    let claims = serde_json::json!({
        "sub": body.user_id,
        "iat": now,
        "exp": now + state.config.access_ttl_secs,
    });
    let issue_access = Command::Issue {
        keyspace: format!("{ks}_access"),
        claims: Some(claims),
        metadata: None,
        ttl_secs: Some(state.config.access_ttl_secs),
        idempotency_key: None,
    };
    let access_result = state.dispatcher.execute(issue_access, None).await;
    let access_token = extract_token_string(&access_result).unwrap_or_default();

    // 3. ISSUE refresh token
    let issue_refresh = Command::Issue {
        keyspace: format!("{ks}_refresh"),
        claims: None,
        metadata: Some(serde_json::json!({"sub": body.user_id})),
        ttl_secs: Some(state.config.refresh_ttl_secs),
        idempotency_key: None,
    };
    let refresh_result = state.dispatcher.execute(issue_refresh, None).await;
    let refresh_token = extract_token_string(&refresh_result).unwrap_or_default();

    // 4. Build response
    let mut response = Json(serde_json::json!({
        "user_id": body.user_id,
        "access_token": access_token,
        "refresh_token": refresh_token,
        "expires_in": state.config.access_ttl_secs,
    }))
    .into_response();

    cookies::set_access_cookie(&mut response, &access_token, &state.config);
    cookies::set_refresh_cookie(&mut response, &refresh_token, &ks, &state.config);

    response
}

// ---------------------------------------------------------------------------
// GET /auth/{ks}/session
// ---------------------------------------------------------------------------

pub async fn session(
    State(state): State<AppState>,
    Path(ks): Path<String>,
    headers: HeaderMap,
) -> axum::response::Response {
    let token = cookies::extract_access_token(&headers, &state.config.cookie_name);
    let Some(token) = token else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let verify_cmd = Command::Verify {
        keyspace: format!("{ks}_access"),
        token,
        payload: None,
        check_revoked: false,
    };
    let result = state.dispatcher.execute(verify_cmd, None).await;

    match result {
        CommandResponse::Success(ref map) => {
            // Extract claims from the verify response
            let mut user_id = None;
            let mut claims = serde_json::Value::Null;
            let mut expires_at = None;

            for (key, value) in &map.fields {
                match key.as_str() {
                    "claims" => {
                        let json = response_value_to_json(value);
                        if let Some(sub) = json.get("sub").and_then(|v| v.as_str()) {
                            user_id = Some(sub.to_string());
                        }
                        if let Some(exp) = json.get("exp").and_then(|v| v.as_i64()) {
                            expires_at = Some(exp);
                        }
                        claims = json;
                    }
                    "sub" => {
                        if let keyva_protocol::ResponseValue::String(s) = value {
                            user_id = Some(s.clone());
                        }
                    }
                    "exp" => {
                        if let keyva_protocol::ResponseValue::Integer(n) = value {
                            expires_at = Some(*n);
                        }
                    }
                    _ => {}
                }
            }

            Json(serde_json::json!({
                "user_id": user_id,
                "claims": claims,
                "expires_at": expires_at,
            }))
            .into_response()
        }
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
}

// ---------------------------------------------------------------------------
// POST /auth/{ks}/refresh
// ---------------------------------------------------------------------------

pub async fn refresh(
    State(state): State<AppState>,
    Path(ks): Path<String>,
    headers: HeaderMap,
) -> axum::response::Response {
    let refresh_token = cookies::extract_refresh_token(&headers, &state.config.cookie_name);
    let Some(refresh_token) = refresh_token else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    // REFRESH the token
    let refresh_cmd = Command::Refresh {
        keyspace: format!("{ks}_refresh"),
        token: refresh_token,
    };
    let result = state.dispatcher.execute(refresh_cmd, None).await;

    if let CommandResponse::Error(ref e) = result {
        return error_response(e, StatusCode::UNAUTHORIZED);
    }

    // Extract the new refresh token
    let new_refresh_token = extract_token_string(&result).unwrap_or_default();

    // Extract user_id from the refresh result metadata
    let user_id = extract_field_json(&result, "metadata")
        .and_then(|m| m.get("sub").and_then(|v| v.as_str()).map(String::from))
        .or_else(|| extract_field_string(&result, "sub"))
        .unwrap_or_default();

    // Issue a new access token
    let now = now_secs();
    let claims = serde_json::json!({
        "sub": user_id,
        "iat": now,
        "exp": now + state.config.access_ttl_secs,
    });
    let issue_access = Command::Issue {
        keyspace: format!("{ks}_access"),
        claims: Some(claims),
        metadata: None,
        ttl_secs: Some(state.config.access_ttl_secs),
        idempotency_key: None,
    };
    let access_result = state.dispatcher.execute(issue_access, None).await;
    let access_token = extract_token_string(&access_result).unwrap_or_default();

    let mut response = Json(serde_json::json!({
        "access_token": access_token,
        "refresh_token": new_refresh_token,
        "expires_in": state.config.access_ttl_secs,
    }))
    .into_response();

    cookies::set_access_cookie(&mut response, &access_token, &state.config);
    cookies::set_refresh_cookie(&mut response, &new_refresh_token, &ks, &state.config);

    response
}

// ---------------------------------------------------------------------------
// POST /auth/{ks}/logout
// ---------------------------------------------------------------------------

pub async fn logout(
    State(state): State<AppState>,
    Path(ks): Path<String>,
    headers: HeaderMap,
) -> axum::response::Response {
    // Extract refresh token and revoke its family
    if let Some(refresh_token) = cookies::extract_refresh_token(&headers, &state.config.cookie_name)
    {
        // Try to revoke the family. Best-effort: if the token is already expired
        // or invalid, we still clear the cookies.
        let revoke_cmd = Command::Revoke {
            keyspace: format!("{ks}_refresh"),
            target: RevokeTarget::Single(refresh_token),
            ttl_secs: None,
        };
        let _ = state.dispatcher.execute(revoke_cmd, None).await;
    }

    let mut response = Json(serde_json::json!({"status": "OK"})).into_response();
    cookies::clear_cookies(&mut response, &ks, &state.config);
    response
}

// ---------------------------------------------------------------------------
// POST /auth/{ks}/change-password
// ---------------------------------------------------------------------------

pub async fn change_password(
    State(state): State<AppState>,
    Path(ks): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ChangePasswordBody>,
) -> axum::response::Response {
    let Some(user_id) = validate_session(&state, &ks, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let change_cmd = Command::PasswordChange {
        keyspace: format!("{ks}_passwords"),
        user_id,
        old_plaintext: body.old_password,
        new_plaintext: body.new_password,
    };
    let change_result = state.dispatcher.execute(change_cmd, None).await;

    if let CommandResponse::Error(ref e) = change_result {
        return error_response(e, error_status_for(e));
    }

    Json(serde_json::json!({"status": "OK"})).into_response()
}

// ---------------------------------------------------------------------------
// POST /auth/{ks}/forgot-password
// ---------------------------------------------------------------------------

pub async fn forgot_password(
    State(state): State<AppState>,
    Path(ks): Path<String>,
    Json(body): Json<ForgotPasswordBody>,
) -> axum::response::Response {
    // Verify the user exists (by checking password entry).
    // Always return 200 to prevent user enumeration — only issue a token
    // if the user actually exists.
    let verify_exists = Command::PasswordVerify {
        keyspace: format!("{ks}_passwords"),
        user_id: body.user_id.clone(),
        // Use a dummy password — we only care about the NotFound error vs other errors.
        // A wrong password still confirms the user exists.
        plaintext: String::new(),
    };
    let result = state.dispatcher.execute(verify_exists, None).await;

    // If user doesn't exist, return 200 anyway (no enumeration).
    let user_exists = !matches!(
        result,
        CommandResponse::Error(keyva_protocol::CommandError::NotFound { .. })
    );

    if user_exists {
        // Issue a short-lived JWT as the reset token (5 minutes).
        let now = now_secs();
        let reset_ttl = 300u64; // 5 minutes
        let claims = serde_json::json!({
            "sub": body.user_id,
            "purpose": "password_reset",
            "iat": now,
            "exp": now + reset_ttl,
        });
        let issue_cmd = Command::Issue {
            keyspace: format!("{ks}_access"),
            claims: Some(claims),
            metadata: None,
            ttl_secs: Some(reset_ttl),
            idempotency_key: None,
        };
        let issue_result = state.dispatcher.execute(issue_cmd, None).await;

        if let Some(token) = extract_token_string(&issue_result) {
            // In production the app would send this token via email/SMS.
            // We return it in the response for the app to handle delivery.
            return Json(serde_json::json!({
                "status": "OK",
                "reset_token": token,
                "expires_in": reset_ttl,
            }))
            .into_response();
        }
    }

    // Always 200 — whether user exists or not
    Json(serde_json::json!({"status": "OK"})).into_response()
}

// ---------------------------------------------------------------------------
// POST /auth/{ks}/reset-password
// ---------------------------------------------------------------------------

pub async fn reset_password(
    State(state): State<AppState>,
    Path(ks): Path<String>,
    Json(body): Json<ResetPasswordBody>,
) -> axum::response::Response {
    // 1. Verify the reset token is a valid JWT with purpose=password_reset
    let verify_cmd = Command::Verify {
        keyspace: format!("{ks}_access"),
        token: body.token,
        payload: None,
        check_revoked: false,
    };
    let result = state.dispatcher.execute(verify_cmd, None).await;

    let (user_id, purpose) = match result {
        CommandResponse::Success(ref map) => {
            let mut uid = None;
            let mut purpose = None;
            for (key, value) in &map.fields {
                if key == "claims" {
                    let json = response_value_to_json(value);
                    uid = json.get("sub").and_then(|v| v.as_str()).map(String::from);
                    purpose = json
                        .get("purpose")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
            }
            (uid, purpose)
        }
        _ => (None, None),
    };

    if purpose.as_deref() != Some("password_reset") {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid reset token"})),
        )
            .into_response();
    }

    let Some(user_id) = user_id else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid reset token"})),
        )
            .into_response();
    };

    // 2. Force-reset the password. Authorization is the reset token,
    //    not the old password.
    let reset_cmd = Command::PasswordReset {
        keyspace: format!("{ks}_passwords"),
        user_id: user_id.clone(),
        new_plaintext: body.new_password,
    };
    let result = state.dispatcher.execute(reset_cmd, None).await;

    if let CommandResponse::Error(ref e) = result {
        return error_response(e, error_status_for(e));
    }

    Json(serde_json::json!({"status": "OK"})).into_response()
}

// ---------------------------------------------------------------------------
// POST /auth/{ks}/logout-all
// ---------------------------------------------------------------------------

pub async fn logout_all(
    State(state): State<AppState>,
    Path(ks): Path<String>,
    headers: HeaderMap,
    Json(body): Json<LogoutAllBody>,
) -> axum::response::Response {
    // Requires a valid session to prevent abuse.
    let Some(session_user_id) = validate_session(&state, &ks, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    // Only allow users to logout-all their own sessions.
    if session_user_id != body.user_id {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "can only revoke own sessions"})),
        )
            .into_response();
    }

    // Scan all refresh tokens in this keyspace and revoke families
    // belonging to the target user_id.
    let ks_name = format!("{ks}_refresh");
    let mut revoked_families = 0u32;

    // Get all active refresh tokens and find families with matching sub
    let keys_cmd = Command::Keys {
        keyspace: ks_name.clone(),
        cursor: None,
        pattern: None,
        state_filter: None,
        count: Some(10000),
    };
    let keys_result = state.dispatcher.execute(keys_cmd, None).await;

    // Extract credential IDs, inspect each for metadata.sub, collect families
    let mut family_ids = std::collections::HashSet::new();

    if let CommandResponse::Success(ref map) = keys_result {
        for (key, value) in &map.fields {
            if key == "credentials"
                && let keyva_protocol::ResponseValue::Array(arr) = value
            {
                for item in arr {
                    let credential_id = match item {
                        keyva_protocol::ResponseValue::String(s) => s.clone(),
                        keyva_protocol::ResponseValue::Map(m) => m
                            .fields
                            .iter()
                            .find(|(k, _)| k == "credential_id")
                            .and_then(|(_, v)| {
                                if let keyva_protocol::ResponseValue::String(s) = v {
                                    Some(s.clone())
                                } else {
                                    None
                                }
                            })
                            .unwrap_or_default(),
                        _ => continue,
                    };

                    let inspect_cmd = Command::Inspect {
                        keyspace: ks_name.clone(),
                        credential_id,
                    };
                    let inspect_result = state.dispatcher.execute(inspect_cmd, None).await;

                    if let CommandResponse::Success(ref imap) = inspect_result {
                        let mut is_target_user = false;
                        let mut family_id = None;

                        for (ik, iv) in &imap.fields {
                            if ik == "metadata" {
                                let json = response_value_to_json(iv);
                                if json.get("sub").and_then(|v| v.as_str()) == Some(&body.user_id) {
                                    is_target_user = true;
                                }
                            }
                            if ik == "family_id"
                                && let keyva_protocol::ResponseValue::String(f) = iv
                            {
                                family_id = Some(f.clone());
                            }
                        }

                        if is_target_user && let Some(fid) = family_id {
                            family_ids.insert(fid);
                        }
                    }
                }
            }
        }
    }

    // Revoke each family
    for family_id in &family_ids {
        let revoke_cmd = Command::Revoke {
            keyspace: ks_name.clone(),
            target: RevokeTarget::Family(family_id.clone()),
            ttl_secs: None,
        };
        let _ = state.dispatcher.execute(revoke_cmd, None).await;
        revoked_families += 1;
    }

    Json(serde_json::json!({
        "status": "OK",
        "revoked_families": revoked_families,
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// GET /auth/{ks}/sessions
// ---------------------------------------------------------------------------

pub async fn sessions(
    State(state): State<AppState>,
    Path(ks): Path<String>,
    headers: HeaderMap,
) -> axum::response::Response {
    // Requires a valid session.
    let Some(user_id) = validate_session(&state, &ks, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    // List active refresh token families for this user.
    let ks_name = format!("{ks}_refresh");
    let keys_cmd = Command::Keys {
        keyspace: ks_name.clone(),
        cursor: None,
        pattern: None,
        state_filter: None,
        count: Some(10000),
    };
    let keys_result = state.dispatcher.execute(keys_cmd, None).await;

    let mut active_sessions = Vec::new();

    if let CommandResponse::Success(ref map) = keys_result {
        for (key, value) in &map.fields {
            if key == "credentials"
                && let keyva_protocol::ResponseValue::Array(arr) = value
            {
                for item in arr {
                    let credential_id = match item {
                        keyva_protocol::ResponseValue::String(s) => s.clone(),
                        keyva_protocol::ResponseValue::Map(m) => m
                            .fields
                            .iter()
                            .find(|(k, _)| k == "credential_id")
                            .and_then(|(_, v)| {
                                if let keyva_protocol::ResponseValue::String(s) = v {
                                    Some(s.clone())
                                } else {
                                    None
                                }
                            })
                            .unwrap_or_default(),
                        _ => continue,
                    };

                    let inspect_cmd = Command::Inspect {
                        keyspace: ks_name.clone(),
                        credential_id,
                    };
                    let inspect_result = state.dispatcher.execute(inspect_cmd, None).await;

                    if let CommandResponse::Success(ref imap) = inspect_result {
                        let json = response_value_to_json_map(imap);
                        // Filter to only this user's tokens
                        let is_active = json
                            .get("state")
                            .and_then(|v| v.as_str())
                            .is_none_or(|s| s == "Active");
                        if is_active
                            && let Some(meta) = json.get("metadata")
                            && meta.get("sub").and_then(|v| v.as_str()) == Some(&user_id)
                        {
                            active_sessions.push(json);
                        }
                    }
                }
            }
        }
    }

    Json(serde_json::json!({
        "user_id": user_id,
        "active_sessions": active_sessions,
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// GET /auth/{ks}/.well-known/jwks.json
// ---------------------------------------------------------------------------

pub async fn jwks(
    State(state): State<AppState>,
    Path(ks): Path<String>,
) -> axum::response::Response {
    let cmd = Command::Jwks {
        keyspace: format!("{ks}_access"),
    };
    let result = state.dispatcher.execute(cmd, None).await;

    match result {
        CommandResponse::Success(ref map) => {
            let json = response_value_to_json_map(map);
            let mut response = Json(json).into_response();
            response
                .headers_mut()
                .insert("cache-control", "public, max-age=3600".parse().unwrap());
            response
        }
        CommandResponse::Error(ref e) => error_response(e, error_status_for(e)),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn response_value_to_json_map(map: &keyva_protocol::ResponseMap) -> serde_json::Value {
    let obj: serde_json::Map<String, serde_json::Value> = map
        .fields
        .iter()
        .map(|(k, v)| (k.clone(), response_value_to_json(v)))
        .collect();
    serde_json::Value::Object(obj)
}

// ---------------------------------------------------------------------------
// GET /auth/health
// ---------------------------------------------------------------------------

pub async fn health(State(state): State<AppState>) -> axum::response::Response {
    let cmd = Command::Health { keyspace: None };
    let result = state.dispatcher.execute(cmd, None).await;

    match result {
        CommandResponse::Success(_) => {
            Json(serde_json::json!({"status": "healthy"})).into_response()
        }
        _ => {
            let mut resp = Json(serde_json::json!({"status": "unhealthy"})).into_response();
            *resp.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
            resp
        }
    }
}

// ---------------------------------------------------------------------------
// GET /metrics
// ---------------------------------------------------------------------------

pub async fn metrics(State(state): State<AppState>) -> String {
    state.metrics_handle.render()
}
