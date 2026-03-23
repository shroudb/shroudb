//! REST and JWKS adapter for Keyva.
//!
//! HTTP server (axum) providing REST endpoints and `.well-known/jwks.json`.

mod error;
mod json;
mod routes;

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use keyva_protocol::CommandDispatcher;

/// Shared application state for all route handlers.
#[derive(Clone)]
pub struct AppState {
    pub dispatcher: Arc<CommandDispatcher>,
    pub metrics_handle: metrics_exporter_prometheus::PrometheusHandle,
}

/// Builds the axum `Router` with all REST endpoints wired to the dispatcher.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        // Credential operations
        .route("/v1/{keyspace}/issue", post(routes::post_issue))
        .route("/v1/{keyspace}/verify", post(routes::post_verify))
        .route("/v1/{keyspace}/revoke", post(routes::post_revoke))
        .route("/v1/{keyspace}/refresh", post(routes::post_refresh))
        .route("/v1/{keyspace}/update", post(routes::post_update))
        .route("/v1/{keyspace}/suspend", post(routes::post_suspend))
        .route("/v1/{keyspace}/unsuspend", post(routes::post_unsuspend))
        .route(
            "/v1/{keyspace}/inspect/{credential_id}",
            get(routes::get_inspect),
        )
        // Password operations
        .route(
            "/v1/{keyspace}/password/set",
            post(routes::post_password_set),
        )
        .route(
            "/v1/{keyspace}/password/verify",
            post(routes::post_password_verify),
        )
        .route(
            "/v1/{keyspace}/password/change",
            post(routes::post_password_change),
        )
        .route(
            "/v1/{keyspace}/password/import",
            post(routes::post_password_import),
        )
        // Key management
        .route("/v1/{keyspace}/rotate", post(routes::post_rotate))
        .route("/v1/{keyspace}/keys", get(routes::get_keys))
        .route("/v1/{keyspace}/keystate", get(routes::get_keystate))
        .route("/v1/{keyspace}/schema", get(routes::get_schema))
        // Health
        .route("/v1/health", get(routes::get_health))
        .route("/v1/health/{keyspace}", get(routes::get_health_keyspace))
        // Metrics (must be before /{keyspace} catch-all)
        .route("/metrics", get(routes::get_metrics))
        // JWKS well-known endpoint (catch-all {keyspace} at root level — must be last)
        .route("/{keyspace}/.well-known/jwks.json", get(routes::get_jwks))
        .with_state(state)
}
