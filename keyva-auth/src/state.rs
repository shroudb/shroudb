//! Shared application state for the auth server.

use std::sync::Arc;

use keyva_protocol::CommandDispatcher;
use metrics_exporter_prometheus::PrometheusHandle;

use crate::config::RuntimeAuthConfig;

/// Application state shared across all route handlers.
#[derive(Clone)]
pub struct AppState {
    pub dispatcher: Arc<CommandDispatcher>,
    pub config: RuntimeAuthConfig,
    pub metrics_handle: PrometheusHandle,
}
