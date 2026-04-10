use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use shroudb_acl::TokenValidator;
use shroudb_protocol::CommandDispatcher;
use shroudb_store::Store;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::config::ServerConfig;
use crate::connection::handle_connection;

/// Run the TCP server until a shutdown signal is received.
pub async fn run<S: Store + 'static, V: TokenValidator + 'static>(
    config: &ServerConfig,
    dispatcher: Arc<CommandDispatcher<S>>,
    token_validator: Arc<V>,
    auth_required: bool,
    rate_limit_rx: watch::Receiver<Option<u32>>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("binding TCP on {}", config.bind))?;
    tracing::info!(addr = %config.bind, "listening");

    let tls_acceptor = config
        .tls
        .as_ref()
        .map(shroudb_server_tcp::build_tls_acceptor)
        .transpose()
        .context("failed to build TLS acceptor")?;
    if tls_acceptor.is_some() {
        tracing::info!("TLS enabled");
    }

    #[cfg(unix)]
    let uds_listener = if let Some(ref uds_path) = config.unix_socket {
        let _ = std::fs::remove_file(uds_path);
        let l = tokio::net::UnixListener::bind(uds_path)
            .with_context(|| format!("binding UDS on {}", uds_path.display()))?;
        tracing::info!(path = %uds_path.display(), "unix socket listening");
        Some(l)
    } else {
        None
    };

    let mut tasks = JoinSet::new();

    loop {
        tokio::select! {
            biased;

            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    tracing::info!("shutdown signal received, stopping accept loop");
                    break;
                }
            }

            result = listener.accept() => {
                match result {
                    Ok((tcp_stream, peer_addr)) => {
                        tracing::debug!(%peer_addr, "accepted TCP connection");
                        let disp = Arc::clone(&dispatcher);
                        let tv = Arc::clone(&token_validator);
                        let ar = auth_required;
                        let srx = shutdown_rx.clone();
                        let rl = *rate_limit_rx.borrow();
                        if let Some(ref acceptor) = tls_acceptor {
                            let acceptor = acceptor.clone();
                            tasks.spawn(async move {
                                match acceptor.accept(tcp_stream).await {
                                    Ok(tls_stream) => {
                                        handle_connection(tls_stream, disp, tv, ar, srx, rl).await;
                                    }
                                    Err(e) => {
                                        tracing::warn!(%peer_addr, error = %e, "TLS handshake failed");
                                    }
                                }
                            });
                        } else {
                            tasks.spawn(async move {
                                handle_connection(tcp_stream, disp, tv, ar, srx, rl).await;
                            });
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "TCP accept error");
                    }
                }
            }

            result = async {
                #[cfg(unix)]
                {
                    if let Some(ref uds) = uds_listener {
                        return uds.accept().await;
                    }
                }
                std::future::pending().await
            } => {
                #[cfg(unix)]
                match result {
                    Ok((uds_stream, _addr)) => {
                        tracing::debug!("accepted UDS connection");
                        let disp = Arc::clone(&dispatcher);
                        let tv = Arc::clone(&token_validator);
                        let ar = auth_required;
                        let srx = shutdown_rx.clone();
                        let rl = *rate_limit_rx.borrow();
                        tasks.spawn(async move {
                            handle_connection(uds_stream, disp, tv, ar, srx, rl).await;
                        });
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "UDS accept error");
                    }
                }
                #[cfg(not(unix))]
                let _ = result;
            }
        }
    }

    tracing::info!(
        pending = tasks.len(),
        "draining in-flight connections (30s timeout)"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !tasks.is_empty() {
        tokio::select! {
            _ = tasks.join_next() => {}
            _ = tokio::time::sleep_until(deadline) => {
                tracing::warn!(remaining = tasks.len(), "drain timeout, aborting remaining connections");
                tasks.abort_all();
                break;
            }
        }
    }

    tracing::info!("server stopped");
    Ok(())
}
