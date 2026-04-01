# ShrouDB — Agent Instructions

> Versioned encrypted key-value store with cryptographic tenant isolation, HKDF-derived per-namespace keys, append-only WAL, and RESP3 wire protocol.

## Quick Context

- **Role in ecosystem**: Core foundation — every engine depends on the Store trait defined here
- **Deployment modes**: embedded (in-process via `StorageEngine`) | remote (TCP/TLS server on port 6399)
- **Wire protocol**: RESP3 (binary framing, NOT Redis-compatible — different command set, data model, and security model)
- **Backing store**: WAL-based embedded storage engine with AES-256-GCM encryption at rest

## Workspace Layout

```
shroudb-protocol/     # Command parsing, dispatch, ACL enforcement, RESP3 codec
shroudb-server/       # TCP/TLS/UDS binary
shroudb-client/       # Typed Rust SDK + RemoteStore (implements Store trait)
shroudb-cli/          # Interactive REPL
```

## RESP3 Commands

### Data Commands

| Command | Args | Returns | Description |
|---------|------|---------|-------------|
| `PUT` | `<ns> <key> [VALUE <value>] [META <json>]` | `{status, version}` | Store value, returns new version |
| `GET` | `<ns> <key> [VERSION <n>] [META]` | `{status, key, value, version, metadata?}` | Retrieve value (latest or specific version) |
| `DELETE` | `<ns> <key>` | `{status, version}` | Tombstone delete, returns version |
| `LIST` | `<ns> [PREFIX <p>] [CURSOR <c>] [LIMIT <n>]` | `{status, keys, cursor?}` | Paginated key listing |
| `VERSIONS` | `<ns> <key> [LIMIT <n>] [FROM <v>]` | `{status, versions}` | Version history for a key |

### Namespace Management

| Command | Args | Returns | Description |
|---------|------|---------|-------------|
| `NAMESPACE CREATE` | `<name> [SCHEMA <json>] [MAX_VERSIONS <n>] [TOMBSTONE_RETENTION <secs>]` | `{status}` | Create namespace (Admin) |
| `NAMESPACE DROP` | `<name> [FORCE]` | `{status}` | Drop namespace (Admin) |
| `NAMESPACE LIST` | `[CURSOR <c>] [LIMIT <n>]` | `{status, namespaces, cursor?}` | List namespaces |
| `NAMESPACE INFO` | `<name>` | `{status, name, key_count, created_at}` | Namespace metadata |
| `NAMESPACE ALTER` | `<name> [SCHEMA <json>] [MAX_VERSIONS <n>] [TOMBSTONE_RETENTION <secs>]` | `{status}` | Modify namespace config (Admin) |
| `NAMESPACE VALIDATE` | `<name>` | `{status, count, reports}` | Validate all entries against schema |

### Batch & Streaming

| Command | Args | Returns | Description |
|---------|------|---------|-------------|
| `PIPELINE` | `[REQUEST_ID <id>] <cmd1> <cmd2> ...` | Array of responses | Atomic batch; idempotent with REQUEST_ID |
| `SUBSCRIBE` | `<ns> [KEY <key>] [EVENTS <PUT\|DELETE\|*>]` | Push frames | Enter streaming mode |
| `UNSUBSCRIBE` | — | — | Exit streaming mode |

### Connection & Operational

| Command | Args | Returns | Description |
|---------|------|---------|-------------|
| `AUTH` | `<token>` | `{status, actor}` | Authenticate connection |
| `PING` | — | `{status, message: "PONG"}` | Liveness |
| `HEALTH` | — | `{status, message: "healthy"}` | Health check |
| `CONFIG GET` | `<key>` | `{status, key, value?, source}` | Read runtime config |
| `CONFIG SET` | `<key> <value>` | `{status}` | Set runtime config (Admin, persisted to WAL) |
| `COMMAND LIST` | — | `{status, count, commands}` | List all commands |

### Command Examples

```
> PUT default mykey VALUE hello META {"env":"prod"}
%2 +status +OK +version :1

> GET default mykey META
%5 +status +OK +key $5 mykey +value $5 hello +version :1 +metadata $14 {"env":"prod"}

> PIPELINE REQUEST_ID abc123 PUT default k1 VALUE v1 PUT default k2 VALUE v2
*2 %2 +status +OK +version :1 %2 +status +OK +version :1
```

## Public API (Embedded Mode)

### Store Trait

```rust
pub trait Store: Send + Sync {
    type Subscription: Subscription;

    async fn put(&self, ns: &str, key: &[u8], value: &[u8], metadata: Option<Metadata>) -> Result<u64, StoreError>;
    async fn get(&self, ns: &str, key: &[u8], version: Option<u64>) -> Result<Entry, StoreError>;
    async fn delete(&self, ns: &str, key: &[u8]) -> Result<u64, StoreError>;
    async fn list(&self, ns: &str, prefix: Option<&[u8]>, cursor: Option<&str>, limit: usize) -> Result<Page, StoreError>;
    async fn versions(&self, ns: &str, key: &[u8], limit: usize, from_version: Option<u64>) -> Result<Vec<VersionInfo>, StoreError>;
    async fn namespace_create(&self, name: &str, config: NamespaceConfig) -> Result<(), StoreError>;
    async fn namespace_drop(&self, name: &str, force: bool) -> Result<(), StoreError>;
    async fn namespace_list(&self, cursor: Option<&str>, limit: usize) -> Result<Page, StoreError>;
    async fn namespace_info(&self, name: &str) -> Result<NamespaceInfo, StoreError>;
    async fn namespace_alter(&self, name: &str, config: NamespaceConfig) -> Result<(), StoreError>;
    async fn namespace_validate(&self, name: &str) -> Result<Vec<ValidationReport>, StoreError>;
    async fn pipeline(&self, ops: Vec<PipelineCommand>) -> Result<Vec<PipelineResult>, StoreError>;
    async fn subscribe(&self, ns: &str, filter: SubscriptionFilter) -> Result<Self::Subscription, StoreError>;
}
```

### Core Types

```rust
pub struct Entry {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub version: u64,
    pub metadata: Metadata,
    pub created_at: u64,
    pub updated_at: u64,
}

pub struct Page { pub keys: Vec<Vec<u8>>, pub cursor: Option<String> }
pub struct VersionInfo { pub version: u64, pub state: EntryState, pub updated_at: u64, pub actor: String }
pub enum EntryState { Active, Deleted }
pub struct NamespaceConfig { pub meta_schema: Option<MetaSchema>, pub max_versions: Option<u64>, pub tombstone_retention_secs: Option<u64> }
pub struct NamespaceInfo { pub name: String, pub key_count: u64, pub created_at: u64 }
```

### Usage Pattern

```rust
use shroudb_storage::{StorageEngine, StorageEngineConfig, EmbeddedStore};
use shroudb_store::Store;

let config = StorageEngineConfig { data_dir: "./data".into(), ..Default::default() };
let engine = StorageEngine::new(config).await?;
let store = EmbeddedStore::new(Arc::new(engine), "myapp");

store.namespace_create("users", NamespaceConfig::default()).await?;
let version = store.put("users", b"alice", b"{\"role\":\"admin\"}", None).await?;
let entry = store.get("users", b"alice", None).await?;
```

## Configuration

Server config (`shroudb.toml`):

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `server.bind` | `SocketAddr` | `"0.0.0.0:6399"` | TCP listen address |
| `server.tls_cert` | `Option<PathBuf>` | `None` | TLS certificate path |
| `server.tls_key` | `Option<PathBuf>` | `None` | TLS private key path |
| `server.tls_client_ca` | `Option<PathBuf>` | `None` | Client CA for mTLS |
| `server.unix_socket` | `Option<PathBuf>` | `None` | Unix domain socket |
| `server.rate_limit_per_second` | `Option<u32>` | `None` | Per-connection rate limit |
| `server.metrics_bind` | `Option<SocketAddr>` | `None` | Prometheus metrics endpoint |
| `server.otel_endpoint` | `Option<String>` | `None` | OTLP endpoint |
| `storage.data_dir` | `PathBuf` | `"./data"` | Data directory |
| `storage.fsync_mode` | `Option<String>` | `None` (PerWrite) | WAL fsync strategy |
| `storage.max_segment_bytes` | `Option<u64>` | `67108864` (64 MiB) | Max WAL segment size |
| `storage.snapshot_interval_entries` | `Option<u64>` | `100000` | Snapshot trigger threshold |
| `storage.snapshot_interval_minutes` | `Option<u64>` | `60` | Snapshot time trigger |
| `storage.cache.memory_budget` | `Option<String>` | `None` (unlimited) | `"256mb"`, `"70%"`, `"auto"` |
| `auth.method` | `Option<String>` | `None` | `"token"` to enable auth |
| `auth.tokens.<key>.tenant` | `String` | — | Tenant ID (HKDF context) |
| `auth.tokens.<key>.actor` | `String` | `"anonymous"` | Audit identity |
| `auth.tokens.<key>.platform` | `bool` | `false` | Cross-tenant superuser |
| `auth.tokens.<key>.grants` | `Vec<Grant>` | `[]` | Namespace + scope grants |

## Data Model

### Logical Structure

```
Namespace (crypto isolation boundary)
  └─ Key (UTF-8 bytes)
      └─ Version history (append-only)
          └─ { version: u64, state: Active|Deleted, value, metadata, actor, timestamps }
```

### Key Derivation (HKDF-SHA256)

```
Master Key (32 bytes, from env/file/ephemeral)
  ├─ HKDF(master, tenant_ctx, "{namespace}_wal")     → WAL encryption key
  ├─ HKDF(master, tenant_ctx, "__snapshot__")         → Snapshot encryption key
  └─ HKDF(master, tenant_ctx, "__snapshot_hmac__")    → Snapshot HMAC key
```

Different tenants get cryptographically isolated storage even with the same master key.

### WAL Entry Format

```
[len:u32][version:u8][flags:u8][ns_len:u16][ns:N][op_type:u8][timestamp:u64][encrypted_payload:M][crc32:u32]
```

All payloads AES-256-GCM encrypted. CRC32 covers entire entry.

### ACL Model

```rust
pub enum AclRequirement {
    None,                                      // PING, HEALTH, AUTH, CONFIG GET, COMMAND LIST
    Admin,                                     // NAMESPACE CREATE/DROP/ALTER, CONFIG SET
    Namespace { ns: String, scope: Scope },    // Read: GET/LIST/VERSIONS  Write: PUT/DELETE
}
```

## Integration Patterns

Every engine in the ecosystem uses the Store trait for persistence. The typical pattern:

```rust
// Engine wraps Store for its domain logic
pub struct MyEngine<S: Store> {
    store: Arc<S>,
    cache: DashMap<String, MyType>,
}

// Works with EmbeddedStore (in-process) or RemoteStore (TCP)
let engine = MyEngine::new(store.clone()).await?;
```

For Moat (unified gateway), all engines share a single `StorageEngine` instance via `EmbeddedStore::new(storage.clone(), "engine_namespace")`.

## Common Mistakes

- Do not assume Redis compatibility — ShrouDB has a completely different command set, data model, and security model. RESP3 is just the wire format.
- Every write creates a new version. There are no blind overwrites. Use `VERSIONS` to inspect history.
- `DELETE` creates a tombstone version, not a physical delete. The key remains in version history until `tombstone_retention_secs` expires.
- Namespace names in HKDF derivation mean renaming a namespace changes its encryption key — data must be migrated, not just relabeled.
- `PIPELINE REQUEST_ID` enables idempotent retries. Always use it for critical multi-step operations.
- Master key loss means permanent data loss. There is no recovery path.

## Related Crates

| Crate | Relationship |
|-------|-------------|
| `shroudb-store` | Defines the Store trait all engines depend on |
| `shroudb-storage` | WAL-based embedded storage engine (EmbeddedStore) |
| `shroudb-crypto` | AES-256-GCM, HKDF, JWT, password hashing primitives |
| `shroudb-acl` | Token validation, AuthContext, Grant model |
| `shroudb-protocol-wire` | RESP3 frame codec |
| `shroudb-moat` | Unified gateway that embeds ShrouDB as the backing store for all engines |
