use std::sync::Arc;
use std::time::Instant;

use metrics::gauge;
use shroudb_protocol::auth::AuthPolicy;
use shroudb_protocol::resp3::Resp3Frame;
use shroudb_protocol::resp3::parse_command::parse_command;
use shroudb_protocol::resp3::reader::read_frame;
use shroudb_protocol::resp3::serialize::response_to_frame;
use shroudb_protocol::resp3::writer::write_frame;
use shroudb_protocol::response::{ResponseMap, ResponseValue};
use shroudb_protocol::{CommandDispatcher, CommandResponse};
use tokio::io::{AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::broadcast;
use tokio::sync::watch;

/// RAII guard that decrements the concurrent connections gauge on drop.
struct ConnectionGuard;

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        gauge!("shroudb_concurrent_connections").decrement(1.0);
    }
}

/// Simple token-bucket rate limiter for per-connection command throttling.
///
/// Future: for multi-tenant deployments, rate limiting will key on
/// (tenant_id, source_ip) rather than just connection. The rate limiter
/// interface will need to move to a shared pool keyed by tenant.
struct RateLimiter {
    tokens: f64,
    max_tokens: f64,
    refill_rate: f64, // tokens per second
    last_refill: Instant,
}

impl RateLimiter {
    fn new(max_tokens: f64, refill_rate: f64) -> Self {
        Self {
            tokens: max_tokens,
            max_tokens,
            refill_rate,
            last_refill: Instant::now(),
        }
    }

    fn try_acquire(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);
        self.last_refill = now;

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Handle a single client connection: read frames, dispatch commands, write responses.
pub async fn handle_connection(
    stream: impl tokio::io::AsyncRead + AsyncWrite + Unpin + Send + 'static,
    dispatcher: Arc<CommandDispatcher>,
    mut shutdown_rx: watch::Receiver<bool>,
    rate_limit: Option<u32>,
) {
    gauge!("shroudb_concurrent_connections").increment(1.0);
    let _conn_guard = ConnectionGuard;

    let (reader_half, writer_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader_half);
    let mut writer = BufWriter::new(writer_half);

    // Per-connection auth state
    let mut auth: Option<AuthPolicy> = None;

    // Per-connection rate limiter (if configured)
    let mut rate_limiter = rate_limit.map(|limit| RateLimiter::new(limit as f64, limit as f64));

    loop {
        // Check shutdown before blocking on the next frame.
        let frame = tokio::select! {
            biased;
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    tracing::debug!("connection shutting down by signal");
                    break;
                }
                continue;
            }
            result = read_frame(&mut reader) => result,
        };

        let frame = match frame {
            Ok(Some(f)) => f,
            Ok(None) => {
                // Clean EOF
                tracing::debug!("client disconnected (EOF)");
                break;
            }
            Err(e) => {
                tracing::warn!(error = %e, "protocol error reading frame");
                // Send an error back if possible, then close.
                let err_frame =
                    shroudb_protocol::Resp3Frame::SimpleError(format!("ERR protocol: {e}"));
                let _ = write_frame(&mut writer, &err_frame).await;
                let _ = writer.flush().await;
                break;
            }
        };

        // Parse the frame into a Command.
        let command = match parse_command(frame) {
            Ok(cmd) => cmd,
            Err(e) => {
                let err_frame = shroudb_protocol::Resp3Frame::SimpleError(format!("ERR {e}"));
                if write_frame(&mut writer, &err_frame).await.is_err() {
                    break;
                }
                if writer.flush().await.is_err() {
                    break;
                }
                continue;
            }
        };

        // Check rate limit before dispatching.
        if let Some(ref mut limiter) = rate_limiter
            && !limiter.try_acquire()
        {
            let response = CommandResponse::Error(shroudb_protocol::CommandError::Denied {
                reason: "rate limit exceeded".into(),
            });
            let response_frame = response_to_frame(&response);
            if write_frame(&mut writer, &response_frame).await.is_err() {
                break;
            }
            if writer.flush().await.is_err() {
                break;
            }
            continue;
        }

        // Handle AUTH command at the connection level
        if let shroudb_protocol::Command::Auth { ref token } = command {
            let response = match dispatcher.auth_registry().authenticate(token) {
                Ok(policy) => {
                    auth = Some(policy.clone());
                    CommandResponse::Success(
                        ResponseMap::ok()
                            .with("policy", ResponseValue::String(policy.name.clone())),
                    )
                }
                Err(e) => CommandResponse::Error(e),
            };
            let response_frame = response_to_frame(&response);
            if write_frame(&mut writer, &response_frame).await.is_err() {
                tracing::debug!("write error, closing connection");
                break;
            }
            if writer.flush().await.is_err() {
                tracing::debug!("flush error, closing connection");
                break;
            }
            continue;
        }

        // Handle SUBSCRIBE at the connection level — enter subscription mode.
        if let shroudb_protocol::Command::Subscribe { ref channel } = command {
            // Send an initial confirmation.
            let confirm = CommandResponse::Success(
                ResponseMap::ok()
                    .with(
                        "message",
                        ResponseValue::String(format!("subscribed to {channel}")),
                    )
                    .with("channel", ResponseValue::String(channel.clone())),
            );
            let confirm_frame = response_to_frame(&confirm);
            if write_frame(&mut writer, &confirm_frame).await.is_err() {
                break;
            }
            if writer.flush().await.is_err() {
                break;
            }

            // Enter subscription streaming loop.
            let channel = channel.clone();
            handle_subscription(&mut writer, &dispatcher, &mut shutdown_rx, &channel).await;

            // After subscription ends, go back to the command loop.
            continue;
        }

        // Execute the command.
        let response = dispatcher.execute(command, auth.as_ref()).await;

        // Serialize and write the response frame.
        let response_frame = response_to_frame(&response);
        if write_frame(&mut writer, &response_frame).await.is_err() {
            tracing::debug!("write error, closing connection");
            break;
        }
        if writer.flush().await.is_err() {
            tracing::debug!("flush error, closing connection");
            break;
        }
    }
}

/// Handle subscription mode: stream lifecycle events to the client until
/// shutdown or disconnect.
async fn handle_subscription(
    writer: &mut (impl AsyncWrite + Unpin),
    dispatcher: &CommandDispatcher,
    shutdown_rx: &mut watch::Receiver<bool>,
    channel: &str,
) {
    let mut rx = dispatcher.event_bus().subscribe();

    loop {
        tokio::select! {
            biased;
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    tracing::debug!("subscription ending due to shutdown");
                    break;
                }
            }
            event = rx.recv() => {
                match event {
                    Ok(evt) if evt.keyspace == channel || channel == "*" => {
                        // Encode the event as a RESP3 array:
                        // ["event", event_type, keyspace, detail, timestamp]
                        let frame = Resp3Frame::Array(vec![
                            Resp3Frame::BulkString(b"event".to_vec()),
                            Resp3Frame::BulkString(evt.event_type.into_bytes()),
                            Resp3Frame::BulkString(evt.keyspace.into_bytes()),
                            Resp3Frame::BulkString(evt.detail.into_bytes()),
                            Resp3Frame::Integer(evt.timestamp as i64),
                        ]);
                        if write_frame(writer, &frame).await.is_err() {
                            tracing::debug!("write error in subscription, ending");
                            break;
                        }
                        if tokio::io::AsyncWriteExt::flush(writer).await.is_err() {
                            tracing::debug!("flush error in subscription, ending");
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(lagged = n, "subscription lagged, some events dropped");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::debug!("event bus closed, ending subscription");
                        break;
                    }
                    _ => continue, // Event for a different channel
                }
            }
        }
    }
}
