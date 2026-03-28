# ShrouDB v2 — Plans

## Starting point: v1 reality check

v1 is feature-complete in the sense that the code exists and compiles. But only 2 of 14 features have tests that prove they work against a real server. The rest are "code exists, wired up, zero validation."

### What is proven to work (tested against real server)

| Feature | Evidence |
|---------|----------|
| PUT/GET/DELETE/LIST/VERSIONS | Smoke test spins up server, exercises full CRUD + version history |
| Auth + ACL | Integration tests verify unauthorized rejection, per-namespace grants |

### What exists but has zero automated proof

| Feature | Risk |
|---------|------|
| SUBSCRIBE | Push frame serialization, event delivery timing, disconnect handling |
| Webhooks | HMAC signing correctness, retry backoff, event filtering, HTTP delivery |
| Pipeline | Nested array parsing, dispatcher execution, response assembly |
| Export/Import | Encryption roundtrip, namespace rename, cross-instance portability |
| Config hot-reload | Token swap atomicity, rate limit propagation, broken config handling |
| Tombstone compaction | Retention timing, key removal, WAL replay of compaction entries |
| Telemetry | Audit log file creation, event routing, OTEL export |
| Idempotency | Dedup on retry through real server (unit tests exist for the map) |
| Rate limiting | Token bucket behavior, burst prevention, hot-reload |
| TLS | Handshake, cert validation, mTLS, plaintext rejection |
| Rekey | Data survives key rotation, old key rejected, version history preserved |
| Doctor | Detects corrupt WAL, missing key, bad config |

---

## Phase 0: Prove v1 works (before anything else)

No new features until the existing ones are proven. This means integration tests that spin up a real server process, connect via the client library (or raw TCP), and exercise happy paths, error paths, and edge cases.

### Test infrastructure needed

A test harness that:
1. Starts a `shroudb` server process on a random port with a temp data directory
2. Generates an ephemeral master key
3. Optionally configures auth tokens, TLS certs, rate limits, webhooks
4. Returns a connected `ShrouDBClient` (or raw TCP stream for low-level tests)
5. Cleans up on drop

This replaces the deleted v0.1 `TestServer`/`TestClient` infrastructure with a v1 equivalent.

### Test plan

**Core data path (extend existing smoke test):**
- PUT with metadata, GET with META flag, verify metadata roundtrip
- PUT same key multiple times, verify version increments
- DELETE, verify GET returns not-found, VERSIONS shows tombstone
- LIST with PREFIX filter, CURSOR pagination, LIMIT
- VERSIONS with LIMIT and FROM
- Error paths: GET on nonexistent namespace, DELETE on nonexistent key, PUT to dropped namespace

**Pipeline:**
- Send nested RESP3 array with 3 commands (PUT, GET, DELETE)
- Verify response is array of 3 sub-responses in order
- Send pipeline with REQUEST_ID, verify response
- Resend same REQUEST_ID, verify cached response returned
- Send pipeline with invalid sub-command, verify PipelineAborted error

**SUBSCRIBE:**
- Subscribe to namespace, PUT a key from another connection, verify push frame arrives
- Subscribe with KEY filter, PUT non-matching key, verify no event
- Subscribe with EVENTS filter, DELETE a key, verify only delete events
- UNSUBSCRIBE, verify normal command processing resumes
- Disconnect during subscription, verify server doesn't crash

**Auth + ACL (extend existing):**
- Expired token rejected
- Token with read-only grant can GET but not PUT
- Wildcard `*` grant allows all namespaces
- AUTH with invalid token, verify error
- Commands before AUTH (when auth required), verify rejection

**Rate limiting:**
- Configure rate_limit_per_second = 10
- Send 20 commands rapidly, verify some get rate limit errors
- Wait for refill, verify commands succeed again

**TLS:**
- Start server with TLS cert/key
- Connect via TLS, verify commands work
- Connect via plain TCP, verify rejection
- Start with mTLS, connect without client cert, verify rejection

**Config hot-reload:**
- Start server with auth token A
- Modify config file to replace with token B
- Wait for reload (>10s)
- Verify token A is rejected, token B works
- Modify rate limit in config, verify new connections use new limit

**Webhooks:**
- Start a mock HTTP server alongside ShrouDB
- Configure webhook pointing to mock server
- PUT a key, verify mock server receives POST with correct HMAC signature
- Verify X-ShrouDB-Event header matches
- Stop mock server, PUT a key, verify retries happen (check logs)

**Export/Import:**
- Create namespace with 10 keys (some with versions, some tombstoned)
- Export to file
- Drop namespace
- Import from file, verify all keys + versions restored
- Import with --namespace rename, verify new name
- Import into existing namespace, verify error

**Rekey:**
- Create data with master key A
- Stop server
- Run rekey --old-key A --new-key B
- Start server with master key B, verify all data accessible
- Verify master key A no longer works

**Doctor:**
- Run doctor on healthy data, verify success
- Corrupt a WAL segment, run doctor, verify failure reported
- Remove master key, run doctor, verify key error reported

**Tombstone compaction:**
- Create namespace with tombstone_retention_secs = 1
- PUT + DELETE a key
- Wait 2 seconds + compaction interval
- Verify key is removed from index (not just tombstoned)
- Restart server, verify compaction survives recovery

**Telemetry:**
- Start server with data_dir
- Send a PUT command
- Verify audit.log file exists and contains the PUT event as JSON

---

## Phase 1: Engine migration (v0.2)

With v1 proven, migrate all engines from v0.1 credential-specific storage to v1 Store trait.

### Per-engine migration pattern

Each engine:
1. Defines its namespaces (e.g., shroudb-auth uses `auth.users`, `auth.sessions`)
2. Maps its domain types to KV entries with metadata schemas
3. Replaces direct WAL/index access with `Store::put`/`get`/`delete`
4. Registers metadata schemas via `NAMESPACE CREATE ... SCHEMA`
5. Uses `SUBSCRIBE` for internal event-driven workflows (e.g., session cleanup on user delete)

### Engines to migrate

| Engine | Namespaces | Key migration notes |
|--------|-----------|---------------------|
| shroudb-auth | `auth.users`, `auth.sessions`, `auth.tokens` | Password hashes in value, user profile in metadata |
| shroudb-transit | `transit.keys`, `transit.datakeys` | Encryption keys in value (double-encrypted) |
| shroudb-veil | `veil.tokens`, `veil.mappings` | Tokenization mappings |
| shroudb-mint | `mint.keys`, `mint.certs` | API key + certificate storage |
| shroudb-sentry | `sentry.policies`, `sentry.audit` | Policy documents, audit events |
| shroudb-keep | `keep.secrets` | Generic secret storage |
| shroudb-courier | `courier.templates`, `courier.queue` | Notification templates + delivery queue |
| shroudb-pulse | `pulse.checks`, `pulse.incidents` | Health check configs + incident history |

### Deployment modes

Each engine can run as:
- **Embedded** — `EmbeddedStore` in-process, single binary, own data directory
- **Remote** — `ShrouDBClient` connects to a shared ShrouDB server over TCP/TLS

The Store trait abstracts the difference. Config determines which mode:
```toml
[store]
mode = "embedded"  # or "remote"
# Embedded:
data_dir = "./data"
# Remote:
uri = "shroudb+tls://token@shroudb.internal:6399"
```

### Moat integration

shroudb-moat embeds all engines in a single binary. Each engine gets its own namespace prefix. Moat passes a shared `EmbeddedStore` (or `RemoteStore` in managed mode) to all engines.

---

## Phase 2: Distribution

### Problem

Single-node ShrouDB is memory-bound. The in-memory DashMap index holds all active data. Once it exceeds available RAM, the node can't serve reads.

For the engine workloads (auth, transit, etc.), this ceiling is high — 500k users with sessions is ~1GB RAM. But managed multi-tenant deployments (Moat hosting many tenants) will hit it.

### Approach: namespace-based sharding

Namespaces are the natural shard boundary. Each namespace already has:
- Its own encryption key (HKDF-derived)
- Its own WAL entries
- Its own ACL grants
- Its own compaction policy

**Shard routing:** A shard map assigns namespace prefixes to nodes. `auth.users` → node A, `transit.keys` → node B. The router is a thin proxy that parses the command, extracts the namespace, and forwards to the owning node.

**Rebalancing:** Move a namespace between nodes by exporting from source, importing to destination, updating the shard map. The export/import infrastructure already exists.

### What needs to be built

1. **Shard map** — consistent hashing or explicit assignment of namespace → node
2. **Router** — RESP3 proxy that parses commands, extracts namespace, forwards to shard owner
3. **RemoteStore** — Store trait implementation that talks to a ShrouDB server over TCP
4. **Rebalance orchestrator** — export from source, import to destination, update shard map, drain in-flight requests
5. **Cross-shard operations** — NAMESPACE LIST needs to fan out to all nodes
6. **Shard-aware replication** — each shard has its own primary-replica pair

### What does NOT change

- Storage engine (WAL + snapshots)
- Encryption model (per-namespace HKDF keys)
- Wire protocol (RESP3)
- Store trait (engines don't know they're sharded)
- Auth/ACL model

---

## Phase 3: Operational maturity

- **Fuzz testing in CI** — the fuzz targets exist (`command_parser`, `acl_check`, `meta_schema_validate`), need nightly runner
- **Chaos testing** — kill server mid-write, corrupt WAL segments, verify recovery
- **Performance benchmarks** — establish baseline for PUT/GET/LIST latency and throughput at various data sizes
- **Timing analysis** — verify constant-time token validation with actual timing measurements
- **TLS testing** — testssl.sh against server, verify no weak ciphers
- **Threat model document** — formal threat model covering key management, network exposure, side channels
- **Audit trail completeness** — verify every write operation produces an audit event

---

## Non-goals

- **Replacing RocksDB/SQLite** — ShrouDB is not a general-purpose embedded database. It's the storage layer for the ShrouDB engine ecosystem.
- **Horizontal write scaling** — the target workloads are read-heavy. Distribution addresses the memory ceiling, not write throughput.
- **SQL or complex queries** — KV with metadata schemas is the data model. If you need relational queries, use a relational database.
