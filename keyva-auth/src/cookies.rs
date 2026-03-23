//! Cookie helpers for setting, clearing, and extracting auth cookies.

use axum::http::header::{COOKIE, HeaderMap, SET_COOKIE};
use axum::response::Response;

use crate::config::RuntimeAuthConfig;

/// Set the access token cookie on the response.
pub fn set_access_cookie(response: &mut Response, token: &str, config: &RuntimeAuthConfig) {
    let secure = if config.cookie_secure { "; Secure" } else { "" };
    let domain = if config.cookie_domain.is_empty() {
        String::new()
    } else {
        format!("; Domain={}", config.cookie_domain)
    };
    let cookie = format!(
        "{}_access={token}; HttpOnly{secure}; SameSite=Lax; Path=/{domain}",
        config.cookie_name,
    );
    response
        .headers_mut()
        .append(SET_COOKIE, cookie.parse().unwrap());
}

/// Set the refresh token cookie on the response, scoped to the refresh path.
pub fn set_refresh_cookie(
    response: &mut Response,
    token: &str,
    ks: &str,
    config: &RuntimeAuthConfig,
) {
    let max_age = config.refresh_ttl_secs;
    let secure = if config.cookie_secure { "; Secure" } else { "" };
    let domain = if config.cookie_domain.is_empty() {
        String::new()
    } else {
        format!("; Domain={}", config.cookie_domain)
    };
    let cookie = format!(
        "{}_refresh={token}; HttpOnly{secure}; SameSite=Lax; Path=/auth/{ks}/refresh; Max-Age={max_age}{domain}",
        config.cookie_name,
    );
    response
        .headers_mut()
        .append(SET_COOKIE, cookie.parse().unwrap());
}

/// Clear both access and refresh cookies by setting Max-Age=0.
pub fn clear_cookies(response: &mut Response, ks: &str, config: &RuntimeAuthConfig) {
    let secure = if config.cookie_secure { "; Secure" } else { "" };
    let domain = if config.cookie_domain.is_empty() {
        String::new()
    } else {
        format!("; Domain={}", config.cookie_domain)
    };

    let access_clear = format!(
        "{}_access=; HttpOnly{secure}; SameSite=Lax; Path=/; Max-Age=0{domain}",
        config.cookie_name,
    );
    let refresh_clear = format!(
        "{}_refresh=; HttpOnly{secure}; SameSite=Lax; Path=/auth/{ks}/refresh; Max-Age=0{domain}",
        config.cookie_name,
    );
    response
        .headers_mut()
        .append(SET_COOKIE, access_clear.parse().unwrap());
    response
        .headers_mut()
        .append(SET_COOKIE, refresh_clear.parse().unwrap());
}

/// Extract the access token from the Authorization header or cookie.
pub fn extract_access_token(headers: &HeaderMap, cookie_name: &str) -> Option<String> {
    // Check Authorization: Bearer header first
    if let Some(auth) = headers.get("authorization")
        && let Ok(value) = auth.to_str()
        && let Some(token) = value.strip_prefix("Bearer ")
    {
        return Some(token.to_string());
    }

    // Fall back to cookie
    let cookie_key = format!("{cookie_name}_access=");
    extract_cookie_value(headers, &cookie_key)
}

/// Extract the refresh token from cookies or the Authorization header.
pub fn extract_refresh_token(headers: &HeaderMap, cookie_name: &str) -> Option<String> {
    // Check cookie first for refresh tokens
    let cookie_key = format!("{cookie_name}_refresh=");
    if let Some(value) = extract_cookie_value(headers, &cookie_key) {
        return Some(value);
    }

    // Fall back to Authorization: Bearer header
    if let Some(auth) = headers.get("authorization")
        && let Ok(value) = auth.to_str()
        && let Some(token) = value.strip_prefix("Bearer ")
    {
        return Some(token.to_string());
    }

    None
}

/// Parse a specific cookie value from the Cookie header.
fn extract_cookie_value(headers: &HeaderMap, key: &str) -> Option<String> {
    for cookie_header in headers.get_all(COOKIE) {
        if let Ok(cookie_str) = cookie_header.to_str() {
            for part in cookie_str.split(';') {
                let trimmed = part.trim();
                if let Some(value) = trimmed.strip_prefix(key) {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}
