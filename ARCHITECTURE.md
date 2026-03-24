# ShrouDB Architecture

A concise guide for contributors. See `PROJECT.md` for the full project plan and architectural commitments.

---

## Crate Map

```
shroudb (binary)
├── shroudb-core         Data model: Keyspace, Credential, MetaSchema, types
├── shroudb-crypto       All cryptography: AEAD, HKDF, JWT, HMAC, API key hashing
├── shroudb-storage      WAL, snapshots, recovery, key manager, in-memory index
│   ├── wal/           Write-ahead log (writer, reader, segments, entry format)
│   ├── snapshot/      Periodic snapshots (writer, reader, format)
│   └── index/         In-memory indexes (api_key, refresh_token, revocation, signing_key)
├── shroudb-protocol     RESP3 codec, command parsing, dispatch, handlers, auth
│   ├── resp3/         Wire format parser/serializer
│   └── handlers/      One file per command (issue, verify, revoke, rotate, ...)
├── shroudb-client       Typed async Rust client library (RESP3 over TCP/TLS)
└── shroudb-cli          Interactive REPL (wraps shroudb-client, adds tab completion)
```

### Dependency Graph

```
shroudb-core  <──  shroudb-crypto  <──  shroudb-storage  <──  shroudb-protocol  <──  shroudb (bin)
                                                    │
                                          shroudb-client  <──  shroudb-cli (bin)
```

`shroudb-client` and `shroudb-cli` are pure TCP clients. They do NOT depend on `shroudb-storage` or `shroudb-protocol` — they speak RESP3 over the wire.

---

## Data Flow

```
Client
  │
  │  TCP / TLS
  ▼
Connection (shroudb/src/connection.rs)
  │
  │  Raw bytes
  ▼
RESP3 Parser (shroudb-protocol/src/resp3/)
  │
  │  Vec<String> args
  ▼
Command Parser (shroudb-protocol/src/command.rs)
  │
  │  Command enum variant
  ▼
Dispatcher (shroudb-protocol/src/dispatch.rs)
  │
  │  Auth check → replica classification → route
  ▼
Handler (shroudb-protocol/src/handlers/{issue,verify,...}.rs)
  │
  │  Business logic + validation
  ▼
Storage Engine (shroudb-storage/src/engine.rs)
  ├──▶ WAL Writer  ──▶  Append entry to segment file (encrypted)
  ├──▶ In-Memory Index  ──▶  Update DashMap-based indexes
  └──▶ Snapshot Writer  ──▶  Periodic full-state dump (encrypted)
```

### Read Path vs Write Path

Commands are classified at the dispatch level (see `Command::replica_behavior()`):

- **PureRead**: INSPECT, JWKS, KEYSTATE, HEALTH, KEYS, SCHEMA, CONFIG GET — index lookup only
- **ObservationalRead**: VERIFY — reads index, optionally updates `last_verified_at`
- **ConditionalWrite**: PASSWORD VERIFY — verify on replicas (read-only), may rehash on primary (write)
- **WriteOnly**: ISSUE, REVOKE, ROTATE, REFRESH, UPDATE, SUSPEND, UNSUSPEND, PASSWORD SET, PASSWORD CHANGE, PASSWORD IMPORT — WAL append + index update

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
│       ├── snap_20240322_143000_abcd1234.bin    # Encrypted snapshot
│       └── snap_20240301_120000_efgh5678.bin
├── audit.log                   # Structured JSON audit log
└── (no other files)
```

**Namespace** defaults to `"default"` in single-tenant mode. Exists to support future multi-tenant partitioning without restructuring paths.

### WAL Entry Format (v1)

```
[len:u32] [version:u8] [flags:u8] [keyspace_id:...] [op_type:u8] [timestamp:u64] [payload:...] [crc32:u32]
```

- `version` enables safe format evolution
- `flags` reserved for future use (compression, extended headers)
- All payload data is AES-256-GCM encrypted with per-keyspace derived keys
- CRC32 covers the entire entry for corruption detection

### Snapshot Format

Binary header + encrypted body:

- **Header**: `version: u16`, `encoding: String` ("postcard-v1"), `created_at`, `snapshot_id`, `namespace`, `wal_checkpoint`, `keyspace_count`, `total_credentials`
- **Body**: Postcard-serialized state, AES-256-GCM encrypted with snapshot-specific derived key, HMAC integrity check

---

## Key Management

```
Master Key (32 bytes, from env/file/KMS)
  │
  │  HKDF-SHA256
  ├──▶ derive_key(master, tenant_ctx, "{keyspace}_wal")      → WAL encryption key
  ├──▶ derive_key(master, tenant_ctx, "{keyspace}_private")  → Private key wrapping key
  ├──▶ derive_key(master, tenant_ctx, "__snapshot__")        → Snapshot encryption key
  ├──▶ derive_key(master, tenant_ctx, "__snapshot_hmac__")   → Snapshot HMAC key
  └──▶ derive_key(master, tenant_ctx, "__export__")          → Export bundle key
```

**Double-layer encryption for private keys**: JWT/HMAC private key material is encrypted with a per-keyspace derived key before being written to the WAL or snapshot. The WAL entry itself is also encrypted. This means private keys are encrypted twice — once at the application layer and once at the storage layer.

**Master key sources** (chained, first success wins):
1. `SHROUDB_MASTER_KEY` environment variable (hex-encoded)
2. `SHROUDB_MASTER_KEY_FILE` file path
3. Ephemeral (dev mode only — data does not survive restart)

**Tenant context**: Currently hardcoded to `"default"`. Multi-tenant deployments will replace this with a tenant ID. The HKDF derivation chain includes it from day one to avoid re-deriving every key during migration.

---

## Background Tasks

All scheduled tasks run on 60-second intervals (30s for metrics) and respect the shutdown signal.

| Task                  | What It Does                                                         |
|-----------------------|----------------------------------------------------------------------|
| `revocation_reaper`   | Prunes expired entries from per-keyspace revocation sets             |
| `idempotency_reaper`  | Removes expired idempotency keys (5-minute window)                   |
| `snapshot_compactor`  | Takes a snapshot when entry threshold or time threshold is exceeded  |
| `rotation_scheduler`  | Checks key age against `rotation_days` policy, triggers ROTATE       |
| `refresh_token_reaper`| Removes expired refresh tokens from all keyspaces                    |
| `metrics_reporter`    | Publishes per-keyspace gauges to Prometheus (credentials, key age)   |

### Password Metrics

Password keyspaces emit dedicated counters in the verify hot path:

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `shroudb_password_verify_failed_total` | counter | keyspace | Failed password verifications (invalid password) |
| `shroudb_password_lockout_total` | counter | keyspace | Verify attempts rejected by rate limiter |
| `shroudb_password_rehash_total` | counter | keyspace | Transparent rehashes due to stale hash parameters |
| `config_reloader`     | Hot-reloads config file changes (currently: keyspace `disabled` flag)|
| `wal_fsync_batcher`   | Flushes pending WAL writes for Batched/Periodic fsync modes          |

---

## Extension Points

### Adding a New Command

1. Add a variant to `Command` enum in `shroudb-protocol/src/command.rs`
2. Add parsing logic in `shroudb-protocol/src/resp3/parse_command.rs`
3. Create handler file in `shroudb-protocol/src/handlers/` (follow existing patterns)
4. Register the handler in `shroudb-protocol/src/handlers/mod.rs`
5. Wire it into the dispatcher in `shroudb-protocol/src/dispatch.rs`
6. Classify as PureRead/ObservationalRead/WriteOnly in `Command::replica_behavior()`
7. Add client method in `shroudb-client/src/lib.rs`
8. Add CLI help text in `shroudb-cli/src/main.rs`

### Adding a New Keyspace Type

1. Add variant to `KeyspaceType` enum in `shroudb-core/src/keyspace_type.rs`
2. Add variant to `KeyspacePolicy` enum in `shroudb-core/src/keyspace.rs`
3. Add credential type in `shroudb-core/src/credential/`
4. Add index type in `shroudb-storage/src/index/`
5. Add WAL entry serialization in `shroudb-storage/src/wal/entry.rs`
6. Add snapshot serialization in `shroudb-storage/src/snapshot/format.rs`
7. Update ISSUE/VERIFY/REVOKE handlers to dispatch on the new type
8. Add config parsing in `shroudb/src/config.rs`

### Adding a New Crypto Algorithm

1. Implement in `shroudb-crypto/src/` (follow `jwt.rs` or `hmac.rs` patterns)
2. Add algorithm variant to the appropriate enum in `shroudb-core`
3. Update the relevant handler to support the new algorithm
4. Ensure key generation, signing, and verification are all covered
