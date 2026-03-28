# ShrouDB Architecture

A concise guide for contributors. See `PROJECT.md` for the full project plan.

---

## Crate Map

```
shroudb (binary)
├── shroudb-protocol     Command parsing, ACL middleware, dispatch, handlers
│   ├── resp3/           Wire format parser/serializer
│   ├── handlers/        One file per command (put, get, delete, list, ...)
│   └── acl.rs           Re-exports from shroudb-acl
├── shroudb-client       Typed async Rust client library (RESP3 over TCP/TLS)
└── shroudb-cli          Interactive REPL (wraps shroudb-client)
```

### Commons Crates (shared across all ShrouDB products)

```
shroudb-store            Store trait + core types (Entry, Namespace, MetaSchema)
shroudb-acl              Token model, scopes, grants, AuthContext, TokenValidator
shroudb-storage          WAL, snapshots, KvIndex, EmbeddedStore (implements Store)
shroudb-crypto           AEAD, HKDF, HMAC, JWT, password hashing
shroudb-protocol-wire    RESP3 frame types (shared wire format)
shroudb-telemetry        OpenTelemetry setup
```

### Dependency Graph

```
shroudb-store  ←── shroudb-acl (no dep on store)
     ↑                  ↑
shroudb-storage    shroudb-protocol  ←── shroudb (bin)
     ↑                  ↑
shroudb-crypto    shroudb-client  ←── shroudb-cli (bin)
```

`shroudb-client` and `shroudb-cli` are pure TCP clients. They do NOT depend on `shroudb-storage` or `shroudb-protocol` — they speak RESP3 over the wire.

---

## Data Flow

```
Client
  │
  │  TCP / TLS
  ▼
Connection (shroudb-server/src/connection.rs)
  │  AUTH → TokenValidator → AuthContext (per-connection)
  │  Rate limiter (token bucket)
  │
  │  Raw bytes
  ▼
RESP3 Parser (shroudb-protocol/src/resp3/)
  │
  │  Command enum
  ▼
ACL Middleware (shroudb-protocol/src/dispatch.rs)
  │  command.acl_requirement() → check(auth_context)
  │  Reject before handler if unauthorized
  ▼
Handler (shroudb-protocol/src/handlers/{put,get,...}.rs)
  │  Calls Store trait methods
  ▼
EmbeddedStore (shroudb-storage/src/embedded_store.rs)
  │  Validates namespace, MetaSchema, version
  ▼
StorageEngine (shroudb-storage/src/engine.rs)
  ├──▶ WAL Writer  ──▶  Append entry to segment file (encrypted)
  ├──▶ KV Index    ──▶  Update in-memory DashMap indexes
  └──▶ Snapshot    ──▶  Periodic full-state dump (encrypted)
```

---

## Storage Layout

```
{data_dir}/
├── {namespace}/
│   ├── wal/
│   │   ├── 000001.wal          # WAL segment files (sequential)
│   │   ├── 000002.wal
│   │   └── 000003.wal
│   └── snapshots/
│       └── snap_1711234567_abcd1234.snap
└── (no other files)
```

### WAL Entry Format (v2)

```
[len:u32] [version:u8] [flags:u8] [ns_len:u16] [ns:N] [op_type:u8] [timestamp:u64] [encrypted_payload:M] [crc32:u32]
```

**Operation types:**
- `EntryPut (1)` — key, value, metadata, version, actor
- `EntryDeleted (2)` — key, version, actor (tombstone)
- `NamespaceCreated (10)` — name, config
- `NamespaceDropped (11)` — name
- `NamespaceAltered (12)` — name, config
- `SnapshotCheckpoint (50)` — snapshot_id, entry_count
- `ConfigChanged (100)` — key, value

All payloads are AES-256-GCM encrypted with per-namespace derived keys. CRC32 covers the entire entry.

---

## Key Management

```
Master Key (32 bytes, from env/file)
  │
  │  HKDF-SHA256
  ├──▶ derive_key(master, tenant_ctx, "{namespace}_wal")       → WAL encryption key
  ├──▶ derive_key(master, tenant_ctx, "__snapshot__")          → Snapshot encryption key
  └──▶ derive_key(master, tenant_ctx, "__snapshot_hmac__")     → Snapshot HMAC key
```

**Tenant context**: Included in HKDF derivation from day one. Different tenants get cryptographically isolated storage — different derived keys from the same master key.

**Master key sources** (first success wins):
1. `SHROUDB_MASTER_KEY` environment variable (hex-encoded)
2. `SHROUDB_MASTER_KEY_FILE` file path
3. Ephemeral (dev mode only — data does not survive restart)

---

## ACL Model

```
Token → AuthContext (per-connection)
  ├── tenant_id (HKDF crypto boundary)
  ├── actor (audit trail)
  ├── is_platform (superuser, cross-tenant)
  ├── expires_at (connection TTL)
  └── grants: Vec<Grant>
        └── (namespace, scopes: [Read, Write])
```

**Enforcement:** `Command::acl_requirement()` is an exhaustive match — the compiler ensures every command declares its scope. The dispatcher checks before the handler runs. Handlers never import the ACL crate.

**Scopes:**
- `None` — PING, HEALTH, AUTH, CONFIG GET, COMMAND LIST
- `Admin` — NAMESPACE CREATE/DROP/ALTER, CONFIG SET
- `Read` (per-namespace) — GET, LIST, VERSIONS, NAMESPACE INFO/VALIDATE, SUBSCRIBE
- `Write` (per-namespace) — PUT, DELETE

---

## In-Memory Index

```
KvIndex
└── namespaces: DashMap<String, NamespaceState>
    ├── config: NamespaceConfig (MetaSchema, max_versions, tombstone_retention)
    ├── created_at: u64
    └── keys: DashMap<Vec<u8>, KeyState>
        ├── current_version: u64
        ├── state: Active | Deleted
        ├── created_at: u64
        └── versions: BTreeMap<u64, VersionRecord>
            ├── state, value, metadata
            ├── updated_at, actor
```

---

## Background Tasks

| Task | Interval | What it does |
|------|----------|-------------|
| `snapshot_compactor` | 60s | Takes a snapshot when entry/time threshold exceeded |
| `wal_fsync_batcher` | configurable | Flushes pending WAL writes (Batched/Periodic modes) |
| `metrics_reporter` | 30s | Publishes uptime, namespace count, active keys |

---

## Store Trait

Engines interact with ShrouDB through the `Store` trait (defined in `shroudb-store`):

```rust
pub trait Store: Send + Sync {
    async fn put(&self, ns, key, value, metadata) -> Result<u64>;
    async fn get(&self, ns, key, version) -> Result<Entry>;
    async fn delete(&self, ns, key) -> Result<u64>;
    async fn list(&self, ns, prefix, cursor, limit) -> Result<Page>;
    async fn versions(&self, ns, key, limit, from) -> Result<Vec<VersionInfo>>;
    async fn namespace_create(&self, ns, config) -> Result<()>;
    async fn namespace_drop(&self, ns, force) -> Result<()>;
    // ... + namespace_list, namespace_info, namespace_alter, namespace_validate
    // ... + pipeline, subscribe
}
```

**Two implementations:**
- **EmbeddedStore** — in-process, wraps StorageEngine (standalone engines, Moat)
- **Remote** (via shroudb-client) — over TCP/TLS (multi-service, managed deployments)

---

## Extension Points

### Adding a New Command

1. Add variant to `Command` enum in `shroudb-protocol/src/command.rs`
2. Add `acl_requirement()` match arm (compiler enforces this)
3. Add parsing in `shroudb-protocol/src/resp3/parse_command.rs`
4. Create handler in `shroudb-protocol/src/handlers/`
5. Wire into dispatcher in `shroudb-protocol/src/dispatch.rs`
6. Add client method in `shroudb-client/src/lib.rs`
7. Add CLI help in `shroudb-cli/src/main.rs`
