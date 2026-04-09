//! `RemoteStore` — `Store` trait implementation backed by a TCP/TLS connection
//! to a ShrouDB server.
//!
//! Engines use this when running in remote mode (`store.mode = "remote"`),
//! connecting to a shared ShrouDB server instead of embedding the storage engine.

use std::sync::Arc;

use tokio::sync::Mutex;

use shroudb_store::{
    Entry, Metadata, NamespaceConfig, NamespaceInfo, Page, PipelineCommand, PipelineResult, Store,
    StoreError, Subscription, SubscriptionEvent, SubscriptionFilter, ValidationReport, VersionInfo,
};

use crate::ShrouDBClient;
use crate::error::ClientError;

/// A `Store` implementation that connects to a remote ShrouDB server over TCP/TLS.
///
/// Thread-safe: wraps `ShrouDBClient` in a `Mutex` since the TCP connection
/// is single-threaded. For high-throughput workloads, use a connection pool.
pub struct RemoteStore {
    client: Arc<Mutex<ShrouDBClient>>,
}

impl RemoteStore {
    /// Create a RemoteStore from an already-connected and authenticated client.
    pub fn new(client: ShrouDBClient) -> Self {
        Self {
            client: Arc::new(Mutex::new(client)),
        }
    }

    /// Connect to a ShrouDB server and optionally authenticate.
    pub async fn connect(uri: &str) -> Result<Self, ClientError> {
        let client = ShrouDBClient::from_uri(uri).await?;
        Ok(Self::new(client))
    }
}

/// Subscription handle for RemoteStore. Not yet implemented — requires
/// a dedicated connection in streaming mode.
pub struct RemoteSubscription {
    _private: (),
}

impl Subscription for RemoteSubscription {
    async fn recv(&mut self) -> Option<SubscriptionEvent> {
        // Remote subscriptions need a separate connection in streaming mode.
        // The current ShrouDBClient doesn't support this because SUBSCRIBE
        // takes over the connection. A proper implementation would use a
        // dedicated connection for the subscription.
        None
    }
}

fn client_err(e: ClientError) -> StoreError {
    match e {
        ClientError::Server(msg) if msg.contains("not found") => StoreError::NotFound,
        ClientError::Server(msg) if msg.contains("namespace not found") => {
            // Extract namespace name if possible
            StoreError::NamespaceNotFound(msg)
        }
        ClientError::Server(msg) if msg.contains("namespace already exists") => {
            StoreError::NamespaceExists(msg)
        }
        ClientError::Server(msg) if msg.contains("namespace not empty") => {
            StoreError::NamespaceNotEmpty(msg)
        }
        ClientError::Server(msg) if msg.contains("invalid cursor") => {
            StoreError::InvalidCursor(msg)
        }
        ClientError::Server(msg) => StoreError::Storage(msg),
        ClientError::Connection(e) => StoreError::Connection(e.to_string()),
        ClientError::Timeout => StoreError::Connection("timeout".into()),
        other => StoreError::Storage(other.to_string()),
    }
}

impl Store for RemoteStore {
    type Subscription = RemoteSubscription;

    async fn put(
        &self,
        ns: &str,
        key: &[u8],
        value: &[u8],
        metadata: Option<Metadata>,
    ) -> Result<u64, StoreError> {
        let mut client = self.client.lock().await;
        let version = if let Some(meta) = metadata {
            client
                .put_with_metadata(ns, key, value, shroudb_store::metadata_to_json(&meta))
                .await
        } else {
            client.put(ns, key, value).await
        };
        version.map_err(client_err)
    }

    async fn get(&self, ns: &str, key: &[u8], version: Option<u64>) -> Result<Entry, StoreError> {
        let mut client = self.client.lock().await;
        let result = if let Some(v) = version {
            client.get_version(ns, key, v).await
        } else {
            client.get(ns, key).await
        };
        let gr = result.map_err(client_err)?;
        Ok(Entry {
            key: gr.key,
            value: gr.value,
            version: gr.version,
            metadata: gr
                .metadata
                .map(|j| serde_json::from_value(j).unwrap_or_default())
                .unwrap_or_default(),
            created_at: 0,
            updated_at: 0,
        })
    }

    async fn delete(&self, ns: &str, key: &[u8]) -> Result<u64, StoreError> {
        let mut client = self.client.lock().await;
        client.delete(ns, key).await.map_err(client_err)
    }

    async fn list(
        &self,
        ns: &str,
        prefix: Option<&[u8]>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Page, StoreError> {
        let mut client = self.client.lock().await;
        // Build LIST command with all optional parameters
        let mut args: Vec<String> = vec!["LIST".into(), ns.into()];
        if let Some(p) = prefix {
            args.push("PREFIX".into());
            args.push(String::from_utf8_lossy(p).into_owned());
        }
        if let Some(c) = cursor {
            args.push("CURSOR".into());
            args.push(c.into());
        }
        args.push("LIMIT".into());
        args.push(limit.to_string());

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let resp = client.raw_command(&args_ref).await.map_err(client_err)?;

        let keys_field = resp
            .get_field("keys")
            .ok_or_else(|| StoreError::Storage("missing keys field".into()))?;
        let keys = match keys_field {
            crate::Response::Array(items) => items
                .iter()
                .filter_map(|r| r.as_str().map(|s| s.as_bytes().to_vec()))
                .collect(),
            _ => Vec::new(),
        };
        let cursor = resp.get_string_field("cursor");

        Ok(Page { keys, cursor })
    }

    async fn versions(
        &self,
        ns: &str,
        key: &[u8],
        limit: usize,
        from_version: Option<u64>,
    ) -> Result<Vec<VersionInfo>, StoreError> {
        let mut client = self.client.lock().await;
        let mut args: Vec<String> = vec![
            "VERSIONS".into(),
            ns.into(),
            String::from_utf8_lossy(key).into_owned(),
        ];
        args.push("LIMIT".into());
        args.push(limit.to_string());
        if let Some(from) = from_version {
            args.push("FROM".into());
            args.push(from.to_string());
        }

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let resp = client.raw_command(&args_ref).await.map_err(client_err)?;

        let versions_field = resp
            .get_field("versions")
            .ok_or_else(|| StoreError::Storage("missing versions field".into()))?;

        match versions_field {
            crate::Response::Array(items) => {
                let mut entries = Vec::new();
                for item in items {
                    let state_str = item.get_string_field("state").unwrap_or_default();
                    let state = match state_str.as_str() {
                        "active" => shroudb_store::EntryState::Active,
                        "deleted" => shroudb_store::EntryState::Deleted,
                        _ => shroudb_store::EntryState::Active,
                    };
                    entries.push(VersionInfo {
                        version: item.get_int_field("version").unwrap_or(0) as u64,
                        state,
                        updated_at: item.get_int_field("updated_at").unwrap_or(0) as u64,
                        actor: item.get_string_field("actor").unwrap_or_default(),
                    });
                }
                Ok(entries)
            }
            _ => Err(StoreError::Storage("versions not an array".into())),
        }
    }

    async fn namespace_create(&self, ns: &str, config: NamespaceConfig) -> Result<(), StoreError> {
        let mut client = self.client.lock().await;
        let mut args: Vec<String> = vec!["NAMESPACE".into(), "CREATE".into(), ns.into()];

        if let Some(max_v) = config.max_versions {
            args.push("MAX_VERSIONS".into());
            args.push(max_v.to_string());
        }
        if let Some(retention) = config.tombstone_retention_secs {
            args.push("TOMBSTONE_RETENTION".into());
            args.push(retention.to_string());
        }
        if let Some(ref schema) = config.meta_schema {
            args.push("SCHEMA".into());
            args.push(serde_json::to_string(schema).unwrap_or_default());
        }

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let resp = client.raw_command(&args_ref).await.map_err(client_err)?;
        crate::check_ok(&resp).map_err(client_err)
    }

    async fn namespace_drop(&self, ns: &str, force: bool) -> Result<(), StoreError> {
        let mut client = self.client.lock().await;
        client.namespace_drop(ns, force).await.map_err(client_err)
    }

    async fn namespace_list(&self, cursor: Option<&str>, limit: usize) -> Result<Page, StoreError> {
        let mut client = self.client.lock().await;
        let mut args: Vec<String> = vec!["NAMESPACE".into(), "LIST".into()];
        if let Some(c) = cursor {
            args.push("CURSOR".into());
            args.push(c.into());
        }
        args.push("LIMIT".into());
        args.push(limit.to_string());

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let resp = client.raw_command(&args_ref).await.map_err(client_err)?;

        let ns_field = resp
            .get_field("namespaces")
            .ok_or_else(|| StoreError::Storage("missing namespaces field".into()))?;
        let keys = match ns_field {
            crate::Response::Array(items) => items
                .iter()
                .filter_map(|r| r.as_str().map(|s| s.as_bytes().to_vec()))
                .collect(),
            _ => Vec::new(),
        };
        let cursor = resp.get_string_field("cursor");

        Ok(Page { keys, cursor })
    }

    async fn namespace_info(&self, ns: &str) -> Result<NamespaceInfo, StoreError> {
        let mut client = self.client.lock().await;
        let info = client.namespace_info(ns).await.map_err(client_err)?;
        Ok(NamespaceInfo {
            name: info.name,
            key_count: info.key_count,
            created_at: info.created_at,
            config: NamespaceConfig::default(),
        })
    }

    async fn namespace_alter(&self, ns: &str, config: NamespaceConfig) -> Result<(), StoreError> {
        let mut client = self.client.lock().await;
        let mut args: Vec<String> = vec!["NAMESPACE".into(), "ALTER".into(), ns.into()];

        if let Some(max_v) = config.max_versions {
            args.push("MAX_VERSIONS".into());
            args.push(max_v.to_string());
        }
        if let Some(retention) = config.tombstone_retention_secs {
            args.push("TOMBSTONE_RETENTION".into());
            args.push(retention.to_string());
        }
        if let Some(ref schema) = config.meta_schema {
            args.push("SCHEMA".into());
            args.push(serde_json::to_string(schema).unwrap_or_default());
        }

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let resp = client.raw_command(&args_ref).await.map_err(client_err)?;
        crate::check_ok(&resp).map_err(client_err)
    }

    async fn namespace_validate(&self, ns: &str) -> Result<Vec<ValidationReport>, StoreError> {
        let mut client = self.client.lock().await;
        let resp = client
            .raw_command(&["NAMESPACE", "VALIDATE", ns])
            .await
            .map_err(client_err)?;
        // Parse validation reports from response
        // For now, return empty if status is OK (no violations)
        crate::check_ok(&resp).map_err(client_err)?;
        Ok(Vec::new())
    }

    async fn pipeline(
        &self,
        commands: Vec<PipelineCommand>,
    ) -> Result<Vec<PipelineResult>, StoreError> {
        let mut client = self.client.lock().await;

        let cmd_args: Vec<Vec<&str>> = commands
            .iter()
            .map(|cmd| match cmd {
                PipelineCommand::Put { ns, key, value, .. } => {
                    vec![
                        "PUT",
                        ns.as_str(),
                        std::str::from_utf8(key).unwrap_or(""),
                        std::str::from_utf8(value).unwrap_or(""),
                    ]
                }
                PipelineCommand::Get { ns, key, .. } => {
                    vec!["GET", ns.as_str(), std::str::from_utf8(key).unwrap_or("")]
                }
                PipelineCommand::Delete { ns, key } => {
                    vec![
                        "DELETE",
                        ns.as_str(),
                        std::str::from_utf8(key).unwrap_or(""),
                    ]
                }
            })
            .collect();

        let cmd_slices: Vec<&[&str]> = cmd_args.iter().map(|v| v.as_slice()).collect();
        let results = client
            .pipeline(&cmd_slices, None)
            .await
            .map_err(client_err)?;

        let mut pipeline_results = Vec::with_capacity(results.len());
        for (cmd, resp) in commands.iter().zip(results.iter()) {
            match cmd {
                PipelineCommand::Put { .. } => {
                    let version = resp.get_int_field("version").unwrap_or(0) as u64;
                    pipeline_results.push(PipelineResult::Put(version));
                }
                PipelineCommand::Get { .. } => {
                    let key = resp
                        .get_string_field("key")
                        .unwrap_or_default()
                        .into_bytes();
                    let value = resp
                        .get_string_field("value")
                        .unwrap_or_default()
                        .into_bytes();
                    let version = resp.get_int_field("version").unwrap_or(0) as u64;
                    pipeline_results.push(PipelineResult::Get(Entry {
                        key,
                        value,
                        version,
                        metadata: Default::default(),
                        created_at: 0,
                        updated_at: 0,
                    }));
                }
                PipelineCommand::Delete { .. } => {
                    let version = resp.get_int_field("version").unwrap_or(0) as u64;
                    pipeline_results.push(PipelineResult::Delete(version));
                }
            }
        }

        Ok(pipeline_results)
    }

    async fn subscribe(
        &self,
        _ns: &str,
        _filter: SubscriptionFilter,
    ) -> Result<Self::Subscription, StoreError> {
        // Remote subscriptions require a dedicated connection in streaming mode.
        // The current implementation returns a no-op subscription.
        // A proper implementation would spawn a second connection, send SUBSCRIBE,
        // and forward push frames to the subscription handle.
        Err(StoreError::Storage(
            "remote subscriptions not yet implemented — use SUBSCRIBE directly via raw_command"
                .into(),
        ))
    }
}
