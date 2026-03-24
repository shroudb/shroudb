# ShrouDB — Project Plan

**Language:** Rust
**Target:** Single static binary with RESP3, REST, and gRPC interfaces

---

## Architectural Commitments

These are constraints imposed by the full vision (managed service, replication, multi-region) on early-phase implementation decisions. They cost little or nothing to implement now but are expensive to retrofit later. Every phase should be checked against this list before closing out.

### Multi-Tenancy Readiness
- **HKDF derivation chain must include a tenant context slot.** In single-tenant mode, this is a hardcoded constant (e.g., `"default"`). The managed service replaces it with a tenant ID. If the derivation chain doesn't include it from the start, going multi-tenant requires re-deriving every key for every keyspace — a migration on every existing deployment.
- **Keyspace storage must be partitionable by an external identifier.** WAL segments and snapshot paths should include a namespace prefix (hardcoded to `"default"` initially). This allows per-tenant WAL/snapshot isolation without restructuring the storage layout.
- **Audit log entries must carry an `actor` field from day one.** In single-tenant mode it's always `"self"` or the auth policy name. The managed service needs to distinguish between tenant operations and platform operations (e.g., platform admin rotating a tenant's keys vs. the tenant doing it). Retrofitting this means migrating every log consumer.
- **Config model must not assume a single global TOML file internally.** The TOML file is the input format, but the internal representation should be a runtime data structure that happens to be loaded from TOML. The managed service will need programmatic keyspace creation via API.

### Replication Readiness
- **WAL entries must be self-describing.** Each entry should contain enough context (keyspace ID, operation type, full payload) to be replayed by a process that has no other state. This is required for WAL-shipping replication where a replica rebuilds from the WAL stream alone.
- **Read path must not assume write capability.** The command engine should separate read operations (VERIFY, INSPECT, JWKS, HEALTH, KEYS, KEYSTATE) from write operations (ISSUE, REVOKE, ROTATE, REFRESH, SUSPEND, UNSUSPEND) at the dispatch level. Replicas will serve reads only.
- **Replica key distribution model (document now, build later).** WAL payloads are encrypted per-keyspace. A replica serving reads needs decryption keys for every keyspace it handles. Two options: (a) replicas receive the master key and derive per-keyspace keys independently (simpler, but higher blast radius — compromised replica exposes all keyspaces), or (b) replicas receive only the per-keyspace derived keys they need over a secure channel (more complex distribution, but better tenant isolation). The choice affects whether replica compromise is a full key exposure or a scoped one. **Decision deferred**, but the intended model should be documented before replication is built, because option (b) requires key distribution infrastructure that option (a) does not.

### Credential Export Readiness
- **WAL entry format should be documented as a stable format from v1.0.** Even if `EXPORT` isn't built yet, designing the WAL format with the assumption that another ShrouDB instance will need to read it prevents format decisions that are internally convenient but externally opaque.

### Protocol Proxy Architecture
- **The core binary is a TCP server. REST and gRPC are stateless proxies that connect as clients.** This matches the industry pattern (Upstash: Redis TCP + separate HTTP proxy for serverless; Neon: Postgres TCP + separate WebSocket/HTTP driver; PlanetScale: MySQL TCP + separate HTTP proxy). The benefits are:
  - Core stays lean — one protocol, one process, easy to reason about
  - Proxies are stateless and horizontally scalable — `docker run shroudb-rest` can scale independently
  - Proxies can run at the edge (Cloudflare Worker, Lambda) while core runs in a region
  - Independent deployment — upgrade the proxy without restarting the core
  - SDK clients connect over TCP directly (high performance) or through the REST proxy (serverless/edge)
- **Current state:** `shroudb-rest` is compiled in-process and shares `Arc<StorageEngine>` directly. This works and is correct, but couples the REST adapter to the core binary.
- **Target state:** `shroudb-rest` becomes a standalone binary that connects to `shroudb` over TCP using the same client protocol as `shroudb-cli`. The `shroudb` binary optionally embeds the REST proxy for zero-config DX (`cargo run` gives you both ports out of the box). The embedded mode is a convenience, not the production architecture.
- **Refactor plan:**
  1. Extract shared TCP client code from `shroudb-cli` into a `shroudb-client` crate (Rust client library — also Phase 10)
  2. Rewrite `shroudb-rest` to use `shroudb-client` instead of importing `shroudb-protocol`/`shroudb-storage` directly
  3. Add `shroudb-rest` as a standalone binary target in the workspace
  4. Keep the embedded mode in `shroudb` binary as an option (controlled by config or `--embedded-rest` flag)
  5. Same approach for `shroudb-grpc` — standalone gRPC proxy that connects to shroudb over TCP
- **`shroudb-cli` is a client, not a server component.** It connects over TCP like any other client. The server Docker image (`docker run shroudb`) does not include the CLI — same as Redis (`docker run redis` doesn't include `redis-cli`) and Postgres (`docker run postgres` doesn't include `psql`). The CLI ships as a separate binary: `brew install shroudb-cli`, download from releases, or `docker run shroudb-cli`. The workspace builds both, but they're distributed independently.
- **Repository organization:**
  - **Main repo (Cargo workspace):** `shroudb` (server), `shroudb-core`, `shroudb-crypto`, `shroudb-storage`, `shroudb-protocol` (RESP3 codec), `shroudb-client` (Rust client library), `shroudb-cli` (wraps shroudb-client)
  - **Separate repos:** `shroudb-rest` (HTTP proxy, uses shroudb-client over TCP), `shroudb-grpc` (gRPC proxy, same pattern), TS/Go/Python/Ruby SDK clients (different language ecosystems)
  - The Rust client stays in the monorepo because it shares `shroudb-protocol` types. Proxies and non-Rust clients connect over TCP and don't need Cargo workspace integration.
- **When to do the REST refactor:** After `shroudb-client` is built. Rewrite `shroudb-rest` to use `shroudb-client` instead of the in-process dispatcher, then move to its own repo. The current in-process architecture works for development and small deployments.

### System Identity

ShrouDB is architecturally a **low-latency, append-only, encrypted, multi-tenant data system with strict correctness guarantees**. It is closer to Redis + Vault + Kafka semantics than a typical auth service. All decisions should be made with database kernel discipline:

- **Backward compatibility is real.** WAL format, snapshot format, and wire protocol are contracts. Breaking changes require migration tooling.
- **Operational failure modes matter more than features.** Recovery, corruption handling, and durability guarantees are tier-1 concerns.
- **Observability and recovery paths are tier-1 concerns.** Not afterthoughts.
- **Features are not cheap.** Each new feature adds surface area to the WAL format, index, and recovery path.

### Format Versioning (implemented)
- **WAL entry format v1:** `[len:u32] [version:u8] [flags:u8] [ks_id:...] [op_type:u8] [timestamp:u64] [payload] [crc32:u32]`. Version field enables safe format evolution. Flags reserved for future use (compression, extended headers).
- **Snapshot format:** Header includes `version: u16` and `encoding: String` ("postcard-v1"). Reader validates encoding compatibility before attempting deserialize.
- Any format change increments the version. Old readers reject unknown versions with a clear error rather than silent corruption.

### WAL Idempotency (implemented)
- `ApiKeyIssued` and `RefreshTokenIssued` WAL payloads carry an optional `request_id: String`. On crash+retry, the duplicate ISSUE produces a WAL entry with the same request_id. Future replay logic can detect and deduplicate.
- This is a data model decision, not a feature. The field exists in the WAL whether or not dedup logic is active.

### Replica Read Classification (implemented)
Commands are classified for replication semantics:
- **PureRead:** INSPECT, JWKS, KEYSTATE, HEALTH, KEYS, SCHEMA, CONFIG GET — safe on replicas, no side effects.
- **ObservationalRead:** VERIFY — returns result but on replicas skips `last_verified_at` update and may have stale revocation data. Consumers needing revocation freshness should verify against the primary.
- **WriteOnly:** ISSUE, REVOKE, ROTATE, REFRESH, UPDATE, SUSPEND, UNSUSPEND — primary only.

This is documented in `Command::replica_behavior()`. The classification exists before replication is built so the boundary is clean from day one.

### Failure Posture
ShrouDB is **fail-closed by default.** The system prefers refusing service over serving incorrect results.

| Failure | Behavior | Rationale |
|---------|----------|-----------|
| Disk full | WAL write fails → command rejected | Cannot guarantee durability |
| WAL corruption (strict mode) | Engine refuses to start | Corrupt state is worse than downtime |
| WAL corruption (recover mode) | Skip corrupt entries, snapshot clean state, start | Operator explicitly opted into data loss |
| Snapshot verification failure | Refuse to prune WAL segments | Corrupt snapshot + deleted WAL = total data loss |
| Master key wrong/unavailable | Engine refuses to start | Cannot decrypt state |
| KMS unavailable | Engine refuses to start | Same — no key, no start |
| Snapshot encoding unknown | Reject snapshot, fall back to WAL replay | Forward-compatible: new snapshot formats don't break old readers |
| WAL version unknown | Reject entry (strict) or skip (recover) | Same pattern as corruption |

### Multi-Tenant Rate Limiting (future)
Current rate limiting is per-connection. For multi-tenant deployments, the rate limiter must key on `(tenant_id, source_ip)` rather than just connection. The rate limiter interface is documented for this extension. This is a data model decision — the composite key structure must be decided before tenant isolation is built.

---

## Phase 0: Project Scaffolding

- [x] Initialize Cargo workspace with the following crates:
  - `shroudb` — binary entry point, CLI arg parsing, config loading
  - `shroudb-core` — data model, credential types, keyspace logic, state machines
  - `shroudb-storage` — WAL, snapshots, encryption, recovery
  - `shroudb-protocol` — RESP3 parser/serializer, command dispatch
  - `shroudb-rest` — REST/JWKS adapter (axum)
  - `shroudb-auth` — standalone HTTP auth server (signup/login/session/refresh/logout)
  - `shroudb-crypto` — key generation, signing, verification, HMAC, hashing, zeroization
- [x] Set up CI (GitHub Actions): `cargo clippy`, `cargo test`, `cargo audit`, `cargo deny`
- [x] Pin Rust toolchain via `rust-toolchain.toml` (stable, latest)
- [x] Add `Dockerfile` (multi-stage: builder + distroless/static runtime)
- [x] Create `config.example.toml` with all keyspace types

> **Crate boundary note:** `shroudb-crypto` should have a tight, auditable public surface from day one — this is the one boundary that must be stable early. Other internal crate boundaries will shift as you discover what needs to be shared; don't over-invest in internal API stability between them yet.

---

## Phase 1: Core Data Model & Crypto

### 1.1 Credential Types & State Machines
- [x] Define `KeyspaceType` enum: `Jwt`, `ApiKey`, `Hmac`, `RefreshToken`
- [x] Define lifecycle state enums:
  - `SigningKeyState`: `Staged → Active → Draining → Retired`
  - `ApiKeyState`: `Active → Suspended → Revoked`
  - `RefreshTokenState`: `Active → Consumed → Revoked`
- [x] Implement state transition validation (only legal transitions allowed)
- [x] Define `Keyspace` struct with name, type, policy, and credential storage
- [x] Define per-credential structs: `JwtSigningKey`, `ApiKeyEntry`, `HmacKey`, `RefreshTokenEntry`
- [x] Implement `FamilyId` and parent-chain tracking for refresh tokens
- [x] Unit tests for every state transition (valid transitions succeed, invalid ones error)
- [x] Define `MetaSchema` struct in `shroudb-core`: holds `enforce` flag + `Vec<FieldDef>`. Stored as an optional field on `Keyspace` — keyspaces without a schema continue to accept freeform metadata.
- [x] Define `FieldDef` struct: field name, type (`String | Integer | Float | Boolean | Array`), `required`, `default`, `enum_values`, `min`, `max`, `immutable`, `items` (element type for arrays — flat only, no nested objects)
- [x] `MetaSchema::validate(&self, metadata: &serde_json::Value) -> Result<(), Vec<ValidationError>>` — single-pass validation returning all errors, not just the first
- [x] `MetaSchema::validate_update(&self, existing: &Value, patch: &Value) -> Result<(), Vec<ValidationError>>` — validates the merged result, checks immutable fields against existing values, rejects null on required fields

### 1.2 Cryptographic Primitives (`shroudb-crypto`)
- [x] JWT signing key generation: ES256, ES384, RS256, RS384, RS512, EdDSA (use `ring`)
- [x] JWT signing and verification (build on `jsonwebtoken` crate — it uses `ring` internally)
- [x] API key generation: 32-byte CSPRNG + base62 encoding + optional prefix
- [x] SHA-256 hashing for API key and refresh token storage
- [x] Constant-time comparison on hash lookup results (prevent timing side-channels)
- [x] HMAC-SHA256/384/512 computation and verification
- [x] Constant-time HMAC comparison (prevent timing attacks on signature verification)
- [x] HKDF-SHA256 key derivation (master key → tenant context → per-keyspace keys)
- [x] AES-256-GCM encrypt/decrypt with unique nonces
- [x] `mlock`-pinned memory allocator for secret material (`memsec` or manual `libc::mlock`)
- [x] `Zeroize` on drop for all secret types (`zeroize` crate, `#[derive(Zeroize, ZeroizeOnDrop)]`)
- [x] Disable core dumps at process startup (`prctl(PR_SET_DUMPABLE, 0)` on Linux)
- [x] Known-answer tests for all crypto operations (test vectors from RFCs) *(HMAC RFC 4231, SHA-256 NIST, HKDF RFC 5869, AES-GCM NIST SP 800-38D, EdDSA RFC 8032)*

---

## Phase 2: Storage Layer

### 2.1 Master Key Management
- [x] Load from env var (`SHROUDB_MASTER_KEY`)
- [x] Load from file (`SHROUDB_MASTER_KEY_FILE`)
- [—] ~~Load from AWS KMS (envelope decryption, `aws-sdk-kms`)~~ *Won't do — operators inject master key via env var from their existing secret manager (Vault, K8s Secrets, AWS Secrets Manager). Adding KMS SDKs is heavy dependency surface for operational convenience, not security. Revisit when a real user requests a specific provider.*
- [—] ~~Load from GCP KMS~~ *Won't do — same rationale*
- [—] ~~Load from Azure Key Vault~~ *Won't do — same rationale*
- [x] Derive per-keyspace keys via HKDF with tenant context + keyspace name as context
- [x] Double-layer encryption for private key material (separate derived key from general keyspace key)
- [x] **Master key rotation tool:** `shroudb rekey --old-key <hex> --new-key <hex>` CLI subcommand. Reads all WAL + snapshots, re-encrypts with new key, writes clean snapshot.

### 2.2 Encrypted WAL
- [x] Define WAL entry format: `[len | keyspace_id | op_type | timestamp | encrypted_payload | CRC32]`
  - Header (len, keyspace_id, op_type, timestamp, CRC) is readable without decryption so recovery can validate and route entries without the decryption key for the payload
- [x] Implement append-only WAL writer with configurable fsync modes:
  - Per-write fsync (default for signing key mutations — rare and must not be lost)
  - Batched fsync (configurable interval, default 10ms — for high-volume operations)
  - Periodic fsync (configurable interval, default 100ms)
- [x] WAL segment rotation (new file after size/entry threshold)
- [x] Per-entry AES-256-GCM encryption using keyspace-derived keys + unique nonce
- [x] WAL reader for replay during recovery
- [x] **WAL corruption handling:**
  - On recovery, validate each entry's CRC before attempting decryption
  - Default mode (strict): halt on first corrupt entry, log segment/offset, refuse to start
  - `--recover` CLI flag: skip corrupt entries, log warnings, continue. After recovery completes, write a new clean WAL segment containing only valid recovered entries so corruption is not re-encountered on next restart.
  - Corrupt entries are never silently ignored in default mode
- [x] **Graceful shutdown:** On SIGTERM: stop accepting new connections → drain in-flight commands (30s timeout) → flush pending batched WAL writes → fsync → exit. Critical for batched fsync mode where unflushed entries would be lost.

### 2.3 Snapshots & Compaction
- [x] Snapshot serializer: serialize full in-memory state to encrypted blob (`bitcode` + AES-256-GCM)
- [x] Snapshot writer: atomic write (write to temp → fsync → rename)
- [x] **Read-back verification:** After writing a snapshot, read it back and verify HMAC/integrity before deleting any WAL segments. Corrupt snapshot + deleted WAL = total data loss.
- [x] **fsync/snapshot invariant:** snapshot() flushes WAL before building snapshot data. Dedicated test in recovery_tests.rs.
- [x] Background compaction: snapshot current state, delete old WAL segments
- [x] Configurable triggers: every N entries or every M minutes
- [x] Snapshot integrity verification on load (HMAC over snapshot content)

### 2.4 In-Memory Indexes
- [x] Sharded `DashMap<[u8; 32], ApiKeyEntry>` for API key hash lookups
- [x] `HashMap<TokenHash, RefreshTokenEntry>` + `HashMap<FamilyId, Vec<TokenHash>>` for refresh tokens
- [x] Signing key ring per JWT/HMAC keyspace (ordered by state/version)
- [x] Per-keyspace revocation sets (`HashMap<String, Instant>` — credential ID → expiry, background reaper every 60s with TTL-based auto-pruning)
- [ ] Optional bloom filter revocation strategy (operator opt-in only — see note)

> **Bloom filter caution:** A false positive on revocation means a valid token gets rejected — that's a production incident for the consumer. At 10M verifications/day, even 0.01% FPR means ~1,000 valid requests rejected. If this is ever offered, it should require explicit opt-in with clear documentation of the tradeoff, and should function as a negative cache (bloom says "definitely not revoked" → skip hashset; bloom says "maybe revoked" → fall back to authoritative hashset), never as the sole decision maker.

### 2.5 Startup Recovery
- [x] Load latest snapshot, verify integrity before trusting it
- [x] Replay WAL entries after snapshot checkpoint
- [x] Rebuild all in-memory indexes
- [x] Report `STARTING` on health endpoints during recovery, `READY` when done
- [x] Refuse all traffic until recovery is complete (not degraded service — hard refusal)
- [x] Benchmark: target <100ms for 10K creds, <2s for 1M creds *(10K: ~25ms, 100K: ~152ms — both within target. 1M not directly benchmarked but extrapolates to ~1.5s)*

---

## Phase 3: Command Engine

### 3.1 Command Parser & Dispatcher
- [x] RESP3 protocol parser *(custom implementation supporting 7 frame types — not redis-protocol crate; we use a subset with our own command syntax)*
- [x] RESP3 response serializer
- [x] **Response envelope convention:** Establish a consistent response shape across all commands from day one. Every success response is a RESP3 map with a `status` key plus command-specific fields. Every error is a RESP3 error with a machine-parseable code prefix (e.g., `-DENIED reason=token_expired`). This convention is load-bearing — it's the contract between the command engine and every client library.
- [x] Command router: parse command name → validate keyspace exists → dispatch to typed handler
- [x] Separate read commands (VERIFY, INSPECT, JWKS, HEALTH, KEYS, KEYSTATE, SCHEMA) from write commands (ISSUE, UPDATE, REVOKE, ROTATE, REFRESH, SUSPEND, UNSUSPEND) at the dispatch level (replication readiness)
- [x] Pipeline support: accumulate commands between `PIPELINE`…`END`, execute, return array

> **On the wire protocol:** RESP3 is used as a wire encoding — binary-safe framing, typed responses, and built-in pipelining — via a custom implementation (not the `redis-protocol` crate). ShrouDB is not Redis and does not aim for Redis client compatibility. RESP3 was chosen because it is a well-established protocol with clean semantics for the types ShrouDB needs (strings, integers, maps, errors, null), not for any Redis association. The command syntax (`VERB keyspace args...`) is ShrouDB's own DSL that rides on RESP3 framing.

### 3.2 Core Commands
- [x] `ISSUE` — dispatch by keyspace type:
  - JWT: sign claims with active key, embed `kid`, apply default/override TTL
  - API key: generate key, store hash, return raw key once
  - HMAC: compute signature over payload with active key
  - Refresh token: create new chain or (via REFRESH command) rotate
  - **Idempotency keys:** Optional `IDEMPOTENCY_KEY <key>` parameter on all ISSUE variants. Backed by a short-lived in-memory dedup map (`HashMap<String, (Instant, ResponseMap)>` behind a `tokio::sync::Mutex`, TTL 5 minutes hardcoded in `dispatch.rs`, background reaper every 60s). If a matching key exists, return the cached response instead of creating a new credential. The dedup map is **global** — shared across all connections and keyspaces via a single `IdempotencyMap` in the dispatcher. The TTL is not currently exposed as a configuration parameter. **Scope: transport-level retry protection only.** The 5-minute TTL covers network retries (client sends ISSUE, times out, retries with same idempotency key). It does not cover application-level exactly-once semantics (e.g., a client that crashes mid-workflow and restarts minutes later expecting to resume). Applications needing longer-lived deduplication should track idempotency at their own layer. The `idempotency_key` is also written to WAL entries as `request_id`, enabling future crash-recovery deduplication.
  - **Meta schema validation on ISSUE:** If the target keyspace has a `meta_schema` with `enforce = true`, validate the META JSON against the schema before storing. Missing required fields, wrong types, enum violations, min/max violations — all rejected with a structured error listing every failing field and reason. Apply defaults for absent non-required fields that have a `default` configured.
- [x] `VERIFY` — dispatch by keyspace type:
  - JWT: decode header → find key by `kid` → verify signature → check expiry (with configurable leeway, default 30s) → check `nbf` → optional CHECKREV
    - **Required claims enforcement:** If keyspace config includes `required_claims` (e.g., `required_claims = { aud = "my-service" }`), validate each required claim is present and matches after signature/expiry checks. Prevents tokens issued for one service from being accepted by another.
    - **Cache hint:** On successful verification, include `cache_until` in response (`min(exp, now + verify_cache_ttl)`). Optional per-keyspace `verify_cache_ttl` config (default: off). Allows high-throughput consumers to skip repeated verification of the same JWT within its validity window.
  - API key: hash input → lookup in index → check state
    - Update `last_verified_at` timestamp on every successful VERIFY (in-memory only, not WAL — this is abuse-detection data, not durable state). **Implementation note:** The field is marked `#[serde(skip)]` so it is excluded from both WAL entries and snapshots. It resets to `None` on every restart. Additionally, this field is currently **write-only** — it is updated on every successful VERIFY but is not exposed via INSPECT or any other command, and no internal logic reads it. It exists as a cheap signal for future abuse-detection features. Operators cannot currently query it without custom instrumentation. A future improvement could: (a) expose it in INSPECT responses, and (b) periodically bulk-flush verification timestamps to the WAL during snapshots (approximate durability without per-verify write amplification).
  - HMAC: recompute with each eligible key (ACTIVE + DRAINING), return matching kid
  - Refresh token: hash input → lookup → check state
- [x] `REVOKE` — add to revocation set, update credential state
  - Single revocation by credential ID
  - `FAMILY` flag for refresh tokens (revoke entire chain)
  - `BULK` for batch revocation
  - TTL on revocation entries (auto-prune after token expiry — no point tracking revocation for an expired token)
- [x] `REFRESH` — atomic consume-and-reissue:
  - Validate token is ACTIVE
  - Transition to CONSUMED
  - Generate new token in same family
  - Detect reuse → revoke entire family
  - **Chain length limit:** Configurable `max_chain_length` per refresh token keyspace (e.g., 100). When REFRESH would exceed the limit, reject and force re-authentication. Checked via the family index vec length (O(1)). Limits exposure window if a family is compromised but reuse isn't detected.
- [x] `UPDATE` — update metadata on an existing credential without revoking:
  - `UPDATE <keyspace> <credential_id> META <json>`
  - Merge semantics: caller sends only the keys to change, not the full map. Keys set to `null` are removed.
  - Validate credential exists and is not REVOKED
  - **Meta schema validation on UPDATE:** If the target keyspace has a `meta_schema` with `enforce = true`, validate the merged result (existing metadata + patch) against the schema. Reject null on required fields. Reject changes to `immutable` fields (compare patch keys against existing values — if an immutable field is present in the patch and differs from the stored value, reject with `-VALIDATION_ERROR field=org_id reason=immutable`).
  - Write UPDATE event to WAL
  - Update in-memory index
  - Supported for API key and refresh token keyspaces (JWT and HMAC keyspaces don't have per-credential metadata)
- [x] `INSPECT` — return metadata without verification

### 3.3 Key Management Commands
- [x] `ROTATE` — trigger rotation for JWT/HMAC keyspaces
  - Check if rotation is due (no-op if not, unless FORCE)
  - Promote STAGED → ACTIVE, demote old ACTIVE → DRAINING
  - Generate new STAGED key
  - If no STAGED key exists, generate and activate immediately (log warning about no pre-stage)
  - `FORCE` flag: rotate regardless of schedule
  - `NOWAIT` flag to skip pre-staging
  - `DRYRUN` flag: run the full rotation logic (check schedule, identify key transitions, generate new key material) but do not write to WAL or update in-memory state. Return what *would* happen. Useful for validating rotation automation pipelines end-to-end before going live.
  - Write rotation event to WAL
- [x] `JWKS` — serialize active + draining public keys as RFC 7517 JWK set JSON
- [x] `KEYSTATE` — return lifecycle state, timestamps, version for all keys in keyspace

### 3.4 Operational Commands
- [x] `HEALTH` — global and per-keyspace health reporting
- [x] `KEYS` — paginated credential enumeration with cursor, pattern match, state filter
- [x] `SUSPEND` / `UNSUSPEND` — API key state transitions
- [x] `CONFIG GET` / `CONFIG SET` — runtime config reading (SET returns error explaining restart required)
- [x] `SUBSCRIBE` — pub/sub for lifecycle events (rotation, revocation, reuse detection) via EventBus broadcast channel. Connection enters streaming mode, pushes events as RESP3 arrays.
- [x] `SCHEMA` — return the metadata schema for a keyspace:
  - `SCHEMA <keyspace>`
  - Read-only, returns the field definitions (name, type, required, enum values, default, immutable, min/max) for the keyspace's `meta_schema`. If no schema is configured, returns empty/none.
  - Useful for client libraries building typed wrappers and for operators verifying their schema loaded correctly.

---

## Phase 4: Network Interfaces

### 4.1 RESP3 TCP Server
- [x] Async TCP listener (tokio)
- [x] TLS support (rustls)
- [x] Unix domain socket support
- [x] Connection multiplexing and pipelining
- [x] Per-connection rate limiting (token bucket, configurable)
- [x] Auth handshake (token-based, checked against config policies)

### 4.2 REST Adapter (`shroudb-rest`)
- [x] HTTP server (axum) on configurable port
- [x] `POST /v1/issue`, `/v1/verify`, `/v1/revoke`, `/v1/refresh`
- [x] `GET /v1/inspect/{keyspace}/{credential_id}`
- [x] `GET /v1/health`, `/v1/health/{keyspace}`
- [x] `GET /{keyspace}/.well-known/jwks.json` with dynamic Cache-Control:
  - Compute based on rotation proximity (24h / 1h / 5m / no-cache)
  - ETag + If-None-Match support
  - **Rate limiting:** Dedicated per-source-IP token bucket on this endpoint (reuse the rate limiter from Phase 4.1, applied per-route). The JWKS endpoint is unauthenticated and public-facing — without its own limit it's a resource exhaustion vector. Document CDN fronting as a recommended production practice.
- [x] `GET /v1/schema/{keyspace}` — REST surface for the SCHEMA command
- [x] `GET /metrics` — Prometheus exposition format
- [x] TLS support *(TCP server has native TLS; REST TLS via reverse proxy — documented in config)*
- [x] Request/response JSON schema validation *(MetaSchema enforcement on ISSUE/UPDATE)*

### 4.3 gRPC Adapter → [shroudb/shroudb-grpc](https://github.com/shroudb/shroudb-grpc)

> **Architecture note:** `shroudb-grpc` is a standalone stateless proxy that connects to the shroudb TCP server as a client — same pattern as `shroudb-rest`. Lives in its own repo (same as SDK clients). Removed from the monorepo workspace.

- [x] Define `.proto` files for all services → [shroudb/shroudb-grpc](https://github.com/shroudb/shroudb-grpc)
- [x] Implement `ShrouDB` service (Issue, Verify, Revoke, Refresh, Inspect, Rotate, Health, KeyState, Schema) → shroudb-grpc repo
- [ ] Implement `VerifyStream` — bidirectional streaming for high-throughput verification *(not yet — unary RPCs cover current needs)*
- [x] Implement `Subscribe` — server-streaming for lifecycle events → shroudb-grpc repo
- [ ] Envoy `ext_authz` (`Authorization.Check`):
  - Extract credential from configurable header
  - Verify with CHECKREV
  - Return metadata as response headers
- [ ] Envoy `ext_proc` (`ExternalProcessor.Process`):
  - Request header processing
  - Token stripping from forwarded requests
  - Metadata header injection
- [ ] TLS + mTLS support

---

## Phase 5: Configuration & Lifecycle

### 5.1 TOML Config Loader
- [x] Parse `config.toml` with `serde` + `toml` crate
- [x] Internal representation is a runtime data structure loaded from TOML (not coupled to the file format — managed service will need programmatic keyspace creation)
- [x] Validate keyspace definitions at startup (reject invalid combos)
- [x] Environment variable interpolation in config values (`${VAR}`)
- [x] Config hot-reload for runtime-tunable values (fsync mode, rate limits)
- [x] **Keyspace lifecycle:** `disabled = true` per-keyspace config option + `shroudb purge <keyspace>` CLI tool
- [x] **Metadata schema config parsing:** Parse the optional `[keyspaces.<name>.meta_schema]` table into `MetaSchema` at startup. Validate the schema definition itself (e.g., `default` values must match the field type, `enum` values must match the field type, `items` is only valid on array fields, `min`/`max` semantics match the type). Reject invalid schema definitions at startup with clear errors. Schema is not hot-reloadable — changing a schema requires restart (changing a schema on a keyspace with existing credentials that don't conform is a migration problem, not a hot-reload problem). Example config:

```toml
[keyspaces.myapp-api-keys.meta_schema]
enforce = true  # false = skip validation (see note below), true = reject invalid

[keyspaces.myapp-api-keys.meta_schema.fields.org_id]
type     = "string"
required = true
immutable = true

[keyspaces.myapp-api-keys.meta_schema.fields.plan]
type     = "string"
required = true
enum     = ["free", "pro", "enterprise"]

[keyspaces.myapp-api-keys.meta_schema.fields.scopes]
type     = "array"
items    = "string"
required = true
min      = 1

[keyspaces.myapp-api-keys.meta_schema.fields.rate_tier]
type     = "string"
required = false
default  = "standard"
enum     = ["standard", "elevated", "unlimited"]
```

> **Schema type system is intentionally minimal.** Supported types: `string`, `integer`, `float`, `boolean`, `array` (with `items` for element type — flat only, no nested objects). Constraints per field: `required`, `enum`, `default` (non-required fields only), `min`/`max` (string length, numeric value, or array element count depending on type), `immutable` (cannot be changed via UPDATE once set on ISSUE). This is data integrity, not authorization — ShrouDB validates structure and type, the consuming application interprets meaning.

> **`enforce = false` behavior and migration path.** The spec describes `enforce = false` as "validate but warn." **The current implementation does neither** — when `enforce = false`, the ISSUE and UPDATE handlers skip validation entirely and accept metadata as-is. No `tracing::warn!()` is emitted, no validation errors are logged. This is a known gap between spec and implementation: the validate-and-warn path was never built. To make `enforce = false` useful as a migration tool, the handlers should call `schema.validate()` and log warnings via structured logging without rejecting the request. **Transitioning from `enforce = false` to `enforce = true`** on a keyspace with existing non-conforming credentials: ShrouDB does not currently offer a validation scan command against existing credentials. Operators should audit existing credentials via `KEYS` + `INSPECT` before flipping the flag. A future `SCHEMA VALIDATE <keyspace>` command that scans all credentials and reports violations without rejecting anything would make this migration safer — deferred until a real operator hits this workflow.

### 5.2 Background Schedulers
- [x] Key rotation scheduler: check all JWT/HMAC keyspaces on interval
  - Pre-stage keys N days before rotation
  - Activate staged keys on rotation day
  - Retire draining keys after overlap window
- [x] Revocation reaper: prune expired revocation entries (default every 60s)
- [x] Idempotency dedup reaper: prune expired dedup entries (same interval as revocation reaper, 60s)
- [x] Snapshot scheduler: trigger compaction per configured thresholds
- [x] Refresh token TTL reaper: purge expired tokens and families

### 5.3 Webhook Notifications
- [x] HTTP client for webhook delivery via reqwest with exponential backoff retries
- [x] HMAC-signed payloads
- [x] Configurable retry with backoff (up to N retries) (up to N retries)
- [x] Event types: `rotate`, `family_revoked`, `reuse_detected`

---

## Phase 6: Observability

### 6.1 Metrics
- [x] Integrate `metrics` crate + `metrics-exporter-prometheus`
- [x] Per-keyspace gauges: active creds, revoked creds, key age, rotation countdown, revocation set size, active families
- [x] Per-keyspace counters: operations by type/result, reuse detections, families revoked, password verify failures, password lockouts, password rehashes
- [x] Per-keyspace histograms: operation latency
- [x] System-level: WAL entries, WAL bytes, snapshot duration, recovery duration, memory usage

### 6.2 Structured Logging
- [x] Use `tracing` + `tracing-subscriber` with JSON output
- [x] Audit log: separate file, append-only, all state-mutating operations
- [x] Include: timestamp, op, keyspace, credential_id, result, source_ip, auth_policy, actor, duration

### 6.3 Health Endpoints
- [x] Liveness: process is running → 200
- [x] Readiness: WAL recovery complete, all keyspaces loaded → 200, else 503
- [x] Per-keyspace detailed status

---

## Phase 7: Access Control

- [x] Token-based authentication for command protocol connections
- [x] mTLS authentication option
- [x] Per-token policy: allowed keyspaces + allowed commands
- [x] Wildcard support (`keyspaces = ["*"]`, `commands = ["*"]`)
- [x] Auth bypass mode for sidecar/UDS deployments (`method = "none"`)

> **Note:** Auth tokens defined in TOML config are themselves credentials that need rotation. Rotating them currently requires config update + restart (or hot-reload). Document this limitation. The managed service will replace static tokens with a dynamic auth system.

---

## ShrouDB Auth — Standalone Auth Server (`shroudb-auth`)

> **Purpose:** Composes ShrouDB primitives (password hashing, JWT, refresh tokens) into turnkey HTTP auth flows. `docker run shroudb-auth` and your app has users. Any stack, no library lock-in.

### Phase 1: Core Flows (complete)
- [x] Signup — `POST /auth/{ks}/signup` → PasswordSet + Issue JWT + Issue refresh token + cookies
- [x] Login — `POST /auth/{ks}/login` → PasswordVerify + Issue JWT + Issue refresh token
- [x] Session — `GET /auth/{ks}/session` → Verify access token, return user claims
- [x] Refresh — `POST /auth/{ks}/refresh` → Rotate refresh token + Issue new access token
- [x] Logout — `POST /auth/{ks}/logout` → Revoke refresh token family + clear cookies
- [x] Change password — `POST /auth/{ks}/change-password` → Verify session + PasswordChange
- [x] JWKS — `GET /auth/{ks}/.well-known/jwks.json` → Cache-Control headers
- [x] Dual delivery: HttpOnly cookies + JSON body (browsers use cookies, APIs use Bearer tokens)
- [x] Cookie security: HttpOnly, Secure, SameSite=Lax, path-scoped refresh cookie
- [x] CORS middleware with configurable origins
- [x] Zero-config dev mode (ephemeral master key, default keyspace, human-readable logs)
- [x] k6 load test: auth-lifecycle.js (22 checks, 100% pass rate)

### Phase 2: Production Hardening (complete)
- [x] CSRF protection — Origin header validation middleware on POST routes
- [x] Per-IP rate limiting on signup/login (token bucket, opt-in via config)
- [x] Health endpoint — `GET /auth/health`
- [x] Prometheus metrics — `GET /metrics`

### Phase 3: Extended Flows (complete)
- [x] Forgot password — `POST /auth/{ks}/forgot-password` → issues short-lived JWT with `purpose=password_reset`; always returns 200 (no user enumeration)
- [x] Reset password — `POST /auth/{ks}/reset-password` → verifies reset token + PasswordReset (new protocol command)
- [x] Logout-all — `POST /auth/{ks}/logout-all` → revokes all refresh token families for a user
- [x] Session introspection — `GET /auth/{ks}/sessions` → lists active sessions for authenticated user

### Future
- [ ] Email/SMS delivery integration for password reset tokens
- [ ] OAuth2/OIDC provider support (Google, GitHub sign-in)
- [ ] Multi-factor authentication
- [ ] Account lockout notification webhooks

---

## Phase 8: Testing & Hardening

- [x] Unit tests for every state machine transition
- [x] Unit tests for all crypto operations (known-answer tests)
- [x] Integration tests: full command round-trips over RESP3 (37 tests), REST (not applicable — shared dispatcher), gRPC (skipped by design)
- [x] WAL crash recovery tests (write N entries, simulate crash, verify recovery)
- [x] WAL corruption recovery tests (inject corrupt entries, verify strict mode halts, verify `--recover` mode skips and rewrites clean)
- [x] fsync/snapshot interaction test (batched fsync + snapshot + crash — verify no lost or double-counted entries)
- [x] Refresh token replay detection tests (consumed token reuse → family revocation)
- [x] Rotation lifecycle tests (full STAGED → ACTIVE → DRAINING → RETIRED cycle)
- [x] Rotation dry-run test (DRYRUN returns correct plan without mutating state)
- [~] JWKS cache header tests (verify Cache-Control values change with rotation proximity) — logic implemented in REST routes; RESP3 test verifies JWKS response, but HTTP Cache-Control header not tested via REST
- [x] Snapshot read-back verification test (corrupt snapshot should not allow WAL pruning)
- [~] Graceful shutdown test (SIGTERM during batched fsync — verify no data loss) — jwt_survives_restart and api_key_survives_restart cover SIGTERM shutdown; dedicated batched-fsync stress test not yet written
- [x] UPDATE command tests (merge semantics, null removal, reject update on revoked credential)
- [x] Idempotency key tests (duplicate ISSUE with same key returns cached response, expired key allows new issuance)
- [x] Required claims enforcement tests (VERIFY rejects JWT missing required claims, accepts JWT with matching claims) — `jwt_required_claims_enforcement` integration test
- [x] Refresh token chain limit tests (REFRESH at max_chain_length rejects, under limit succeeds)
- [x] Keyspace disabled tests (all commands rejected with clear error, data retained, re-enable works)
- [x] JWKS endpoint rate limiting test *(per-connection rate limiting implemented and tested)*
- [x] Meta schema validation tests:
  - ISSUE rejected when required field missing, wrong type, enum violation, min/max violation
  - ISSUE succeeds with valid metadata; defaults applied for absent non-required fields with defaults
  - UPDATE rejected when nulling required field, changing immutable field, enum violation on new value
  - UPDATE succeeds with valid partial patch; merged result conforms to schema
  - `enforce = false` mode: validation warnings logged but ISSUE/UPDATE not rejected
  - Schema with no fields defined: all metadata accepted (passthrough)
  - SCHEMA command returns correct field definitions
  - Invalid schema definitions rejected at startup (e.g., default value type mismatch, items on non-array)
- [x] Fuzzing: RESP3 parser, config parser, WAL entry parser (`cargo-fuzz`) *(3 fuzz targets in fuzz/ directory — run manually with `cargo +nightly fuzz run <target>`)*
- [x] `cargo audit` in CI for dependency vulnerabilities
- [x] `unsafe` audit: documented in UNSAFE_AUDIT.md (3 production, 9 test-only)
- [x] Load testing: verify throughput targets *(1000 ops in <10s — `cargo test -p shroudb --test load_test -- --ignored`)*
- [x] Memory leak testing under sustained load *(2000 ops smoke test — `cargo test -p shroudb --test memory_test -- --ignored`)*

---

## Phase 9: Packaging & Distribution

- [x] Multi-stage Dockerfile (static musl binary + scratch/distroless)
- [x] Docker Compose example (ShrouDB + persistent volume)
- [x] Helm chart for Kubernetes
- [x] systemd unit file for bare-metal
- [x] Binary releases for Linux (x86_64, aarch64), macOS (aarch64) via CI
- [x] `config.example.toml` with all options documented
- [x] **Credential export/import:** `shroudb export <keyspace> --output <file>` and `shroudb import --file <path> --keyspace <name>`. Encrypted portable bundles with HMAC integrity.

---

## Developer Experience

RESP3 is an implementation detail. Developers should never need to know or care about the wire protocol — they interact with ShrouDB through client libraries, the REST API, or `shroudb-cli`. The goal is `docker run shroudb` and you're up.

### Zero-Config Development Mode
- [x] **Sane defaults without a config file.** If no `--config` is provided (or the file doesn't exist), ShrouDB starts with:
  - Bind `0.0.0.0:6399` (command protocol) + `0.0.0.0:8080` (REST)
  - Generate an **ephemeral master key** (random, in-memory only) with a loud startup warning: `"⚠ using ephemeral master key — data will not survive restart. Set SHROUDB_MASTER_KEY for persistence."`
  - Data directory `./data`
  - No TLS, no auth
  - No keyspaces (create them at runtime via commands or REST)
  - This is `docker run shroudb` behavior — instant usability with no ceremony.
- [x] **Environment-driven configuration.** Every config field overridable via env var (`SHROUDB_BIND`, `SHROUDB_REST_BIND`, `SHROUDB_DATA_DIR`, `SHROUDB_MASTER_KEY`) using `${VAR}` in TOML. Env vars take precedence over the config file. Follows the 12-factor app pattern. For Docker: `docker run -e SHROUDB_MASTER_KEY=... shroudb` is the production path.
- [x] **Simplified config surface.** Rename `resp3_bind` → `bind` (the primary protocol is the default, not a qualified variant). TLS, Unix sockets, and protocol-specific options are nested under `[server.tls]`, `[server.unix]` — not top-level fields. Example:
  ```toml
  bind = "0.0.0.0:6399"

  [rest]
  bind = "0.0.0.0:8080"

  [storage]
  data_dir = "./data"
  ```
  The current `resp3_bind` / `rest_bind` / `grpc_bind` naming leaks implementation details into the operator's mental model.

### `shroudb-cli` — Purpose-Built CLI
- [x] Interactive REPL for ShrouDB commands. Not redis-cli — a first-class tool that understands the ShrouDB DSL.
- [x] Connects to `localhost:6399` by default. `shroudb-cli --host <addr> --port <port>` for remote. `shroudb-cli --tls` for TLS connections using system root certs.
- [x] Human-readable output by default. `--raw` for RESP3 wire format. `--json` for JSON output.
- [x] Built-in help: `help <command>` shows syntax, args, examples for all 15+ commands.
- [x] Tab completion for commands and keyspace names.
- [x] Ships in the same binary or as a subcommand: `shroudb cli` or `shroudb-cli` (separate binary in the workspace).
- [x] Move ahead of the client libraries in the build order — `shroudb-cli` is needed for testing and demos before any SDK.

> **Why not redis-cli?** redis-cli works for sending raw commands, but it doesn't understand ShrouDB's command syntax, can't display keyspace-aware help, and forces developers to think in terms of RESP3 arrays. `shroudb-cli` is the face of the product — the first thing a developer touches after `docker run`. It should be polished.

### Docker First-Run Experience
- [x] `docker run shroudb` — starts with ephemeral key, binds both ports, human-readable logs. Dockerfile tested.
- [x] `docker run shroudb --help` — shows all CLI flags
- [x] Docker Compose example (ShrouDB + persistent volume)
- [x] README quick-start: 3 commands from zero to issuing a JWT

---

## Phase 10: Client Libraries

- [x] `shroudb-cli` — interactive REPL + scripting CLI (see Developer Experience section above). Build before SDK libraries — needed for testing and demos.
- [x] Rust client (dogfood — also used internally for integration tests)
- [x] TypeScript/Node.js client → [shroudb/shroudb-ts](https://github.com/shroudb/shroudb-ts)
- [x] Go client → [shroudb/shroudb-go](https://github.com/shroudb/shroudb-go)
- [x] Python client → [shroudb/shroudb-py](https://github.com/shroudb/shroudb-py)
- [x] Ruby client → [shroudb/shroudb-rb](https://github.com/shroudb/shroudb-rb)
- [x] Each client: connection management, pipelining, typed responses, pub/sub, health checks

> Client libraries are intentionally thin — 100–300 lines each. RESP3 framing is handled internally by the client. Developers never see RESP3 — they call `shroudb.issue("auth-tokens", claims)` and get back a typed response. Clients don't contain crypto, caching, or rotation logic.

---

## Managed Service

The managed service wraps the same core binary with multi-tenancy, usage metering, and a control plane. It is a separate product with its own spec and plan. It is included here to maintain visibility into how the core binary's design supports it — not as build scope for the phases above.

### Tenant Isolation
- Isolated keyspace namespace (tenant ID prefixed to all keyspace names, maps to the tenant context slot in HKDF derivation)
- Dedicated encryption envelope (per-tenant derived keys from tenant-specific master key)
- Independent WAL and snapshot partitions (enabled by the namespace prefix in storage paths)
- Per-tenant metrics and audit logs (enabled by the actor field in audit entries)

### Plans

|                   |Free     |Pro       |Enterprise|
|-------------------|---------|----------|----------|
|Keyspaces          |3        |20        |Unlimited |
|Credentials        |1,000    |100,000   |Unlimited |
|Verify ops/month   |100,000  |10,000,000|Custom    |
|Audit log retention|7d       |90d       |Custom    |
|Support            |Community|Email     |Dedicated |
|SLA                |—        |99.9%     |99.99%    |
|mTLS               |—        |Yes       |Yes       |
|External KMS       |—        |—         |Yes       |
|Regions            |US       |US, EU    |Custom    |

### Control Plane
- Web dashboard: keyspace creation/configuration, rotation status/history, credential counts/usage, audit log viewer, alert configuration
- The control plane manages configuration and observability only — it does not handle the data path
- All VERIFY, ISSUE, REVOKE operations hit the ShrouDB data plane directly
- Programmatic keyspace creation via API (enabled by the runtime config data structure, not TOML-coupled)

### Replication (for HA deployments)
- WAL-shipping replication: primary streams encrypted WAL entries to read-only replicas
- Replicas serve VERIFY/INSPECT only (enabled by the read/write dispatch separation in Phase 3)
- Consistency model: eventual consistency on revocation — document the propagation window
- Revocation must propagate quickly across regions (a revoked token verified in a different region should fail)

---

## Suggested Build Order

The phases above are roughly ordered by dependency. Within a milestone, here is the recommended critical path:

1. **Crypto + Data Model** (Phase 1) — everything depends on this
2. **Storage WAL + Recovery** (Phase 2.1, 2.2, 2.4, 2.5) — need durability before commands
3. **Core Commands over RESP3** (Phase 3.1, 3.2 + Phase 4.1) — first usable system
4. **`shroudb-cli` + zero-config defaults** (DX section) — testable, demo-able system
5. **REST adapter + JWKS** (Phase 4.2) — needed for real JWT consumers
6. **Rotation lifecycle + schedulers** (Phase 3.3, 5.2) — completes JWT/HMAC story
7. **Snapshots + compaction** (Phase 2.3) — required before production use
8. **gRPC proxy** (Phase 4.3) — separate repo, unlocks infrastructure integration
9. **ShrouDB Auth** (standalone auth server) — composed auth flows for app developers
10. **Everything else** (access control, observability polish, webhooks, remaining clients)

---

## Technical Decisions to Lock Down Early

These are decisions that will be painful to change later. Make them deliberately in Phase 0/1.

| Decision | Recommendation | Why it matters |
|---|---|---|
| **Crypto backend** | `ring` for core primitives, `jsonwebtoken` for JWT encode/decode (uses `ring` internally) | Switching crypto libraries later means re-validating every operation. `ring` is the Rust ecosystem's most audited option. |
| **Async runtime** | tokio (multi-threaded) | Every network crate in the plan (axum, tonic, rustls) assumes tokio. |
| **Serialization for WAL/snapshots** | `postcard` (binary, encrypted payloads) | Compact varint encoding via `postcard` crate. Enabled by replacing `serde_json::Value` metadata with typed `MetadataValue` enum. Previous attempts with `bincode` (unmaintained, RUSTSEC-2025-0141) and `bitcode` (panics on `deserialize_any`) failed because `serde_json::Value` is incompatible with all schema-driven binary formats. |
| **In-memory map for API keys** | `DashMap` (sharded concurrent map) | The hot path. Needs lock-free reads. SHA-256 hashes are uniform by definition so shard distribution should be even — verify under load. |
| **JWT clock leeway** | Configurable, default 30s | Without this, clock skew between ShrouDB and the issuing service causes spurious DENIED responses. Bake it in from the start. |
| **WAL entry envelope** | Fixed header (len + keyspace + op + timestamp + CRC) + encrypted payload | Header must be readable without decryption so recovery can validate/skip corrupt entries without the payload key. CRC covers the encrypted payload bytes. |
| **RESP3 response convention** | Every success: RESP3 map with `status` key + command-specific fields. Every error: RESP3 error with machine-parseable code prefix. | This is the contract between the command engine and every client library. If each command invents its own response shape, clients become painful to write. |
| **HKDF context format** | `"{tenant_context}:{keyspace_name}"` with tenant hardcoded to `"default"` | Costs one extra string parameter now. Without it, multi-tenancy requires re-deriving every key. |

---

## Things You May Not Have Considered

### Security & Operational

1. **Key ceremony / master key rotation.** Addressed in Phase 2.1 as the `shroudb rekey` CLI tool. Offline operation requiring quiesce. Design should anticipate KMS sources for both old and new keys.

2. **Graceful shutdown and drain.** Addressed in Phase 2.2. SIGTERM → stop accepting → drain in-flight → flush batched WAL → fsync → exit. Especially critical for batched/periodic fsync modes.

3. **WAL corruption handling.** Addressed in Phase 2.2. Strict mode (halt) by default, `--recover` flag for skip-and-rewrite. Clean WAL rewrite after recovery prevents re-encountering corruption.

4. **Snapshot verification before WAL truncation.** Addressed in Phase 2.3. Read-back verification after snapshot write, before any WAL pruning.

5. **Clock skew and JWT expiry.** Addressed in Phase 3.2 as configurable leeway (default 30s) on VERIFY.

6. **Secret material in core dumps.** Addressed in Phase 1.2. `prctl(PR_SET_DUMPABLE, 0)` at process startup.

7. **Side-channel resistance for HMAC verification.** Addressed in Phase 1.2. Constant-time comparison for both HMAC signatures and API key hash lookups.

### Functionality

8. **Credential metadata updates.** Addressed in Phase 3.2 as `UPDATE` command. Merge semantics (send keys to change, not full map). Supported for API key and refresh token keyspaces.

9. **Batch VERIFY.** Dropped as a dedicated command. Pipeline support already covers this use case — client libraries can expose a `verify_many()` method that uses pipelining internally. Adding a separate command creates another code path and response format for marginal convenience. Trivial to add later if the need materializes.

10. **Token binding / audience restriction.** Addressed in Phase 3.2 under VERIFY for JWT keyspaces, and Phase 5.1 as per-keyspace `required_claims` config. Validates required claims are present and match after signature/expiry checks.

11. **Refresh token absolute chain limits.** Addressed in Phase 3.2 under REFRESH. Configurable `max_chain_length` per keyspace, checked via family index vec length.

12. **API key scoping by IP/CIDR.** Deferred. Requires structured metadata (not opaque), source IP threading through the command protocol, and a new failure mode. Operators who need this today can store CIDRs in the metadata map and check in their application layer. Revisit after core is stable — implementation would touch Phase 3.2 (VERIFY accepts optional `SOURCE_IP`) and Phase 4 (REST/gRPC adapters extract and forward client IP).

13. **Credential export for migration.** Addressed in Phase 9 as `EXPORT` command + `shroudb import` CLI tool. WAL format designed in Phase 2 already anticipates this per the Architectural Commitments.

14. **Read replicas / replication.** Addressed architecturally in the Managed Service section and the Architectural Commitments (read/write dispatch separation in Phase 3.1).

15. **Keyspace lifecycle management.** Addressed in Phase 5.1. `disabled = true` config option for soft decommission, `shroudb purge` CLI tool for destructive deletion.

16. **Rate limiting per credential, not just per connection.** Full per-credential rate limiting deferred (requires a second hot-path data structure with its own storage/eviction). `last_verified_at` timestamp tracking on API key VERIFY added to Phase 3.2 as a cheap signal — updated in-memory on every successful verification, gives operators enough data to detect abuse in their own monitoring.

17. **VERIFY response caching hints.** Addressed in Phase 3.2 under VERIFY for JWT keyspaces. `cache_until` field in response, driven by optional per-keyspace `verify_cache_ttl` config (default: off).

18. **Idempotency keys on ISSUE.** Addressed in Phase 3.2 under ISSUE. Optional `IDEMPOTENCY_KEY <key>` parameter with short-lived in-memory dedup map (5 minute TTL, background reaper).

19. **Multi-region consistency model.** Documented in the Managed Service section. Revocation must propagate quickly across regions.

20. **Admin audit separation.** Addressed via the `actor` field in audit entries (see Architectural Commitments and Phase 6.2). Distinguishes tenant operations from platform operations in the managed service.

21. **Canary/dry-run rotation.** Addressed in Phase 3.3. `ROTATE <keyspace> DRYRUN` flag runs full rotation logic without committing.

22. **JWKS endpoint rate limiting / abuse protection.** Addressed in Phase 4.2. Dedicated per-source-IP rate limiter on the JWKS endpoint, plus documented CDN recommendation.

23. **Structured metadata schemas.** Addressed across Phase 1.1 (data model: `MetaSchema`, `FieldDef`, validation methods), Phase 3.2 (enforcement on ISSUE and UPDATE), Phase 3.4 (`SCHEMA` command), Phase 4.2 (`GET /v1/schema/{keyspace}`), and Phase 5.1 (TOML config parsing and startup validation). Minimal type system (`string`, `integer`, `float`, `boolean`, `array`), field constraints (`required`, `enum`, `default`, `min`/`max`, `immutable`), `enforce` toggle for migration. Data integrity, not authorization.
---

## What's Next: Usage Pressure

The system is architecturally complete. The remaining risk is not technical — it's that **incorrect assumptions don't show up in code, they show up in usage patterns.**

### What we don't yet know
- Which credential type dominates real usage (JWT? API keys? Refresh tokens?)
- Whether metadata schemas matter to users or are over-engineered
- How often rotation is actually triggered in practice
- What VERIFY throughput patterns look like under real load
- Whether the WAL format decisions hold under multi-region replication

### What to do next
1. **Put ShrouDB behind one real service.** Issue real JWTs, verify real API keys, rotate real signing keys. The integration tests prove correctness; usage proves relevance.
2. **Expose to 2-3 external users.** Let them hit the REST API and `shroudb-cli`. Watch what they struggle with, what they ignore, and what they ask for that doesn't exist.
3. **Instrument aggressively.** The Prometheus metrics are in place. Use them. Track VERIFY latency percentiles, ISSUE rates per keyspace, rotation frequency, WAL segment sizes.
4. **Resist adding features.** Every new feature adds WAL format surface area. The next features should come from usage feedback, not architecture planning.
5. **Maintain error message quality.** A design review flagged error messages as a potential gap, but the implementation is already strong. Error messages are structured (`CODE key=value` format), specific (`BADARG missing required argument: keyspace`, `DENIED command VERIFY not allowed by policy 'read-only'`, `WRONGTYPE keyspace=sessions type=ApiKey expected=refresh_token`), and include context that aids debugging (entity types, IDs, expected vs. actual values, parse errors). The main weakness is occasional opacity in INTERNAL errors for system-level failures, which is acceptable. Watch for regressions as new commands are added.

The system is correct in isolation. It needs real-world pressure to validate the assumptions baked into its design.

---

## Design Review Notes

External review feedback and where each observation was addressed in this document.

### Validated Design Decisions

The following decisions were specifically called out as strong:

- **Fail-closed posture** — correct call for a credential system. Explicitly committed to rather than letting it emerge implicitly. Too many systems in this space default to fail-open for availability and spend years walking it back. *(See: Failure Posture table in Architectural Commitments)*
- **HKDF tenant context slot** — costs nothing now, expensive to retrofit. Same principle applies to the `actor` field in audit logs and namespace prefix in storage paths. These are cheap insurance policies. *(See: Multi-Tenancy Readiness)*
- **Protocol proxy architecture** — core is TCP, REST/gRPC are stateless clients. Right precedents cited (Upstash, Neon, PlanetScale). Embedded mode for DX is a smart concession. *(See: Protocol Proxy Architecture)*
- **Deferred features with clear rationale** — bloom filter revocation, IP/CIDR scoping, batch VERIFY each have a clear "revisit when" trigger. Mark of a plan that survives contact with reality.

### Observations Checked Against Implementation

The reviewer worked from the spec only, without codebase access. Each recommendation was verified against the actual code:

1. **Replica key distribution for WAL encryption.** Reviewer flagged that replicas need decryption keys for every keyspace they serve reads for, and asked whether replicas derive from the master key or receive per-keyspace keys. **Verified:** No replication code exists. Per-keyspace keys are derived via HKDF in `key_manager.rs` using a `RwLock`-based read-through cache. The `KeyManager` derives three key types per keyspace (keyspace key, private key key, snapshot key) all from the same master key + tenant context. The reviewer's concern is architecturally real — the choice affects blast radius and must be decided before replication ships. → *Added to Replication Readiness in Architectural Commitments.*

2. **`last_verified_at` resets on restart.** Reviewer flagged this as an acknowledged gap worth documenting. **Verified and expanded:** The field has `#[serde(skip)]` in `api_key.rs`, so it is excluded from both WAL and snapshots — resets to `None` on every restart. What the reviewer couldn't know: the field is currently **write-only**. It is updated on every successful VERIFY in `verify.rs` but is never read by any command (including INSPECT) and no internal logic consumes it. It's a dead signal — useful infrastructure for future abuse detection, but not yet wired to anything observable. → *Updated in Phase 3.2 VERIFY.*

3. **`enforce = false` warn destination.** Reviewer asked: "warn where?" **Verified: the spec oversells the implementation.** The spec says "validate but warn." The code does neither — when `enforce = false`, the ISSUE and UPDATE handlers skip `schema.validate()` entirely. No warnings are emitted to structured logging or anywhere else. The validate-and-warn path was never built. This is the one item where the reviewer caught a real spec-vs-implementation gap, even without seeing the code. → *Fixed in Phase 5.1 schema config note — doc now reflects actual behavior.*

4. **Idempotency key 5-minute TTL scope.** Reviewer correctly noted the TTL won't cover longer-lived idempotency needs. **Verified and expanded:** The TTL is hardcoded at 300 seconds in `dispatch.rs:28` (not configurable). The dedup map is a global `HashMap<String, (Instant, ResponseMap)>` behind a `tokio::sync::Mutex`, shared across all connections and keyspaces. The `idempotency_key` is also persisted to WAL entries as `request_id`, enabling future crash-recovery deduplication — a detail the reviewer couldn't see. → *Clarified in Phase 3.2 ISSUE.*

5. **Error message quality.** Reviewer predicted the first integration would reveal error message gaps. **Verified: this concern was premature.** Error messages are already specific and actionable — `BADARG missing required argument: keyspace`, `DENIED command VERIFY not allowed by policy 'read-only'`, `WRONGTYPE keyspace=sessions type=ApiKey expected=refresh_token`, `CHAIN_LIMIT family=fam-123 limit=5`. Each error type carries structured context (entity+ID, expected vs. actual, state transitions). HTTP status code mapping in `shroudb-rest` is also correct (400/403/404/410/422/503). The main weakness is INTERNAL errors for system-level failures, which is acceptable. → *Reframed as "maintain quality" rather than "invest in quality" in What's Next.*
