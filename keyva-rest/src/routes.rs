use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use serde_json::Value;

use keyva_protocol::auth::AuthPolicy;
use keyva_protocol::{Command, CommandDispatcher, CommandError, CommandResponse, RevokeTarget};

use crate::AppState;
use crate::json::{
    IssueBody, KeysQuery, PasswordChangeBody, PasswordImportBody, PasswordSetBody,
    PasswordVerifyBody, RefreshBody, RevokeBody, RotateBody, SuspendBody, UpdateBody, VerifyBody,
};

/// Extract an auth policy from the Authorization header, if present.
/// Returns `Err` with a 401 response if auth is required but not provided,
/// or if the token is invalid.
fn extract_auth(
    headers: &HeaderMap,
    dispatcher: &CommandDispatcher,
) -> Result<Option<AuthPolicy>, (StatusCode, Json<Value>)> {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(value) => {
            let token = value.strip_prefix("Bearer ").unwrap_or(value);
            match dispatcher.auth_registry().authenticate(token) {
                Ok(policy) => Ok(Some(policy.clone())),
                Err(_) => Err((
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({
                        "error": "DENIED",
                        "message": "invalid token"
                    })),
                )),
            }
        }
        None => {
            if dispatcher.auth_registry().is_required() {
                Err((
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({
                        "error": "DENIED",
                        "message": "authentication required"
                    })),
                ))
            } else {
                Ok(None)
            }
        }
    }
}

pub async fn get_metrics(State(state): State<AppState>) -> impl IntoResponse {
    let body = state.metrics_handle.render();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        body,
    )
}

pub async fn post_issue(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(keyspace): Path<String>,
    Json(body): Json<IssueBody>,
) -> impl IntoResponse {
    let auth = match extract_auth(&headers, &state.dispatcher) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let cmd = Command::Issue {
        keyspace,
        claims: body.claims,
        metadata: body.metadata,
        ttl_secs: body.ttl,
        idempotency_key: body.idempotency_key,
    };
    let resp = state.dispatcher.execute(cmd, auth.as_ref()).await;
    crate::json::response_to_json(&resp).into_response()
}

pub async fn post_verify(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(keyspace): Path<String>,
    Json(body): Json<VerifyBody>,
) -> impl IntoResponse {
    let auth = match extract_auth(&headers, &state.dispatcher) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let cmd = Command::Verify {
        keyspace,
        token: body.token,
        payload: body.payload,
        check_revoked: body.check_revoked.unwrap_or(false),
    };
    let resp = state.dispatcher.execute(cmd, auth.as_ref()).await;
    crate::json::response_to_json(&resp).into_response()
}

pub async fn post_revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(keyspace): Path<String>,
    Json(body): Json<RevokeBody>,
) -> impl IntoResponse {
    let auth = match extract_auth(&headers, &state.dispatcher) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let target = if let Some(ids) = body.bulk {
        RevokeTarget::Bulk(ids)
    } else if let Some(fam) = body.family_id {
        RevokeTarget::Family(fam)
    } else if let Some(cid) = body.credential_id {
        RevokeTarget::Single(cid)
    } else {
        let resp = CommandResponse::Error(CommandError::BadArg {
            message: "one of credential_id, family_id, or bulk is required".into(),
        });
        return crate::json::response_to_json(&resp).into_response();
    };

    let cmd = Command::Revoke {
        keyspace,
        target,
        ttl_secs: body.ttl,
    };
    let resp = state.dispatcher.execute(cmd, auth.as_ref()).await;
    crate::json::response_to_json(&resp).into_response()
}

pub async fn post_refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(keyspace): Path<String>,
    Json(body): Json<RefreshBody>,
) -> impl IntoResponse {
    let auth = match extract_auth(&headers, &state.dispatcher) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let cmd = Command::Refresh {
        keyspace,
        token: body.token,
    };
    let resp = state.dispatcher.execute(cmd, auth.as_ref()).await;
    crate::json::response_to_json(&resp).into_response()
}

pub async fn post_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(keyspace): Path<String>,
    Json(body): Json<UpdateBody>,
) -> impl IntoResponse {
    let auth = match extract_auth(&headers, &state.dispatcher) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let cmd = Command::Update {
        keyspace,
        credential_id: body.credential_id,
        metadata: body.metadata,
    };
    let resp = state.dispatcher.execute(cmd, auth.as_ref()).await;
    crate::json::response_to_json(&resp).into_response()
}

pub async fn post_rotate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(keyspace): Path<String>,
    Json(body): Json<RotateBody>,
) -> impl IntoResponse {
    let auth = match extract_auth(&headers, &state.dispatcher) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let cmd = Command::Rotate {
        keyspace,
        force: body.force.unwrap_or(false),
        nowait: body.nowait.unwrap_or(false),
        dryrun: body.dryrun.unwrap_or(false),
    };
    let resp = state.dispatcher.execute(cmd, auth.as_ref()).await;
    crate::json::response_to_json(&resp).into_response()
}

pub async fn post_suspend(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(keyspace): Path<String>,
    Json(body): Json<SuspendBody>,
) -> impl IntoResponse {
    let auth = match extract_auth(&headers, &state.dispatcher) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let cmd = Command::Suspend {
        keyspace,
        credential_id: body.credential_id,
    };
    let resp = state.dispatcher.execute(cmd, auth.as_ref()).await;
    crate::json::response_to_json(&resp).into_response()
}

pub async fn post_unsuspend(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(keyspace): Path<String>,
    Json(body): Json<SuspendBody>,
) -> impl IntoResponse {
    let auth = match extract_auth(&headers, &state.dispatcher) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let cmd = Command::Unsuspend {
        keyspace,
        credential_id: body.credential_id,
    };
    let resp = state.dispatcher.execute(cmd, auth.as_ref()).await;
    crate::json::response_to_json(&resp).into_response()
}

pub async fn get_inspect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((keyspace, credential_id)): Path<(String, String)>,
) -> impl IntoResponse {
    let auth = match extract_auth(&headers, &state.dispatcher) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let cmd = Command::Inspect {
        keyspace,
        credential_id,
    };
    let resp = state.dispatcher.execute(cmd, auth.as_ref()).await;
    crate::json::response_to_json(&resp).into_response()
}

pub async fn get_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(keyspace): Path<String>,
    Query(query): Query<KeysQuery>,
) -> impl IntoResponse {
    let auth = match extract_auth(&headers, &state.dispatcher) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let cmd = Command::Keys {
        keyspace,
        cursor: query.cursor,
        pattern: query.pattern,
        state_filter: query.state,
        count: query.count,
    };
    let resp = state.dispatcher.execute(cmd, auth.as_ref()).await;
    crate::json::response_to_json(&resp).into_response()
}

pub async fn get_keystate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(keyspace): Path<String>,
) -> impl IntoResponse {
    let auth = match extract_auth(&headers, &state.dispatcher) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let cmd = Command::KeyState { keyspace };
    let resp = state.dispatcher.execute(cmd, auth.as_ref()).await;
    crate::json::response_to_json(&resp).into_response()
}

pub async fn get_schema(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(keyspace): Path<String>,
) -> impl IntoResponse {
    let auth = match extract_auth(&headers, &state.dispatcher) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let cmd = Command::Schema { keyspace };
    let resp = state.dispatcher.execute(cmd, auth.as_ref()).await;
    crate::json::response_to_json(&resp).into_response()
}

pub async fn get_jwks(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(keyspace): Path<String>,
) -> impl IntoResponse {
    let auth = match extract_auth(&headers, &state.dispatcher) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };

    let cmd = Command::Jwks {
        keyspace: keyspace.clone(),
    };
    let resp = state.dispatcher.execute(cmd, auth.as_ref()).await;
    let (status, Json(body)) = crate::json::response_to_json(&resp);

    // Compute ETag from response body
    let body_str = body.to_string();
    let etag = format!("\"{:x}\"", crc32fast::hash(body_str.as_bytes()));

    // Check If-None-Match for conditional responses
    if let Some(if_none_match) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        && if_none_match == etag
    {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag.as_str())]).into_response();
    }

    // Compute dynamic Cache-Control based on rotation proximity
    let cache_control = compute_jwks_cache_control(&state, &keyspace);

    (
        status,
        [
            (header::CACHE_CONTROL, cache_control.as_str()),
            (header::ETAG, etag.as_str()),
        ],
        Json(body),
    )
        .into_response()
}

/// Compute Cache-Control header value based on how close the active key is to rotation.
fn compute_jwks_cache_control(state: &AppState, keyspace: &str) -> String {
    use keyva_core::KeyspacePolicy;

    let engine = state.dispatcher.engine();
    let index = engine.index();

    // Look up keyspace to get rotation_days
    let rotation_days = match index.keyspaces.get(keyspace) {
        Some(ks_ref) => match &ks_ref.value().policy {
            KeyspacePolicy::Jwt { rotation_days, .. } => *rotation_days,
            _ => return "public, max-age=3600".to_string(),
        },
        None => return "public, max-age=3600".to_string(),
    };

    // Get active key's created_at
    let active_created_at = index
        .jwt_rings
        .get(keyspace)
        .and_then(|ring| ring.active_key().map(|k| k.created_at));

    let Some(created_at) = active_created_at else {
        return "public, max-age=3600".to_string();
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let next_rotation = created_at + (rotation_days as u64 * 86400);
    let time_until_rotation = next_rotation.saturating_sub(now);

    let directive = if time_until_rotation > 86400 {
        "public, max-age=86400"
    } else if time_until_rotation > 3600 {
        "public, max-age=3600"
    } else if time_until_rotation > 300 {
        "public, max-age=300"
    } else {
        "no-cache"
    };

    directive.to_string()
}

pub async fn post_password_set(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(keyspace): Path<String>,
    Json(body): Json<PasswordSetBody>,
) -> impl IntoResponse {
    let auth = match extract_auth(&headers, &state.dispatcher) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let cmd = Command::PasswordSet {
        keyspace,
        user_id: body.user_id,
        plaintext: body.password,
        metadata: body.metadata,
    };
    let resp = state.dispatcher.execute(cmd, auth.as_ref()).await;
    crate::json::response_to_json(&resp).into_response()
}

pub async fn post_password_verify(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(keyspace): Path<String>,
    Json(body): Json<PasswordVerifyBody>,
) -> impl IntoResponse {
    let auth = match extract_auth(&headers, &state.dispatcher) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let cmd = Command::PasswordVerify {
        keyspace,
        user_id: body.user_id,
        plaintext: body.password,
    };
    let resp = state.dispatcher.execute(cmd, auth.as_ref()).await;
    crate::json::response_to_json(&resp).into_response()
}

pub async fn post_password_change(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(keyspace): Path<String>,
    Json(body): Json<PasswordChangeBody>,
) -> impl IntoResponse {
    let auth = match extract_auth(&headers, &state.dispatcher) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let cmd = Command::PasswordChange {
        keyspace,
        user_id: body.user_id,
        old_plaintext: body.old_password,
        new_plaintext: body.new_password,
    };
    let resp = state.dispatcher.execute(cmd, auth.as_ref()).await;
    crate::json::response_to_json(&resp).into_response()
}

pub async fn post_password_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(keyspace): Path<String>,
    Json(body): Json<PasswordImportBody>,
) -> impl IntoResponse {
    let auth = match extract_auth(&headers, &state.dispatcher) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let cmd = Command::PasswordImport {
        keyspace,
        user_id: body.user_id,
        hash: body.hash,
        metadata: body.metadata,
    };
    let resp = state.dispatcher.execute(cmd, auth.as_ref()).await;
    crate::json::response_to_json(&resp).into_response()
}

pub async fn get_health(State(state): State<AppState>) -> impl IntoResponse {
    // Health bypasses auth — no header extraction needed
    let cmd = Command::Health { keyspace: None };
    let resp = state.dispatcher.execute(cmd, None).await;
    crate::json::response_to_json(&resp).into_response()
}

pub async fn get_health_keyspace(
    State(state): State<AppState>,
    Path(keyspace): Path<String>,
) -> impl IntoResponse {
    // Health bypasses auth — no header extraction needed
    let cmd = Command::Health {
        keyspace: Some(keyspace),
    };
    let resp = state.dispatcher.execute(cmd, None).await;
    crate::json::response_to_json(&resp).into_response()
}
