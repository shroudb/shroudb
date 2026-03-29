# ShrouDB — Scaling Roadmap

## Problem

Single-node ShrouDB is memory-bound. The DashMap index holds all active data in RAM. The ceiling for auth workloads (~500k users) is ~1GB. This will be hit sooner than a full distribution layer can be justified — especially in managed/multi-tenant (Moat) deployments.

The scaling strategy is layered: make each node smarter before adding more nodes.

---

## Phase 1 — Bounded Index (v1.x)

**Goal:** Decouple memory footprint from total dataset size. The DashMap becomes a bounded LRU cache over the existing WAL/snapshot data on disk.

### Behavior

- `get`/`put`/`delete` contract unchanged — engines and clients see no difference.
- Cache hit: served from DashMap (fast path, same as today).
- Cache miss: read from disk (WAL/snapshot), decrypt, deserialize, insert into DashMap, evict coldest entry.
- `put` writes to WAL (unchanged) and inserts into the LRU cache.
- `delete` removes from both cache and WAL (unchanged).

### Observability

- `cache_hit` / `cache_miss` counters per namespace, exposed via shroudb-telemetry.
- Operators monitor miss rate to decide if their memory budget is well-sized.

### Tuning DX

Developers think in terms of instance size, not cache entries. The config should meet them there.

```toml
[store.cache]
# Option A: explicit memory budget
memory_budget = "256mb"

# Option B: fractional — use N% of available system memory
# memory_budget = "70%"

# Option C: omit both — ShrouDB auto-selects based on available memory
# (sensible default, e.g., 50% of available RAM, capped)
```

ShrouDB translates the memory budget into an entry limit internally, based on a running average of entry size. The operator never thinks in terms of entry counts.

For managed platforms (Fly, Railway, etc.) where the container memory is the constraint, the fractional or auto mode means zero config — deploy, and the cache sizes itself to the instance.

### What changes

- DashMap index gains an LRU eviction layer with a memory budget.
- Snapshot/WAL read path becomes a cold-path fallback (already exists, just not used post-startup today).
- New config section: `[store.cache]`.
- New telemetry: cache hit/miss counters.

### What doesn't change

- Store trait contract.
- WAL write path.
- Encryption model.
- Wire protocol.
- Engine code.

---

## Phase 2 — Moat Refactor

**Goal:** Rebuild Moat against ShrouDB v1 architecture. Prerequisite for everything Moat-specific.

Moat was built against v0.1 which had a fundamentally different storage model. The refactor brings Moat onto the v1 Store trait, namespace model, and ACL system.

Scope and detail TBD once v1.x bounded index is stable. Key areas:

- Migrate from v0.1 credential-specific storage to v1 Store trait.
- Adopt v1 namespace model (per-namespace encryption, WAL, compaction).
- Adopt v1 ACL/token model.
- Multi-engine orchestration over the v1 Store.

---

## Phase 3 — Namespace Lifecycle / Scale-to-Zero (Moat)

**Goal:** Idle tenants consume zero memory. Active tenants consume memory proportional to their working set, not their total data.

Depends on: Phase 1 (bounded index) + Phase 2 (Moat refactor).

### Behavior

- Idle namespace → unloaded from memory entirely (zero footprint).
- First request to unloaded namespace → load from disk (WAL replay + snapshot), populate LRU cache with accessed keys only.
- Active namespace → bounded by Phase 1's LRU, so even large tenants have a predictable ceiling.

### Key design areas

- Namespace lifecycle API: `load` / `unload` / `is_loaded` (Moat orchestration layer, above Store trait).
- Idle detection: inactivity timeout per namespace, configurable.
- Cold-start latency: WAL replay time is the cost. Snapshot freshness controls this.
- Telemetry: namespace load/unload events, cold-start duration.

---

## Phase 4 — Distribution (if needed)

**Goal:** Horizontal scale beyond what a single node can handle, even with bounded index.

With Phase 1 pushing the single-node ceiling significantly higher, distribution becomes a much later concern. Documented here so the design direction isn't lost.

### Approach: namespace-based sharding

Namespaces are the natural shard boundary — each already has its own encryption key, WAL, ACL grants, and compaction policy.

### What needs to be built

1. **Shard map** — consistent hashing or explicit namespace → node assignment.
2. **Router** — RESP3 proxy: parse command, extract namespace, forward to shard owner.
3. **RemoteStore** — Store trait over TCP/TLS.
4. **Rebalance orchestrator** — export, import, update map, drain in-flight.
5. **Cross-shard fan-out** — `NAMESPACE LIST` needs all nodes.
6. **Shard-aware replication** — each shard has own primary-replica pair.

### What stays the same

- Storage engine (WAL + snapshots + bounded index).
- Encryption model (per-namespace HKDF).
- Wire protocol (RESP3).
- Store trait contract.
- Auth/ACL model.

---

## Engine Migration (orthogonal)

Engine migration from v0.1 to v0.2 (Store trait) is tracked separately per engine. It is not gated on any of the above — engines can migrate to the v1 Store trait independently.

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
