//! `RemoteStore` — `Store` trait implementation backed by a TCP/TLS connection
//! to a ShrouDB server.
//!
//! Engines use this when running in remote mode (`store.mode = "remote"`),
//! connecting to a shared ShrouDB server instead of embedding the storage engine.

use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;

use shroudb_store::{
    Entry, EventType, Metadata, NamespaceConfig, NamespaceInfo, Page, PipelineCommand,
    PipelineResult, Store, StoreError, Subscription, SubscriptionEvent, SubscriptionFilter,
    ValidationReport, VersionInfo,
};

use crate::connection::Connection;
use crate::error::ClientError;
use crate::response::Response;
use crate::{ConnectionConfig, ShrouDBClient};

/// A `Store` implementation that connects to a remote ShrouDB server over TCP/TLS.
///
/// Thread-safe: wraps `ShrouDBClient` in a `Mutex` since the TCP connection
/// is single-threaded. For high-throughput workloads, use a connection pool.
pub struct RemoteStore {
    client: Arc<Mutex<ShrouDBClient>>,
    /// Connection config for spawning dedicated subscription connections.
    config: ConnectionConfig,
}

impl RemoteStore {
    /// Create a RemoteStore from an already-connected and authenticated client.
    ///
    /// The URI is required so that `subscribe()` can open dedicated connections.
    pub fn new(client: ShrouDBClient, uri: &str) -> Result<Self, ClientError> {
        let config = crate::parse_uri(uri)?;
        Ok(Self {
            client: Arc::new(Mutex::new(client)),
            config,
        })
    }

    /// Connect to a ShrouDB server and optionally authenticate.
    pub async fn connect(uri: &str) -> Result<Self, ClientError> {
        let client = ShrouDBClient::from_uri(uri).await?;
        Self::new(client, uri)
    }

    /// Open a new, authenticated connection for streaming (subscription) use.
    async fn open_streaming_connection(&self) -> Result<Connection, ClientError> {
        let addr = format!("{}:{}", self.config.host, self.config.port);
        let mut conn = if self.config.tls {
            Connection::connect_tls(&addr).await?
        } else {
            Connection::connect(&addr).await?
        };

        if let Some(ref token) = self.config.auth_token {
            let resp = conn.send_meta_command_strs(&["AUTH", token]).await?;
            crate::check_ok(&resp)?;
        }

        Ok(conn)
    }
}

/// Active subscription over a dedicated TCP connection.
///
/// Events are received from a background task that reads RESP3 push frames
/// from the server. Dropping the subscription cancels the background task.
pub struct RemoteSubscription {
    rx: mpsc::Receiver<SubscriptionEvent>,
    _task: JoinHandle<()>,
}

impl Subscription for RemoteSubscription {
    async fn recv(&mut self) -> Option<SubscriptionEvent> {
        self.rx.recv().await
    }
}

/// Parse a RESP3 push frame (received as a `Response::Array`) into a
/// `SubscriptionEvent`.
///
/// Server format: `["subscribe", event_type, namespace, key, version, actor, timestamp]`
fn parse_push_frame(resp: &Response) -> Option<SubscriptionEvent> {
    let items = match resp {
        Response::Array(items) if items.len() >= 7 => items,
        _ => return None,
    };

    // items[0] = "subscribe" (label)
    let label = items[0].as_str()?;
    if label != "subscribe" {
        return None;
    }

    // items[1] = event type ("put" or "delete")
    let event = match items[1].as_str()? {
        "put" => EventType::Put,
        "delete" => EventType::Delete,
        _ => return None,
    };

    // items[2] = namespace
    let namespace = items[2].as_str()?.to_string();

    // items[3] = key (bulk string)
    let key = items[3].as_str()?.as_bytes().to_vec();

    // items[4] = version
    let version = items[4].as_int()? as u64;

    // items[5] = actor
    let actor = items[5].as_str()?.to_string();

    // items[6] = timestamp
    let timestamp = items[6].as_int()? as u64;

    Some(SubscriptionEvent {
        event,
        namespace,
        key,
        version,
        actor,
        timestamp,
    })
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
        ns: &str,
        filter: SubscriptionFilter,
    ) -> Result<Self::Subscription, StoreError> {
        // Open a dedicated connection for this subscription.
        let mut conn = self.open_streaming_connection().await.map_err(client_err)?;

        // Build SUBSCRIBE command: SUBSCRIBE <ns> [KEY <key>] [EVENTS <evt1|evt2|...>]
        let mut args: Vec<String> = vec!["SUBSCRIBE".into(), ns.into()];
        if let Some(ref key) = filter.key {
            args.push("KEY".into());
            args.push(String::from_utf8_lossy(key).into_owned());
        }
        if !filter.events.is_empty() {
            args.push("EVENTS".into());
            let event_strs: Vec<&str> = filter
                .events
                .iter()
                .map(|e| match e {
                    EventType::Put => "put",
                    EventType::Delete => "delete",
                })
                .collect();
            args.push(event_strs.join("|"));
        }

        // Send SUBSCRIBE and read the OK confirmation.
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let resp = conn
            .send_command_strs(&args_ref)
            .await
            .map_err(client_err)?;
        crate::check_ok(&resp).map_err(client_err)?;

        // Channel for relaying events from the background reader to the
        // RemoteSubscription handle. Bounded to prevent unbounded memory growth
        // if the consumer falls behind.
        let (tx, rx) = mpsc::channel::<SubscriptionEvent>(1024);

        // Spawn background task that reads push frames from the dedicated connection.
        let task = tokio::spawn(async move {
            while let Ok(resp) = conn.read_response_streaming().await {
                // Check for subscription closed signal
                if let Response::Array(ref items) = resp
                    && items.len() >= 2
                    && let (Some("subscription"), Some("closed")) =
                        (items[0].as_str(), items[1].as_str())
                {
                    break;
                }

                if let Some(event) = parse_push_frame(&resp)
                    && tx.send(event).await.is_err()
                {
                    // Receiver dropped — consumer no longer interested.
                    break;
                }
            }
        });

        Ok(RemoteSubscription { rx, _task: task })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_push_frame_valid_put() {
        let frame = Response::Array(vec![
            Response::String("subscribe".into()),
            Response::String("put".into()),
            Response::String("myapp.users".into()),
            Response::String("user:1".into()),
            Response::Integer(3),
            Response::String("admin".into()),
            Response::Integer(1700000000),
        ]);
        let event = parse_push_frame(&frame).unwrap();
        assert_eq!(event.event, EventType::Put);
        assert_eq!(event.namespace, "myapp.users");
        assert_eq!(event.key, b"user:1");
        assert_eq!(event.version, 3);
        assert_eq!(event.actor, "admin");
        assert_eq!(event.timestamp, 1700000000);
    }

    #[test]
    fn parse_push_frame_valid_delete() {
        let frame = Response::Array(vec![
            Response::String("subscribe".into()),
            Response::String("delete".into()),
            Response::String("ns".into()),
            Response::String("key".into()),
            Response::Integer(5),
            Response::String("system".into()),
            Response::Integer(1700000001),
        ]);
        let event = parse_push_frame(&frame).unwrap();
        assert_eq!(event.event, EventType::Delete);
        assert_eq!(event.version, 5);
    }

    #[test]
    fn parse_push_frame_wrong_label() {
        let frame = Response::Array(vec![
            Response::String("other".into()),
            Response::String("put".into()),
            Response::String("ns".into()),
            Response::String("key".into()),
            Response::Integer(1),
            Response::String("actor".into()),
            Response::Integer(0),
        ]);
        assert!(parse_push_frame(&frame).is_none());
    }

    #[test]
    fn parse_push_frame_too_short() {
        let frame = Response::Array(vec![
            Response::String("subscribe".into()),
            Response::String("put".into()),
        ]);
        assert!(parse_push_frame(&frame).is_none());
    }

    #[test]
    fn parse_push_frame_not_array() {
        let frame = Response::String("subscribe".into());
        assert!(parse_push_frame(&frame).is_none());
    }

    #[test]
    fn parse_push_frame_invalid_event_type() {
        let frame = Response::Array(vec![
            Response::String("subscribe".into()),
            Response::String("update".into()),
            Response::String("ns".into()),
            Response::String("key".into()),
            Response::Integer(1),
            Response::String("actor".into()),
            Response::Integer(0),
        ]);
        assert!(parse_push_frame(&frame).is_none());
    }
}
