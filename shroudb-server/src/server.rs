use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use shroudb_acl::TokenValidator;
use shroudb_protocol::CommandDispatcher;
use shroudb_store::Store;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;

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

    let tls_acceptor = build_tls_acceptor(config)?;
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

fn build_tls_acceptor(config: &ServerConfig) -> anyhow::Result<Option<TlsAcceptor>> {
    let (cert_path, key_path) = match (&config.tls_cert, &config.tls_key) {
        (Some(c), Some(k)) => (c, k),
        (None, None) => return Ok(None),
        _ => anyhow::bail!("both tls_cert and tls_key must be set (or neither)"),
    };

    let cert_pem = std::fs::read(cert_path)
        .with_context(|| format!("reading TLS cert: {}", cert_path.display()))?;
    let key_pem = std::fs::read(key_path)
        .with_context(|| format!("reading TLS key: {}", key_path.display()))?;

    use rustls_pki_types::pem::PemObject;
    let certs: Vec<rustls_pki_types::CertificateDer<'static>> =
        rustls_pki_types::CertificateDer::pem_slice_iter(&cert_pem)
            .collect::<Result<Vec<_>, _>>()
            .context("parsing TLS certificates")?;
    let key = rustls_pki_types::PrivateKeyDer::from_pem_slice(&key_pem)
        .context("parsing TLS private key")?;

    let tls_config = if let Some(ref ca_path) = config.tls_client_ca {
        let ca_pem = std::fs::read(ca_path)
            .with_context(|| format!("reading client CA cert: {}", ca_path.display()))?;
        let ca_certs: Vec<rustls_pki_types::CertificateDer<'static>> =
            rustls_pki_types::CertificateDer::pem_slice_iter(&ca_pem)
                .collect::<Result<Vec<_>, _>>()
                .context("parsing client CA certificates")?;
        let mut root_store = rustls::RootCertStore::empty();
        for cert in ca_certs {
            root_store
                .add(cert)
                .context("adding client CA certificate to root store")?;
        }
        let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(root_store))
            .build()
            .context("building mTLS client verifier")?;
        tracing::info!("mTLS enabled (client certificate required)");
        rustls::ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, key)
            .context("building TLS server config with mTLS")?
    } else {
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .context("building TLS server config")?
    };

    Ok(Some(TlsAcceptor::from(Arc::new(tls_config))))
}
