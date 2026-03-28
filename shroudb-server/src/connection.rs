use std::sync::Arc;
use std::time::Instant;

use metrics::gauge;
use shroudb_acl::{AuthContext, TokenValidator};
use shroudb_protocol::resp3::Resp3Frame;
use shroudb_protocol::resp3::parse_command::parse_command;
use shroudb_protocol::resp3::reader::read_frame;
use shroudb_protocol::resp3::serialize::response_to_frame;
use shroudb_protocol::resp3::writer::write_frame;
use shroudb_protocol::response::{ResponseMap, ResponseValue};
use shroudb_protocol::{CommandDispatcher, CommandResponse};
use shroudb_store::{EventType, Store, SubscriptionEvent, SubscriptionFilter};
use tokio::io::{AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::watch;

/// RAII guard that decrements the concurrent connections gauge on drop.
struct ConnectionGuard;

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        gauge!("shroudb_concurrent_connections").decrement(1.0);
    }
}

/// Simple token-bucket rate limiter for per-connection command throttling.
struct RateLimiter {
    tokens: f64,
    max_tokens: f64,
    refill_rate: f64,
    last_refill: Instant,
}

impl RateLimiter {
    fn new(max_tokens: f64, refill_rate: f64) -> Self {
        Self {
            tokens: 1.0, // Start with 1 token, not max — prevents burst after idle
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
pub async fn handle_connection<S: Store, V: TokenValidator>(
    stream: impl tokio::io::AsyncRead + AsyncWrite + Unpin + Send + 'static,
    dispatcher: Arc<CommandDispatcher<S>>,
    token_validator: Arc<V>,
    auth_required: bool,
    mut shutdown_rx: watch::Receiver<bool>,
    rate_limit: Option<u32>,
) {
    gauge!("shroudb_concurrent_connections").increment(1.0);
    let _conn_guard = ConnectionGuard;

    let (reader_half, writer_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader_half);
    let mut writer = BufWriter::new(writer_half);

    // Per-connection auth state.
    // When auth is not required, give every connection a platform context
    // so ACL checks in the dispatcher pass.
    let mut auth: Option<AuthContext> = if auth_required {
        None
    } else {
        Some(AuthContext::platform("default", "anonymous"))
    };

    // Per-connection rate limiter
    let mut rate_limiter = rate_limit.map(|limit| RateLimiter::new(limit as f64, limit as f64));

    loop {
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
                tracing::debug!("client disconnected (EOF)");
                break;
            }
            Err(e) => {
                tracing::warn!(error = %e, "protocol error reading frame");
                let err_frame = Resp3Frame::SimpleError(format!("ERR protocol: {e}"));
                if let Err(we) = write_frame(&mut writer, &err_frame).await {
                    tracing::debug!(error = %we, "failed to write error response");
                }
                let _ = writer.flush().await;
                break;
            }
        };

        let command = match parse_command(frame) {
            Ok(cmd) => cmd,
            Err(e) => {
                let err_frame = Resp3Frame::SimpleError(format!("ERR {e}"));
                if write_frame(&mut writer, &err_frame).await.is_err() {
                    break;
                }
                let _ = writer.flush().await;
                continue;
            }
        };

        // Rate limit check
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
            let _ = writer.flush().await;
            continue;
        }

        // Handle AUTH at connection level
        if let shroudb_protocol::Command::Auth { ref token } = command {
            let response = match token_validator.validate(token) {
                Ok(parsed_token) => {
                    let ctx = parsed_token.into_context();
                    let actor = ctx.actor.clone();
                    auth = Some(ctx);
                    CommandResponse::Success(
                        ResponseMap::ok().with("actor", ResponseValue::String(actor)),
                    )
                }
                Err(e) => CommandResponse::Error(shroudb_protocol::CommandError::Denied {
                    reason: e.to_string(),
                }),
            };
            let response_frame = response_to_frame(&response);
            if write_frame(&mut writer, &response_frame).await.is_err() {
                break;
            }
            let _ = writer.flush().await;
            continue;
        }

        // Pre-auth: only PING and AUTH are allowed
        if auth_required && auth.is_none() && !matches!(command, shroudb_protocol::Command::Ping) {
            let response = CommandResponse::Error(shroudb_protocol::CommandError::NotAuthenticated);
            let response_frame = response_to_frame(&response);
            if write_frame(&mut writer, &response_frame).await.is_err() {
                break;
            }
            let _ = writer.flush().await;
            continue;
        }

        // Check token expiry — fail closed on clock error
        if let Some(ref ctx) = auth {
            let now = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
                Ok(d) => d.as_secs(),
                Err(_) => {
                    let response = CommandResponse::Error(
                        shroudb_protocol::CommandError::Internal("system clock error".into()),
                    );
                    let response_frame = response_to_frame(&response);
                    let _ = write_frame(&mut writer, &response_frame).await;
                    let _ = writer.flush().await;
                    break;
                }
            };
            if ctx.is_expired(now) {
                auth = None;
                let response =
                    CommandResponse::Error(shroudb_protocol::CommandError::NotAuthenticated);
                let response_frame = response_to_frame(&response);
                if write_frame(&mut writer, &response_frame).await.is_err() {
                    break;
                }
                let _ = writer.flush().await;
                continue;
            }
        }

        // Handle SUBSCRIBE at connection level — enters streaming mode
        if let shroudb_protocol::Command::Subscribe {
            ref ns,
            ref key,
            ref events,
        } = command
        {
            // ACL check
            let requirement = command.acl_requirement();
            if let Some(ref ctx) = auth {
                if let Err(e) = ctx.check(&requirement) {
                    let response = CommandResponse::Error(shroudb_protocol::CommandError::Denied {
                        reason: e.to_string(),
                    });
                    let response_frame = response_to_frame(&response);
                    if write_frame(&mut writer, &response_frame).await.is_err() {
                        break;
                    }
                    let _ = writer.flush().await;
                    continue;
                }
            } else if auth_required {
                let response =
                    CommandResponse::Error(shroudb_protocol::CommandError::NotAuthenticated);
                let response_frame = response_to_frame(&response);
                if write_frame(&mut writer, &response_frame).await.is_err() {
                    break;
                }
                let _ = writer.flush().await;
                continue;
            }

            // Parse event type filters
            let event_filter: Vec<EventType> = events
                .iter()
                .filter_map(|e| match e.to_ascii_lowercase().as_str() {
                    "put" => Some(EventType::Put),
                    "delete" => Some(EventType::Delete),
                    _ => None,
                })
                .collect();

            let filter = SubscriptionFilter {
                key: key.clone(),
                events: event_filter,
            };

            // Subscribe via the store
            let mut subscription = match dispatcher.store().subscribe(ns, filter).await {
                Ok(sub) => sub,
                Err(e) => {
                    let response = CommandResponse::Error(shroudb_protocol::CommandError::Store(e));
                    let response_frame = response_to_frame(&response);
                    if write_frame(&mut writer, &response_frame).await.is_err() {
                        break;
                    }
                    let _ = writer.flush().await;
                    continue;
                }
            };

            // Send OK to confirm subscription is active
            let ok_response = CommandResponse::Success(
                ResponseMap::ok().with("namespace", ResponseValue::String(ns.clone())),
            );
            if write_frame(&mut writer, &response_to_frame(&ok_response))
                .await
                .is_err()
            {
                break;
            }
            let _ = writer.flush().await;

            // Enter streaming loop: read events from subscription + commands from client
            let exited_cleanly = handle_subscription(
                &mut reader,
                &mut writer,
                &mut subscription,
                &mut shutdown_rx,
            )
            .await;

            if !exited_cleanly {
                break;
            }
            // UNSUBSCRIBE received — fall through to resume normal command loop
            continue;
        }

        // Dispatch
        let response = dispatcher.execute(command, auth.as_ref()).await;

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

/// Subscription streaming loop. Returns `true` if the client cleanly unsubscribed
/// (so the caller should resume normal command processing), or `false` if the
/// connection should be closed.
async fn handle_subscription<S: shroudb_store::Subscription>(
    reader: &mut BufReader<impl tokio::io::AsyncRead + Unpin + Send>,
    writer: &mut BufWriter<impl AsyncWrite + Unpin + Send>,
    subscription: &mut S,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> bool {
    loop {
        tokio::select! {
            biased;

            // Shutdown signal
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    return false;
                }
            }

            // Next event from subscription
            event = subscription.recv() => {
                match event {
                    Some(ev) => {
                        let frame = subscription_event_to_push_frame(&ev);
                        if write_frame(writer, &frame).await.is_err() {
                            return false;
                        }
                        if writer.flush().await.is_err() {
                            return false;
                        }
                    }
                    None => {
                        // Subscription closed (engine shutdown)
                        let frame = Resp3Frame::Push(vec![
                            Resp3Frame::SimpleString("subscription".into()),
                            Resp3Frame::SimpleString("closed".into()),
                        ]);
                        let _ = write_frame(writer, &frame).await;
                        let _ = writer.flush().await;
                        return true;
                    }
                }
            }

            // Client sent a command while subscribed (only UNSUBSCRIBE and PING are valid)
            result = read_frame(reader) => {
                match result {
                    Ok(Some(frame)) => {
                        match parse_command(frame) {
                            Ok(shroudb_protocol::Command::Unsubscribe) => {
                                let response = CommandResponse::Success(ResponseMap::ok());
                                let response_frame = response_to_frame(&response);
                                let _ = write_frame(writer, &response_frame).await;
                                let _ = writer.flush().await;
                                return true;
                            }
                            Ok(shroudb_protocol::Command::Ping) => {
                                let response = CommandResponse::Success(
                                    ResponseMap::ok()
                                        .with("message", ResponseValue::String("PONG".into())),
                                );
                                let _ = write_frame(writer, &response_to_frame(&response)).await;
                                let _ = writer.flush().await;
                            }
                            Ok(_) => {
                                let err = Resp3Frame::SimpleError(
                                    "ERR only UNSUBSCRIBE and PING allowed during subscription".into(),
                                );
                                let _ = write_frame(writer, &err).await;
                                let _ = writer.flush().await;
                            }
                            Err(_) => {
                                // Parse error during subscription — ignore
                            }
                        }
                    }
                    Ok(None) => {
                        // Client disconnected
                        return false;
                    }
                    Err(_) => {
                        // Protocol error
                        return false;
                    }
                }
            }
        }
    }
}

/// Convert a subscription event into a RESP3 push frame.
fn subscription_event_to_push_frame(event: &SubscriptionEvent) -> Resp3Frame {
    let event_str = match event.event {
        EventType::Put => "put",
        EventType::Delete => "delete",
    };
    Resp3Frame::Push(vec![
        Resp3Frame::SimpleString("subscribe".into()),
        Resp3Frame::SimpleString(event_str.into()),
        Resp3Frame::BulkString(event.namespace.as_bytes().to_vec()),
        Resp3Frame::BulkString(event.key.clone()),
        Resp3Frame::Integer(event.version as i64),
        Resp3Frame::BulkString(event.actor.as_bytes().to_vec()),
        Resp3Frame::Integer(event.timestamp as i64),
    ])
}
