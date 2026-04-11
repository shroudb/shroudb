use std::sync::Arc;
use std::time::Instant;

use metrics::{counter, histogram};

use shroudb_acl::AuthContext;
use shroudb_storage::StorageEngine;
use shroudb_store::Store;

use crate::command::Command;
use crate::error::CommandError;
use crate::handlers;
use crate::idempotency::IdempotencyMap;
use crate::resp3::serialize::response_to_frame;
use crate::response::{CommandResponse, ResponseMap, ResponseValue};

/// Routes parsed commands to handlers, enforcing ACL at the dispatcher level.
pub struct CommandDispatcher<S: Store> {
    store: S,
    engine: Option<Arc<StorageEngine>>,
    idempotency: IdempotencyMap,
}

impl<S: Store> CommandDispatcher<S> {
    /// Create a dispatcher with an embedded storage engine (normal mode).
    pub fn new(store: S, engine: Arc<StorageEngine>) -> Self {
        Self {
            store,
            engine: Some(engine),
            idempotency: IdempotencyMap::new(),
        }
    }

    /// Create a dispatcher backed by a remote store (no local engine).
    /// CONFIG GET/SET operations are unavailable in this mode.
    pub fn new_remote(store: S) -> Self {
        Self {
            store,
            engine: None,
            idempotency: IdempotencyMap::new(),
        }
    }

    /// Access the underlying store (e.g., for connection-level SUBSCRIBE).
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Access the idempotency map (for periodic pruning).
    pub fn idempotency(&self) -> &IdempotencyMap {
        &self.idempotency
    }

    /// Execute a command with the given auth context.
    pub async fn execute(&self, cmd: Command, auth: Option<&AuthContext>) -> CommandResponse {
        // Pipeline: execute each sub-command (boxed to avoid infinite future size)
        if let Command::Pipeline {
            commands,
            request_id,
            ..
        } = cmd
        {
            // Idempotency: return cached response if request_id was seen before
            if let Some(ref id) = request_id
                && let Some(cached_frame) = self.idempotency.get(id)
            {
                return CommandResponse::CachedFrame(cached_frame);
            }

            let mut results = Vec::with_capacity(commands.len());
            for c in commands {
                results.push(Box::pin(self.execute(c, auth)).await);
            }
            let response = CommandResponse::Array(results);

            // Cache the serialized frame for future retries
            if let Some(id) = request_id {
                let frame = response_to_frame(&response);
                self.idempotency.insert(id, frame);
            }

            return response;
        }

        // ACL middleware: check before handler runs
        let requirement = cmd.acl_requirement();
        match &requirement {
            shroudb_acl::AclRequirement::None => {}
            _ => match auth {
                None => {
                    return CommandResponse::Error(CommandError::NotAuthenticated);
                }
                Some(ctx) => {
                    if let Err(e) = ctx.check(&requirement) {
                        return CommandResponse::Error(CommandError::Denied {
                            reason: e.to_string(),
                        });
                    }
                }
            },
        }

        let verb = cmd.verb();
        let is_write = !cmd.is_read();

        let start = Instant::now();
        let result = self.dispatch(cmd, auth).await;
        let duration = start.elapsed();

        let result_label = match &result {
            Ok(_) => "ok",
            Err(_) => "error",
        };

        counter!("shroudb_commands_total", "command" => verb, "result" => result_label)
            .increment(1);
        histogram!("shroudb_command_duration_seconds", "command" => verb)
            .record(duration.as_secs_f64());

        if is_write {
            tracing::info!(
                target: "shroudb::audit",
                op = verb,
                result = result_label,
                duration_ms = duration.as_millis() as u64,
                actor = auth.map(|a| a.actor.as_str()).unwrap_or("anonymous"),
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
        _auth: Option<&AuthContext>,
    ) -> Result<ResponseMap, CommandError> {
        match cmd {
            // ── Connection ───────────────────────────────────────────
            Command::Auth { .. } => Err(CommandError::Internal(
                "AUTH must be handled at the connection level (bug: reached dispatcher)".into(),
            )),

            Command::Ping => {
                Ok(ResponseMap::ok().with("message", ResponseValue::String("PONG".into())))
            }

            // ── Data operations ──────────────────────────────────────
            Command::Put {
                ns,
                key,
                value,
                metadata,
            } => handlers::put::handle(&self.store, &ns, &key, &value, metadata).await,

            Command::Get {
                ns,
                key,
                version,
                meta,
            } => handlers::get::handle(&self.store, &ns, &key, version, meta).await,

            Command::Delete { ns, key } => handlers::delete::handle(&self.store, &ns, &key).await,

            Command::List {
                ns,
                prefix,
                cursor,
                limit,
            } => {
                handlers::list::handle(
                    &self.store,
                    &ns,
                    prefix.as_deref(),
                    cursor.as_deref(),
                    limit.unwrap_or(100),
                )
                .await
            }

            Command::Versions {
                ns,
                key,
                limit,
                from,
            } => {
                handlers::versions::handle(&self.store, &ns, &key, limit.unwrap_or(100), from).await
            }

            // ── Namespace operations ─────────────────────────────────
            Command::NamespaceCreate {
                name,
                schema,
                max_versions,
                tombstone_retention_secs,
            } => {
                handlers::namespace::handle_create(
                    &self.store,
                    &name,
                    schema,
                    max_versions,
                    tombstone_retention_secs,
                )
                .await
            }

            Command::NamespaceDrop { name, force } => {
                handlers::namespace::handle_drop(&self.store, &name, force).await
            }

            Command::NamespaceList { cursor, limit } => {
                handlers::namespace::handle_list(
                    &self.store,
                    cursor.as_deref(),
                    limit.unwrap_or(100),
                )
                .await
            }

            Command::NamespaceInfo { name } => {
                handlers::namespace::handle_info(&self.store, &name).await
            }

            Command::NamespaceAlter {
                name,
                schema,
                max_versions,
                tombstone_retention_secs,
            } => {
                handlers::namespace::handle_alter(
                    &self.store,
                    &name,
                    schema,
                    max_versions,
                    tombstone_retention_secs,
                )
                .await
            }

            Command::NamespaceValidate { name } => {
                handlers::namespace::handle_validate(&self.store, &name).await
            }

            // ── Operational ──────────────────────────────────────────
            Command::Health => handlers::health::handle().await,

            Command::ConfigGet { key } => match self.engine {
                Some(ref engine) => handlers::config::handle_get(engine.config_store(), &key).await,
                None => Err(CommandError::Internal(
                    "CONFIG not available in remote store mode".into(),
                )),
            },

            Command::ConfigSet { key, value } => match self.engine {
                Some(ref engine) => handlers::config::handle_set(engine, &key, &value).await,
                None => Err(CommandError::Internal(
                    "CONFIG not available in remote store mode".into(),
                )),
            },

            Command::CommandList => handlers::command_list::handle().await,

            // ── Streaming ────────────────────────────────────────────
            Command::Subscribe { .. } => Err(CommandError::BadArg {
                message: "SUBSCRIBE must be handled at the connection level".into(),
            }),

            Command::Unsubscribe => Err(CommandError::BadArg {
                message: "UNSUBSCRIBE: no active subscription".into(),
            }),

            // Pipeline handled above
            Command::Pipeline { .. } => unreachable!("pipeline handled in execute()"),
        }
    }
}
