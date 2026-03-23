//! Per-IP rate limiting for login and signup endpoints.
//!
//! Token bucket algorithm keyed by client IP address. Applied as axum
//! middleware on routes that are expensive (argon2id hashing) or
//! security-sensitive (credential stuffing).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Instant;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Rate limiter state shared across requests.
#[derive(Clone)]
pub struct RateLimitState {
    inner: std::sync::Arc<Mutex<RateLimitInner>>,
    config: RateLimitConfig,
}

#[derive(Clone, Copy)]
pub struct RateLimitConfig {
    /// Maximum burst size (tokens).
    pub max_tokens: f64,
    /// Tokens added per second.
    pub refill_rate: f64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_tokens: 10.0,
            refill_rate: 1.0, // 1 request/sec sustained, burst of 10
        }
    }
}

struct RateLimitInner {
    buckets: HashMap<IpAddr, TokenBucket>,
    last_prune: Instant,
}

struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
}

impl RateLimitState {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            inner: std::sync::Arc::new(Mutex::new(RateLimitInner {
                buckets: HashMap::new(),
                last_prune: Instant::now(),
            })),
            config,
        }
    }

    /// Try to acquire one token for the given IP. Returns true if allowed.
    fn try_acquire(&self, ip: IpAddr) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let now = Instant::now();

        // Prune stale buckets every 60 seconds
        if now.duration_since(inner.last_prune).as_secs() > 60 {
            let stale_threshold = now - std::time::Duration::from_secs(120);
            inner.buckets.retain(|_, b| b.last_refill > stale_threshold);
            inner.last_prune = now;
        }

        let bucket = inner.buckets.entry(ip).or_insert(TokenBucket {
            tokens: self.config.max_tokens,
            last_refill: now,
        });

        // Refill tokens based on elapsed time
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens =
            (bucket.tokens + elapsed * self.config.refill_rate).min(self.config.max_tokens);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Axum middleware that enforces per-IP rate limits.
///
/// Must be applied after `ConnectInfo` is available (via `axum::serve` which
/// provides it automatically, or via `Router::into_make_service_with_connect_info`).
pub async fn rate_limit_middleware(
    State(state): State<RateLimitState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    // Extract client IP from ConnectInfo or X-Forwarded-For
    let ip = extract_client_ip(&request);

    if let Some(ip) = ip
        && !state.try_acquire(ip)
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            axum::Json(serde_json::json!({"error": "rate limit exceeded"})),
        )
            .into_response();
    }

    next.run(request).await
}

use axum::extract::State;

/// Extract the client IP, preferring X-Forwarded-For (for reverse proxy setups),
/// falling back to the direct connection IP.
fn extract_client_ip(request: &Request<Body>) -> Option<IpAddr> {
    // Check X-Forwarded-For first (leftmost = original client)
    if let Some(xff) = request
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        && let Some(first) = xff.split(',').next()
        && let Ok(ip) = first.trim().parse::<IpAddr>()
    {
        return Some(ip);
    }

    // Fall back to ConnectInfo
    request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip())
}
