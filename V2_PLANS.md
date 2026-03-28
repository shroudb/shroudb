# ShrouDB v2 — Distribution & Engine Migration

v2 is about scale, not correctness. v1 must be fully hardened before any v2 work begins.

## Engine migration (v0.2 engines)

Each engine migrates from v0.1 credential-specific storage to v1 Store trait.

### Per-engine pattern

1. Define namespaces (e.g., shroudb-auth: `auth.users`, `auth.sessions`, `auth.tokens`)
2. Map domain types to KV entries with metadata schemas
3. Replace direct WAL/index access with `Store::put`/`get`/`delete`
4. Register metadata schemas via `NAMESPACE CREATE ... SCHEMA`
5. Use `SUBSCRIBE` for internal event-driven workflows

### Engines

| Engine | Namespaces | Notes |
|--------|-----------|-------|
| shroudb-auth | `auth.users`, `auth.sessions`, `auth.tokens` | Password hashes in value, profile in metadata |
| shroudb-transit | `transit.keys`, `transit.datakeys` | Encryption keys double-encrypted in value |
| shroudb-veil | `veil.tokens`, `veil.mappings` | Tokenization mappings |
| shroudb-mint | `mint.keys`, `mint.certs` | API key + certificate storage |
| shroudb-sentry | `sentry.policies`, `sentry.audit` | Policy documents, audit events |
| shroudb-keep | `keep.secrets` | Generic secret storage |
| shroudb-courier | `courier.templates`, `courier.queue` | Notification templates + delivery queue |
| shroudb-pulse | `pulse.checks`, `pulse.incidents` | Health check configs + incident history |

### Deployment modes

```toml
[store]
mode = "embedded"  # or "remote"
# Embedded:
data_dir = "./data"
# Remote:
uri = "shroudb+tls://token@shroudb.internal:6399"
```

Store trait abstracts the difference. Engine code doesn't change between modes.

## Distribution

### Problem

Single-node ShrouDB is memory-bound. The DashMap index holds all active data in RAM. The ceiling for auth workloads (~500k users) is ~1GB. Managed multi-tenant deployments (Moat) will exceed this.

### Approach: namespace-based sharding

Namespaces are the shard boundary. Each already has its own encryption key, WAL entries, ACL grants, and compaction policy.

**Shard routing:** Namespace prefix → node assignment. Router parses command, extracts namespace, forwards to owning node.

**Rebalancing:** Export from source, import to destination, update shard map. Infrastructure already exists.

### What needs to be built

1. **Shard map** — consistent hashing or explicit namespace → node assignment
2. **Router** — RESP3 proxy: parse command, extract namespace, forward to shard owner
3. **RemoteStore** — Store trait over TCP/TLS (the client library is most of this)
4. **Rebalance orchestrator** — export, import, update map, drain in-flight
5. **Cross-shard fan-out** — NAMESPACE LIST needs all nodes
6. **Shard-aware replication** — each shard has own primary-replica pair

### What stays the same

- Storage engine (WAL + snapshots)
- Encryption model (per-namespace HKDF)
- Wire protocol (RESP3)
- Store trait contract
- Auth/ACL model
