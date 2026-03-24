use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use metrics::{counter, histogram};

use shroudb_storage::{HealthState, StorageEngine};

use crate::auth::{self, AuthPolicy, AuthRegistry};
use crate::command::Command;
use crate::error::CommandError;
use crate::events::{EventBus, LifecycleEvent};
use crate::handlers;
use crate::idempotency::IdempotencyMap;
use crate::response::{CommandResponse, ResponseMap, ResponseValue};

/// Routes parsed commands to the appropriate handler.
pub struct CommandDispatcher {
    engine: Arc<StorageEngine>,
    idempotency: IdempotencyMap,
    auth_registry: Arc<AuthRegistry>,
    event_bus: Arc<EventBus>,
}

impl CommandDispatcher {
    pub fn new(engine: Arc<StorageEngine>, auth_registry: Arc<AuthRegistry>) -> Self {
        Self {
            engine,
            idempotency: IdempotencyMap::new(Duration::from_secs(300)),
            auth_registry,
            event_bus: Arc::new(EventBus::new(1024)),
        }
    }

    /// Returns a reference to the auth registry.
    pub fn auth_registry(&self) -> &AuthRegistry {
        &self.auth_registry
    }

    /// Returns a reference to the underlying storage engine.
    pub fn engine(&self) -> &StorageEngine {
        &self.engine
    }

    /// Returns a reference to the event bus for subscribing to lifecycle events.
    pub fn event_bus(&self) -> &Arc<EventBus> {
        &self.event_bus
    }

    pub async fn execute(&self, cmd: Command, auth: Option<&AuthPolicy>) -> CommandResponse {
        // Handle pipeline recursively (boxed to avoid infinite future size)
        if let Command::Pipeline(commands) = cmd {
            let mut results = Vec::with_capacity(commands.len());
            for c in commands {
                results.push(Box::pin(self.execute(c, auth)).await);
            }
            return CommandResponse::Array(results);
        }

        // Auth and Health bypass auth checks
        if !matches!(cmd, Command::Auth { .. } | Command::Health { .. })
            && self.auth_registry.is_required()
        {
            match auth {
                None => {
                    return CommandResponse::Error(CommandError::Denied {
                        reason: "authentication required".into(),
                    });
                }
                Some(policy) => {
                    if let Err(e) = policy.check(&cmd) {
                        return CommandResponse::Error(e);
                    }
                }
            }
        }

        // Check engine health (allow Health commands through)
        if !matches!(cmd, Command::Health { .. }) && self.engine.health() != HealthState::Ready {
            return CommandResponse::Error(CommandError::NotReady(
                self.engine.health().to_string(),
            ));
        }

        // Look up keyspace if the command has one
        let keyspace = if let Some(ks_name) = cmd.keyspace() {
            match self.engine.index().keyspaces.get(ks_name) {
                Some(ks_ref) => {
                    let ks = ks_ref.value().clone();
                    // Check keyspace not disabled (except for Health/Schema/KeyState/Jwks which are read-only)
                    if ks.disabled && !cmd.is_read() {
                        return CommandResponse::Error(CommandError::Disabled {
                            keyspace: ks_name.to_string(),
                        });
                    }
                    Some(ks)
                }
                None => {
                    return CommandResponse::Error(CommandError::NotFound {
                        entity: "keyspace".into(),
                        id: ks_name.to_string(),
                    });
                }
            }
        } else {
            None
        };

        let verb = auth::command_verb(&cmd);
        let keyspace_label = cmd.keyspace().unwrap_or("_global").to_string();
        let is_write = !cmd.is_read();
        let is_verify = matches!(cmd, Command::Verify { .. });
        let behavior = match cmd.replica_behavior() {
            crate::command::ReplicaBehavior::PureRead => "PureRead",
            crate::command::ReplicaBehavior::ObservationalRead => "ObservationalRead",
            crate::command::ReplicaBehavior::ConditionalWrite => "WriteOnly",
            crate::command::ReplicaBehavior::WriteOnly => "WriteOnly",
        };

        let start = Instant::now();
        let result = self.dispatch(cmd, keyspace.as_ref()).await;
        let duration = start.elapsed();

        let result_label = match &result {
            Ok(_) => "ok",
            Err(_) => "error",
        };

        counter!("shroudb_commands_total", "command" => verb, "keyspace" => keyspace_label.clone(), "result" => result_label).increment(1);
        histogram!("shroudb_command_duration_seconds", "command" => verb, "keyspace" => keyspace_label.clone()).record(duration.as_secs_f64());

        // Phase 0 replication metric: command behavior classification
        counter!("shroudb_commands_by_behavior_total", "behavior" => behavior).increment(1);

        // Phase 0 replication metric: VERIFY rate histogram
        if is_verify {
            histogram!("shroudb_verify_rate", "keyspace" => keyspace_label.clone())
                .record(duration.as_secs_f64());
        }

        if is_write {
            tracing::info!(
                target: "shroudb::audit",
                op = verb,
                keyspace = keyspace_label.as_str(),
                result = result_label,
                duration_ms = duration.as_millis() as u64,
                actor = auth.map(|a| a.name.as_str()).unwrap_or("anonymous"),
                "command executed"
            );
        }

        match result {
            Ok(resp) => CommandResponse::Success(resp),
            Err(e) => CommandResponse::Error(e),
        }
    }

    async fn dispatch(
        &self,
        cmd: Command,
        keyspace: Option<&shroudb_core::Keyspace>,
    ) -> Result<ResponseMap, CommandError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Capture info for lifecycle event publishing before the command is moved.
        let lifecycle_info = self.lifecycle_info_for(&cmd);

        let result = match cmd {
            Command::Issue {
                claims,
                metadata,
                ttl_secs,
                idempotency_key,
                ..
            } => {
                let ks = keyspace.unwrap();

                // Check idempotency
                if let Some(ref idem_key) = idempotency_key
                    && let Some(cached) = self.idempotency.check(idem_key).await
                {
                    return Ok(cached);
                }

                let result = handlers::issue::handle_issue(
                    &self.engine,
                    ks,
                    claims,
                    metadata,
                    ttl_secs,
                    idempotency_key.as_deref(),
                )
                .await?;

                // Cache idempotency response
                if let Some(idem_key) = idempotency_key {
                    self.idempotency.insert(idem_key, result.clone()).await;
                }

                Ok(result)
            }

            Command::Verify {
                token,
                payload,
                check_revoked,
                ..
            } => {
                let ks = keyspace.unwrap();
                handlers::verify::handle_verify(
                    &self.engine,
                    ks,
                    &token,
                    payload.as_deref(),
                    check_revoked,
                )
                .await
            }

            Command::Revoke {
                target, ttl_secs, ..
            } => {
                let ks = keyspace.unwrap();
                handlers::revoke::handle_revoke(&self.engine, ks, &target, ttl_secs).await
            }

            Command::Refresh { token, .. } => {
                let ks = keyspace.unwrap();
                handlers::refresh::handle_refresh(&self.engine, ks, &token).await
            }

            Command::Update {
                credential_id,
                metadata,
                ..
            } => {
                let ks = keyspace.unwrap();
                handlers::update::handle_update(&self.engine, ks, &credential_id, metadata).await
            }

            Command::Inspect { credential_id, .. } => {
                let ks = keyspace.unwrap();
                handlers::inspect::handle_inspect(&self.engine, ks, &credential_id).await
            }

            Command::Rotate { force, dryrun, .. } => {
                let ks = keyspace.unwrap();
                handlers::rotate::handle_rotate(&self.engine, ks, force, dryrun).await
            }

            Command::Jwks { .. } => {
                let ks = keyspace.unwrap();
                handlers::jwks::handle_jwks(&self.engine, ks).await
            }

            Command::KeyState { .. } => {
                let ks = keyspace.unwrap();
                handlers::keystate::handle_keystate(&self.engine, ks).await
            }

            Command::Health {
                keyspace: ks_name, ..
            } => handlers::health::handle_health(&self.engine, ks_name.as_deref()).await,

            Command::Keys {
                cursor,
                pattern,
                state_filter,
                count,
                ..
            } => {
                let ks = keyspace.unwrap();
                let params = handlers::keys::KeysParams {
                    cursor,
                    pattern,
                    state_filter,
                    count,
                };
                handlers::keys::handle_keys(&self.engine, ks, &params).await
            }

            Command::Suspend { credential_id, .. } => {
                let ks = keyspace.unwrap();
                handlers::suspend::handle_suspend(&self.engine, ks, &credential_id).await
            }

            Command::Unsuspend { credential_id, .. } => {
                let ks = keyspace.unwrap();
                handlers::suspend::handle_unsuspend(&self.engine, ks, &credential_id).await
            }

            Command::Schema { .. } => {
                let ks = keyspace.unwrap();
                handlers::schema::handle_schema(ks).await
            }

            Command::ConfigGet { key } => {
                handlers::config::handle_config_get(&self.engine, &key).await
            }

            Command::ConfigSet { key, value } => {
                handlers::config::handle_config_set(&key, &value).await
            }

            Command::Subscribe { .. } => {
                // SUBSCRIBE is handled at the connection level; reaching here means
                // it was dispatched in a context that doesn't support streaming.
                Err(CommandError::BadArg {
                    message: "SUBSCRIBE is only supported on persistent TCP connections".into(),
                })
            }

            Command::PasswordSet {
                user_id,
                plaintext,
                metadata,
                ..
            } => {
                let ks = keyspace.unwrap();
                handlers::password::handle_password_set(
                    &self.engine,
                    ks,
                    &user_id,
                    &plaintext,
                    metadata,
                )
                .await
            }

            Command::PasswordVerify {
                user_id, plaintext, ..
            } => {
                let ks = keyspace.unwrap();
                handlers::password::handle_password_verify(&self.engine, ks, &user_id, &plaintext)
                    .await
            }

            Command::PasswordChange {
                user_id,
                old_plaintext,
                new_plaintext,
                ..
            } => {
                let ks = keyspace.unwrap();
                handlers::password::handle_password_change(
                    &self.engine,
                    ks,
                    &user_id,
                    &old_plaintext,
                    &new_plaintext,
                )
                .await
            }

            Command::PasswordReset {
                user_id,
                new_plaintext,
                ..
            } => {
                let ks = keyspace.unwrap();
                handlers::password::handle_password_reset(
                    &self.engine,
                    ks,
                    &user_id,
                    &new_plaintext,
                )
                .await
            }

            Command::PasswordImport {
                user_id,
                hash,
                metadata,
                ..
            } => {
                let ks = keyspace.unwrap();
                handlers::password::handle_password_import(
                    &self.engine,
                    ks,
                    &user_id,
                    &hash,
                    metadata,
                )
                .await
            }

            Command::Auth { .. } => {
                // AUTH is handled at the connection/request level, not here
                Ok(ResponseMap::ok().with(
                    "message",
                    ResponseValue::String("use AUTH at connection level".into()),
                ))
            }

            Command::Pipeline(_) => unreachable!("pipeline handled above"),
        };

        // Publish lifecycle events for successful write commands.
        if result.is_ok()
            && let Some((event_type, ks_name, detail)) = lifecycle_info
        {
            self.event_bus.publish(LifecycleEvent {
                event_type,
                keyspace: ks_name,
                detail,
                timestamp: now,
            });
        }

        // Publish reuse_detected on the error path (ReuseDetected is an error).
        if let Err(CommandError::ReuseDetected { ref family_id }) = result {
            let ks_name = keyspace.map(|ks| ks.name.clone()).unwrap_or_default();
            self.event_bus.publish(LifecycleEvent {
                event_type: "reuse_detected".into(),
                keyspace: ks_name,
                detail: family_id.clone(),
                timestamp: now,
            });
        }

        result
    }

    /// Extract lifecycle event info from a command before it is consumed.
    fn lifecycle_info_for(&self, cmd: &Command) -> Option<(String, String, String)> {
        match cmd {
            Command::Revoke {
                keyspace, target, ..
            } => {
                let detail = match target {
                    crate::command::RevokeTarget::Single(id) => id.clone(),
                    crate::command::RevokeTarget::Family(fam) => fam.clone(),
                    crate::command::RevokeTarget::Bulk(ids) => {
                        format!("{} credentials", ids.len())
                    }
                };
                let event_type = match target {
                    crate::command::RevokeTarget::Family(_) => "family_revoked".to_string(),
                    _ => "revocation".to_string(),
                };
                Some((event_type, keyspace.clone(), detail))
            }
            Command::Rotate { keyspace, .. } => {
                Some(("rotation".into(), keyspace.clone(), String::new()))
            }
            Command::PasswordSet {
                keyspace, user_id, ..
            } => Some(("password_set".into(), keyspace.clone(), user_id.clone())),
            Command::PasswordChange {
                keyspace, user_id, ..
            } => Some(("password_changed".into(), keyspace.clone(), user_id.clone())),
            Command::PasswordReset {
                keyspace, user_id, ..
            } => Some(("password_reset".into(), keyspace.clone(), user_id.clone())),
            Command::PasswordImport {
                keyspace, user_id, ..
            } => Some((
                "password_imported".into(),
                keyspace.clone(),
                user_id.clone(),
            )),
            _ => None,
        }
    }

    /// Prune expired idempotency entries. Called by the background reaper.
    pub async fn prune_idempotency(&self) -> usize {
        self.idempotency.prune_expired().await
    }
}
