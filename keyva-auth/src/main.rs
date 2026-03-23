//! Keyva Auth — standalone authentication server with embedded Keyva engine.
//!
//! Binary entry point: CLI argument parsing, config loading, engine startup,
//! keyspace registration, and HTTP server.

mod config;
mod cookies;
mod cors;
mod csrf;
mod rate_limit;
mod routes;
mod state;

use std::sync::Arc;

use axum::Router;
use axum::middleware;
use axum::routing::{get, post};
use clap::Parser;
use keyva_crypto::SecretBytes;
use keyva_protocol::{AuthRegistry, Command, CommandDispatcher, CommandResponse};
use keyva_storage::{ChainedMasterKeySource, MasterKeySource, StorageEngine};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::state::AppState;

#[derive(Parser)]
#[command(
    name = "keyva-auth",
    about = "Standalone auth server with embedded Keyva engine",
    version
)]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(long, default_value = "auth.toml")]
    config: std::path::PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 0. Disable core dumps (Linux only).
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0);
    }

    // 1. Parse CLI arguments.
    let cli = Cli::parse();

    // 2. Load configuration (or use defaults).
    let cfg = match config::load(&cli.config)? {
        Some(cfg) => {
            // Config file found — use JSON tracing for production.
            let data_dir = &cfg.storage.data_dir;
            std::fs::create_dir_all(data_dir)?;

            let env_filter = tracing_subscriber::EnvFilter::from_default_env();
            let console_layer = tracing_subscriber::fmt::layer()
                .json()
                .with_filter(env_filter);

            tracing_subscriber::registry().with(console_layer).init();

            tracing::info!(config = %cli.config.display(), "configuration loaded");
            cfg
        }
        None => {
            // No config file — dev mode with human-readable logs.
            let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
            let console_layer = tracing_subscriber::fmt::layer().with_filter(env_filter);

            tracing_subscriber::registry().with(console_layer).init();

            tracing::info!("no config file found, starting with defaults");
            config::AuthServerConfig::default()
        }
    };

    // 3. Resolve runtime auth config.
    let runtime_config = config::to_runtime_config(&cfg.auth)?;

    // 4. Resolve master key source.
    let key_source = resolve_master_key()?;

    // 5. Convert storage section to engine config and open engine (WAL recovery).
    std::fs::create_dir_all(&cfg.storage.data_dir)?;
    let engine_config = config::to_engine_config(&cfg);
    let engine = StorageEngine::open(engine_config, &*key_source).await?;
    let engine = Arc::new(engine);
    tracing::info!("storage engine ready");

    // 6. Register keyspaces from config.
    for (name, ks_config) in &cfg.keyspaces {
        let keyspaces = config::build_auth_keyspaces(
            name,
            ks_config,
            runtime_config.access_ttl_secs,
            runtime_config.refresh_ttl_secs,
        )?;
        for keyspace in keyspaces {
            let ks_type = keyspace.keyspace_type;
            let ks_name = keyspace.name.clone();
            engine.index().keyspaces.insert(ks_name.clone(), keyspace);
            engine.index().ensure_keyspace(&ks_name, ks_type);
            tracing::info!(keyspace = %ks_name, r#type = ?ks_type, "registered keyspace");
        }
    }

    // 7. Build auth registry (permissive — auth is handled by the auth routes)
    //    and command dispatcher.
    let auth_registry = Arc::new(AuthRegistry::permissive());
    let dispatcher = Arc::new(CommandDispatcher::new(
        Arc::clone(&engine),
        Arc::clone(&auth_registry),
    ));

    // 8. Create initial signing keys for JWT keyspaces if they don't exist.
    for name in cfg.keyspaces.keys() {
        let access_ks = format!("{name}_access");
        ensure_signing_key(&dispatcher, &access_ks).await?;
    }

    // 9. Install Prometheus metrics recorder.
    let metrics_handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install metrics recorder");

    // 10. Build application state and router.
    let app_state = AppState {
        dispatcher,
        config: runtime_config,
        metrics_handle,
    };

    // Build CSRF middleware state.
    let csrf_config = csrf::CsrfConfig::new(&cfg.auth.cors_origins);

    // Rate-limited routes (signup + login — expensive argon2id + credential stuffing target).
    // Only applied when explicitly configured; dev mode (no config) runs without rate limits.
    let has_rate_limit =
        cfg.auth.login_rate_limit_burst.is_some() || cfg.auth.login_rate_limit_per_sec.is_some();

    let rate_limited_routes = if has_rate_limit {
        let rate_limit_config = rate_limit::RateLimitConfig {
            max_tokens: cfg.auth.login_rate_limit_burst.unwrap_or(10) as f64,
            refill_rate: cfg.auth.login_rate_limit_per_sec.unwrap_or(2) as f64,
        };
        let rate_limit_state = rate_limit::RateLimitState::new(rate_limit_config);
        tracing::info!(
            burst = rate_limit_config.max_tokens,
            per_sec = rate_limit_config.refill_rate,
            "per-IP rate limiting enabled on signup/login"
        );

        Router::new()
            .route("/auth/{ks}/signup", post(routes::signup))
            .route("/auth/{ks}/login", post(routes::login))
            .route_layer(middleware::from_fn_with_state(
                rate_limit_state,
                rate_limit::rate_limit_middleware,
            ))
            .with_state(app_state.clone())
    } else {
        Router::new()
            .route("/auth/{ks}/signup", post(routes::signup))
            .route("/auth/{ks}/login", post(routes::login))
            .with_state(app_state.clone())
    };

    let router = Router::new()
        .merge(rate_limited_routes)
        // Auth endpoints (not rate-limited)
        .route("/auth/{ks}/session", get(routes::session))
        .route("/auth/{ks}/refresh", post(routes::refresh))
        .route("/auth/{ks}/logout", post(routes::logout))
        .route("/auth/{ks}/change-password", post(routes::change_password))
        // Phase 3: Extended flows
        .route("/auth/{ks}/forgot-password", post(routes::forgot_password))
        .route("/auth/{ks}/reset-password", post(routes::reset_password))
        .route("/auth/{ks}/logout-all", post(routes::logout_all))
        .route("/auth/{ks}/sessions", get(routes::sessions))
        // JWKS
        .route("/auth/{ks}/.well-known/jwks.json", get(routes::jwks))
        // Health and metrics
        .route("/auth/health", get(routes::health))
        .route("/metrics", get(routes::metrics))
        .with_state(app_state)
        // CSRF protection on all POST routes (state-independent middleware)
        .layer(middleware::from_fn_with_state(
            csrf_config,
            csrf::csrf_middleware,
        ))
        .layer(cors::cors_layer(&cfg.auth.cors_origins));

    // 11. Set up shutdown signal.
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = shutdown_tx.send(true);
    });

    // 12. Start server.
    let listener = tokio::net::TcpListener::bind(cfg.server.bind).await?;
    tracing::info!(bind = %cfg.server.bind, "keyva-auth ready");

    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = shutdown_rx.wait_for(|&v| v).await;
    })
    .await?;

    // 13. Shut down storage engine.
    engine.shutdown().await?;
    tracing::info!("keyva-auth shut down cleanly");

    Ok(())
}

/// Ensure a JWT keyspace has at least one signing key by running ROTATE FORCE.
async fn ensure_signing_key(dispatcher: &CommandDispatcher, keyspace: &str) -> anyhow::Result<()> {
    // Check if the keyspace already has signing keys via KEYSTATE.
    let keystate_cmd = Command::KeyState {
        keyspace: keyspace.to_string(),
    };
    let result = dispatcher.execute(keystate_cmd, None).await;

    let needs_key = match result {
        CommandResponse::Success(ref map) => {
            // If there are no active keys, we need to create one
            let mut has_active = false;
            for (key, value) in &map.fields {
                if (key == "active_count" || key == "total")
                    && let keyva_protocol::ResponseValue::Integer(n) = value
                    && *n > 0
                {
                    has_active = true;
                }
                if key == "keys"
                    && let keyva_protocol::ResponseValue::Array(arr) = value
                    && !arr.is_empty()
                {
                    has_active = true;
                }
            }
            !has_active
        }
        // If KEYSTATE fails (e.g., no keys yet), we need to create one
        _ => true,
    };

    if needs_key {
        tracing::info!(keyspace, "no signing keys found, running ROTATE FORCE");
        let rotate_cmd = Command::Rotate {
            keyspace: keyspace.to_string(),
            force: true,
            nowait: false,
            dryrun: false,
        };
        let result = dispatcher.execute(rotate_cmd, None).await;
        if let CommandResponse::Error(ref e) = result {
            anyhow::bail!("failed to create initial signing key for {keyspace}: {e}");
        }
        tracing::info!(keyspace, "initial signing key created");
    }

    Ok(())
}

/// Resolve the master key source: try env/file first, fall back to ephemeral.
fn resolve_master_key() -> anyhow::Result<Box<dyn MasterKeySource>> {
    if std::env::var("KEYVA_MASTER_KEY").is_ok() || std::env::var("KEYVA_MASTER_KEY_FILE").is_ok() {
        return Ok(Box::new(ChainedMasterKeySource::default_chain()));
    }

    tracing::warn!(
        "no master key configured (set KEYVA_MASTER_KEY or KEYVA_MASTER_KEY_FILE for persistence)"
    );
    tracing::warn!("using ephemeral master key — data will NOT survive restart");
    Ok(Box::new(EphemeralMasterKey::generate()))
}

/// An ephemeral in-memory master key for development mode.
struct EphemeralMasterKey {
    key: SecretBytes,
}

impl EphemeralMasterKey {
    fn generate() -> Self {
        use ring::rand::{SecureRandom, SystemRandom};
        let rng = SystemRandom::new();
        let mut bytes = vec![0u8; 32];
        rng.fill(&mut bytes).expect("CSPRNG fill failed");
        Self {
            key: SecretBytes::new(bytes),
        }
    }
}

impl MasterKeySource for EphemeralMasterKey {
    fn load(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<SecretBytes, keyva_storage::StorageError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async { Ok(self.key.clone()) })
    }

    fn source_name(&self) -> &str {
        "ephemeral"
    }
}

/// Wait for either SIGINT (Ctrl-C) or SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => { tracing::info!("received SIGINT"); }
            _ = sigterm.recv() => { tracing::info!("received SIGTERM"); }
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.expect("failed to listen for Ctrl-C");
        tracing::info!("received SIGINT");
    }
}
