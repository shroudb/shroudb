# Replication Plan

## Status: Pre-decision

This document exists to structure thinking about replication — not to commit to building it. The first phase is instrumentation: collect the data needed to decide whether replication is warranted, and if so, what topology fits.

---

## Phase 0: Metrics That Justify (or Kill) Replication

Before designing anything, answer these questions with data from real deployments.

### Metrics to add

These metrics were implemented as part of Phase 0 instrumentation. They are now emitted from the command dispatch path, connection handler, storage engine, and metrics reporter.

| Metric | Type | Labels | Why it matters |
|--------|------|--------|----------------|
| `keyva_commands_by_behavior_total` | counter | `behavior={PureRead,ObservationalRead,ConditionalWrite,WriteOnly}` | Read/write ratio. If writes dominate, replication buys nothing. If reads are 95%+, replicas have clear value. |
| `keyva_verify_rate` | histogram | `keyspace` | VERIFY is the hot path that replicas would serve. If it's not hot, replicas are overhead. |
| `keyva_concurrent_connections` | gauge | — | Saturation signal. Single-node may be fine at 50 connections; not at 5,000. |
| `keyva_command_queue_depth` | gauge | — | If commands are queuing behind the WAL mutex, that's a write bottleneck replicas won't fix. |
| `keyva_wal_write_duration_seconds` | histogram | `keyspace` | Write latency. If WAL writes are fast, the bottleneck is elsewhere. |
| `keyva_wal_segment_bytes_total` | counter | — | WAL throughput in bytes/sec. Determines bandwidth needed for WAL shipping. |
| `keyva_snapshot_size_bytes` | gauge | — | Bootstrap cost for new replicas. If snapshots are 10MB, bootstrap is trivial. If 10GB, it's a production event. |
| `keyva_revocation_check_duration_seconds` | histogram | `keyspace` | If revocation checks are slow, stale replicas become a security problem faster. |

### Existing metrics that already inform the decision

These are already emitted (from `scheduler.rs` and `dispatch.rs`):

- `keyva_commands_total{command, keyspace, result}` — command volume and error rates
- `keyva_command_duration_seconds{command, keyspace}` — latency per command type
- `keyva_wal_entries_since_snapshot` — WAL growth rate
- `keyva_credentials_total{keyspace, type}` — dataset size
- `keyva_revocations_active{keyspace}` — revocation set pressure
- `keyva_uptime_seconds` — availability baseline

### Decision thresholds

Replication is likely justified when:

- **Read/write ratio exceeds 10:1** — replicas absorb read load without touching the primary
- **VERIFY p99 latency exceeds 10ms** under normal load — indicates contention
- **Connection count regularly exceeds the Tokio thread pool** — horizontal scaling needed
- **Uptime requirements demand failover** — this is the non-performance reason; even a fast single node is a SPOF

Replication is likely NOT justified when:

- Read/write ratio is below 3:1 — most load is writes, which must hit the primary anyway
- Single-node VERIFY throughput is well within capacity — premature scaling
- The deployment is internal/non-critical — restart tolerance > 0

### Phase 0 deliverable

All Phase 0 metrics are live and ready for dashboard creation. The instrumentation is in place across the command dispatch path, connection handler, storage engine, and metrics reporter.

A Grafana dashboard (or equivalent) with panels for:
1. Read vs. write command ratio over time
2. VERIFY latency percentiles (p50, p95, p99) per keyspace
3. WAL write rate (entries/sec and bytes/sec)
4. Connection count over time
5. Snapshot size trend

Run this for 2-4 weeks against a real workload before proceeding to Phase 1.

---

## Phase 1: Topology Decision

After Phase 0 data is in hand, choose a topology.

### Option A: Single-leader WAL shipping (recommended starting point)

```
Primary (read/write)
  │
  ├──WAL stream──→ Replica 1 (read-only)
  ├──WAL stream──→ Replica 2 (read-only)
  └──WAL stream──→ Replica N (read-only)
```

**How it works:**
- Primary appends WAL entries as it does today
- Replicas connect to primary over a dedicated TCP stream
- Primary sends WAL entries (still encrypted, still self-describing) as they're written
- Replicas decrypt, apply to their in-memory index, discard (no local WAL needed for read-only replicas)

**Why this fits Keyva:**
- WAL entries are already self-describing (keyspace ID, op type, full payload) — the format was designed for this
- Command classification already separates reads from writes — replicas just refuse `WriteOnly` commands
- Encryption keys must be shared (replicas need the master key or derived keys to decrypt WAL entries)
- No consensus protocol needed — single leader, no elections, no split-brain

**Limitations:**
- No automatic failover. Primary dies → manual promotion or operator intervention
- Replication lag is unbounded (no acknowledgment protocol)
- All writes still hit one node

### Option B: Single-leader with synchronous acknowledgment

Same as Option A, but the primary waits for at least one replica to acknowledge before responding to the client.

**Tradeoff:** Higher write latency in exchange for durability guarantee (data exists on 2+ nodes before client gets OK).

**When to choose this:** When the deployment cannot tolerate any data loss on primary failure. Cost: every write pays one network RTT to the nearest replica.

### Option C: Multi-leader (NOT recommended)

Multiple nodes accept writes. Conflict resolution required.

**Why not:** Credential operations are not commutative. Two nodes both issuing `ROTATE` for the same keyspace creates divergent key rings. Two nodes both revoking the same credential is fine (idempotent), but two nodes issuing credentials with the same request ID may produce different outputs. The conflict resolution complexity is not worth it for the use case.

### Decision criteria

| Signal from Phase 0 | Recommended topology |
|---|---|
| High read ratio, low write volume, restart tolerance > 0 | Option A |
| High read ratio, zero data loss requirement | Option B |
| Multi-region requirement | Option A per region, with cross-region async shipping |
| Write throughput is the bottleneck | Replication won't help. Optimize the WAL path instead. |

---

## Phase 2: Replica Bootstrap

A new replica needs to reach a consistent state before it can serve reads.

### Bootstrap protocol

1. Replica connects to primary, sends `REPLICATE` handshake
2. Primary responds with latest snapshot (encrypted, same format as on-disk snapshots)
3. Primary notes the WAL checkpoint at the time of snapshot
4. Primary begins streaming WAL entries from that checkpoint forward
5. Replica loads snapshot into its in-memory index
6. Replica applies buffered WAL entries
7. Replica transitions to `ready` state and begins serving reads

### Open questions

- **Snapshot transfer mechanism:** Inline over the replication TCP stream, or out-of-band (S3, shared filesystem)? Inline is simpler. Out-of-band scales better for large datasets.
- **Snapshot size gating:** If `keyva_snapshot_size_bytes` (Phase 0) is large, bootstrap could take minutes. Need a progress indicator and health state (`bootstrapping` → `catching_up` → `ready`).
- **Encryption key distribution:** Replicas need the same master key (or derived per-keyspace keys). This is a deployment/operational concern, not a protocol concern. Document it clearly.

---

## Phase 3: WAL Streaming Protocol

### Wire format for replication stream

The replication stream carries the same `WalEntry` bytes that are written to disk. No new format needed. The framing is:

```
[entry_len:u32] [wal_entry_bytes:N]
```

This is identical to the on-disk WAL segment format. A replica's replication reader is the same code path as crash recovery replay.

### Heartbeats and lag detection

- Primary sends a heartbeat frame every 5 seconds if no WAL entries have been sent
- Heartbeat carries the primary's current WAL position (segment_seq, byte_offset)
- Replica compares its position to the heartbeat to compute lag
- Replica exposes `keyva_replication_lag_seconds` gauge
- Replica exposes `keyva_replication_lag_entries` gauge

### Backpressure

If a replica can't keep up:
- Primary buffers up to N entries (configurable, default 10,000) per replica
- If buffer overflows, primary drops the replica connection
- Replica must re-bootstrap from snapshot

This avoids unbounded memory growth on the primary.

---

## Phase 4: Replica Behavior

### Command routing

Already implemented via `Command::replica_behavior()`:

| Classification | Primary | Replica |
|---|---|---|
| `PureRead` | Execute normally | Execute normally |
| `ObservationalRead` | Execute with side effects | Execute without side effects (skip `last_verified_at` update) |
| `ConditionalWrite` | Execute normally | Execute read portion only (PASSWORD VERIFY returns result, skips rehash) |
| `WriteOnly` | Execute normally | Return error: `READONLY You can't write against a read-only replica` |

### Staleness and revocation propagation

This is the security-critical question.

**Problem:** A token is revoked on the primary. Until the WAL entry propagates, replicas will still verify it as valid.

**Options:**

1. **Accept the window.** Document it. Replication lag = revocation propagation delay. If lag is typically <100ms, this is acceptable for most use cases.

2. **Staleness budget.** Replica refuses VERIFY if `keyva_replication_lag_seconds > max_staleness` (configurable). Fail-closed: return an error rather than a potentially stale answer. Clients retry against the primary.

3. **Revocation forwarding.** On REVOKE, primary pushes a lightweight "revocation hint" to all replicas out-of-band (separate from WAL stream, lower latency). Replicas apply revocation immediately, then receive the full WAL entry later for durability.

**Recommendation:** Start with option 2 (staleness budget). It's simple, fail-closed (consistent with Keyva's failure posture), and requires no new protocol. Option 3 is an optimization for later if the staleness window proves too large.

---

## Phase 5: Failover

### Manual failover (Phase 5a — build first)

1. Operator stops the primary
2. Operator identifies the most caught-up replica (lowest `keyva_replication_lag_entries`)
3. Operator promotes it: `keyva promote --config config.toml`
4. Promoted replica starts accepting writes, opens WAL writer
5. Other replicas reconnect to the new primary

**This is sufficient for v1.** Automatic failover adds complexity (leader election, fencing, split-brain prevention) that is only justified at scale.

### Automatic failover (Phase 5b — defer)

Requires a consensus mechanism for leader election. Options:

- **External coordinator** (etcd, Consul, ZooKeeper): Keyva nodes register, coordinator manages leases. Simplest, but adds a dependency — violates the single-binary philosophy.
- **Embedded Raft**: Keyva nodes elect a leader among themselves. No external dependencies, but significant implementation effort and testing surface.
- **Witness-based**: A lightweight witness process (not a full replica) breaks ties. Smaller than full Raft, but still new code.

**Recommendation:** Defer. Manual failover with good operational tooling (`keyva status --replication`, `keyva promote`) covers most deployments. Revisit when there's demand for sub-minute automated recovery.

---

## Phase 6: Observability

### Metrics to add for replication

| Metric | Node | Type | Description |
|--------|------|------|-------------|
| `keyva_replication_lag_seconds` | replica | gauge | Time since last applied WAL entry |
| `keyva_replication_lag_entries` | replica | gauge | WAL entries behind primary |
| `keyva_replication_connected` | replica | gauge | 1 if connected to primary, 0 if not |
| `keyva_replication_bootstrap_duration_seconds` | replica | gauge | Time spent in last bootstrap |
| `keyva_replica_count` | primary | gauge | Number of connected replicas |
| `keyva_replication_bytes_sent_total` | primary | counter | WAL bytes shipped to replicas |
| `keyva_replication_entries_sent_total` | primary | counter | WAL entries shipped to replicas |
| `keyva_replica_buffer_depth` | primary | gauge | Per-replica send buffer depth |
| `keyva_verify_stale_rejected_total` | replica | counter | VERIFYs rejected due to staleness budget |

### HEALTH command on replicas

```
> HEALTH
< {status: ok, role: replica, lag_seconds: 0.03, lag_entries: 12, primary: "10.0.1.5:6399", state: ready}
```

States: `bootstrapping` → `catching_up` → `ready` → `stale` (if lag exceeds budget)

---

## Architectural Constraints (already satisfied)

These are prerequisites from PROJECT.md that are already implemented:

- [x] WAL entries are self-describing (keyspace ID, op type, full payload, timestamp)
- [x] Commands classified via `Command::replica_behavior()`
- [x] Read path does not assume write capability
- [x] WAL format versioned (v1), with version field for safe evolution
- [x] Snapshot format includes WAL checkpoint position
- [x] Idempotency keys in WAL payloads for dedup on replay
- [x] Fail-closed failure posture

---

## What This Plan Does NOT Cover

- **Multi-region replication.** Same mechanism (WAL shipping over TCP/TLS), but cross-region adds latency, partition tolerance, and regional failover semantics. Design after single-region replication is proven.
- **Multi-tenant replication topology.** Per-tenant replication policies (some tenants replicated, others not). Design after multi-tenancy is built.
- **Consensus-based automatic failover.** See Phase 5b. Deferred intentionally.
- **Read-your-writes consistency.** Client issues REVOKE, then immediately VERIFYs on a replica. The replica may not have the revocation yet. Solution: client sticks to primary for the read-after-write window, or uses a session token that routes to primary. This is a client-side concern, not a replication protocol concern.

---

## Summary

| Phase | What | Depends on | Build when |
|-------|------|------------|------------|
| 0 | Instrumentation + metrics | Nothing | Now |
| 1 | Topology decision | Phase 0 data | After 2-4 weeks of production metrics |
| 2 | Replica bootstrap | Phase 1 decision | When replication is committed |
| 3 | WAL streaming protocol | Phase 2 | When replication is committed |
| 4 | Replica command behavior | Phase 3 | When replication is committed |
| 5a | Manual failover | Phase 4 | When replication is committed |
| 5b | Automatic failover | Phase 5a + demand | When manual failover is insufficient |
| 6 | Replication observability | Phase 3 | Alongside Phase 3-5a |
