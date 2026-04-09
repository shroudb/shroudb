# ShrouDB — Repository Analysis

**Component:** shroudb  
**Type:** Database server binary + client library + CLI tool (4-crate workspace)  
**Language:** Rust (edition 2024, stable toolchain, rust-version 1.92)  
**License:** MIT OR Apache-2.0 (dual)  
**Published:** Private registry ("shroudb") for internal crates; Docker images to ghcr.io/shroudb/shroudb  
**Analyzed:** /Users/nlucas/dev/shroudb/shroudb  
**Version:** 1.0.2

---

## Role in Platform

ShrouDB is the storage kernel for the entire platform. Every engine (Cipher, Keep, Veil, Forge, Sentry, Chronicle, Courier) depends on the Store trait defined and implemented here. Without ShrouDB, no engine can persist or retrieve data — the platform has zero function. It provides encrypted-at-rest KV storage with per-namespace HKDF key derivation, version history, tombstone lifecycle, ACL enforcement, and both embedded (in-process) and remote (TCP/TLS) access modes.

---

## Behavioral Surface

### Public API

**Wire protocol (20 RESP3 commands):**
- Connection: `AUTH`, `PING`
- Data: `PUT`, `GET`, `DELETE`, `LIST`, `VERSIONS`
- Namespace: `NAMESPACE CREATE|DROP|LIST|INFO|ALTER|VALIDATE`
- Batch: `PIPELINE` (with idempotency via `REQUEST_ID`)
- Streaming: `SUBSCRIBE`, `UNSUBSCRIBE`
- Operational: `HEALTH`, `CONFIG GET|SET`, `COMMAND LIST`

**CLI subcommands:** `doctor` (health check), `rekey` (master key rotation), `export` (encrypted namespace backup), `import` (restore from backup)

**Rust client SDK (`shroudb-client`):** Typed async API wrapping RESP3. Implements `Store` trait via `RemoteStore` — any engine can use ShrouDB over the network transparently.

**Store trait (from `shroudb-store`):** `put`, `get`, `delete`, `list`, `versions`, `namespace_create|drop|list|info|alter|validate`, `subscribe`. Two implementations: `EmbeddedStore` (in-process, from `shroudb-storage`) and `RemoteStore` (TCP/TLS client).

### Core operations traced

**PUT flow:** RESP3 frame → `parse_command` (array of bulk strings → `Command::Put`) → `dispatch` (ACL check via `AuthContext::check(&AclRequirement::Namespace { scope: Write })`) → `handlers::put::handle` → `store.put(ns, key, value, metadata)` → WAL append (AES-256-GCM encrypted entry with HKDF-derived namespace key) → in-memory DashMap index update → version number returned → RESP3 Map response serialized → optional webhook broadcast + audit log entry.

**GET with version:** Same ACL path (scope: Read) → `store.get(ns, key, Some(version))` → index lookup, if cache miss then WAL/snapshot scan → decrypt entry → return value + version + optional metadata. Bounded index mode: LRU eviction, cache miss triggers WAL recovery read.

**PIPELINE with idempotency:** Parse `REQUEST_ID` from pipeline frame → check `IdempotencyMap` (DashMap, 5-min TTL) → if hit, return cached RESP3 frame verbatim → if miss, execute sub-commands sequentially, serialize combined response, cache frame, return.

### Capability gating

No Cargo feature flags. Gating is runtime:
- **Platform tokens** (boolean on Token): grants cross-tenant access and admin commands
- **Scope enum** (Read/Write): per-namespace grant matching
- **Auth required** (config flag): when false, anonymous platform context granted
- **Ephemeral mode**: no master key env var → random key generated, data doesn't survive restart

---

## Cryptographic Constructs

- **AES-256-GCM** encryption at rest for all WAL entries and snapshots (via `shroudb-storage` + `shroudb-crypto`)
- **HKDF-SHA256** key derivation: master key → per-namespace data encryption keys. Derivation tree structure in `shroudb-crypto`, not visible in this repo
- **Master key**: 32-byte symmetric, sourced from env var (hex) or file; wrapped in `SecretBytes` (zeroize-on-drop, mlock-pinned)
- **ring** crypto provider throughout: `SystemRandom` for key generation, `hmac::HMAC_SHA256` for webhook signatures, TLS via rustls with ring backend
- **Export format**: HKDF-derived export key from master key + AAD (header JSON) → AES-256-GCM encrypted postcard payload
- **Constant-time token validation**: timing attack mitigation (tested)
- **Core dump disabled** at startup via `shroudb_crypto::disable_core_dumps()`
- **CRC32** integrity check on every WAL entry (tamper detection, not cryptographic)

No key rotation without downtime — `rekey` subcommand requires server stopped, replays all WAL entries with new master key.

---

## Engine Relationships

### Calls out to
- `shroudb-store` (Store trait definition, core types: Entry, VersionInfo, Page, Namespace, StoreError)
- `shroudb-storage` (EmbeddedStore implementation — WAL, snapshots, in-memory index, encryption)
- `shroudb-crypto` (SecretBytes, disable_core_dumps, key material handling)
- `shroudb-acl` (AuthContext, StaticTokenValidator, AclRequirement, Grant, Scope, Token, TokenValidator)
- `shroudb-telemetry` (structured logging, audit, metrics)

### Called by
- Every engine (Cipher, Keep, Veil, Forge, Sentry, Chronicle, Courier) via Store trait — either EmbeddedStore in Moat or RemoteStore over TCP/TLS
- `shroudb-moat` embeds EmbeddedStore directly
- `shroudb-codegen` reads `protocol.toml` to generate SDK clients
- `shroudb-wal-tool` operates on WAL/snapshot files for topology migration

### Sentry / ACL integration

ACL enforcement is built into the dispatch layer (`shroudb-protocol/src/dispatch.rs`). Every command declares an `AclRequirement` (None, Admin, or Namespace with scope). The dispatcher checks `auth_context.check(&requirement)` before handler execution. Auth tokens are validated via `StaticTokenValidator` from `shroudb-acl`. No Sentry fallback pattern here — this is the base layer that Sentry itself stores policies in.

---

## Store Trait

ShrouDB defines the canonical Store trait (in `shroudb-store` crate). This repo provides two implementations:

- **EmbeddedStore** (from `shroudb-storage`): WAL-backed, AES-256-GCM encrypted, DashMap in-memory index with optional LRU bounded cache. Supports snapshot compaction, tombstone TTL, crash recovery.
- **RemoteStore** (in `shroudb-client`): Wraps `ShrouDBClient` in `Arc<Mutex<>>`, sends RESP3 commands over TCP/TLS, maps responses back to Store types. Subscription stub (returns None — requires dedicated streaming connection).

Storage backends: WAL segments + periodic snapshots on local filesystem. No pluggable backend abstraction at this layer — the storage engine is the WAL.

---

## Licensing Tier

**Tier:** Open core (MIT OR Apache-2.0)

The entire shroudb repository — server, protocol, client, CLI — is dual-licensed MIT/Apache-2.0. No capability traits, feature flags, or code-level fences between open and commercial tiers. The commercial boundary is at the repo level: engines like Cipher, Veil, Forge, Sentry, Keep, Chronicle, and Courier are in separate repositories with their own licensing. ShrouDB itself is the open foundation that makes those engines possible.

---

## Standalone Extractability

**Extractable as independent product:** Yes — it already is one.

ShrouDB is a fully functional, self-contained encrypted KV database. It runs as a single static binary with zero external dependencies (no database, no message queue, no service mesh). Docker image, Helm chart, systemd service file, and CLI are all included. The `protocol.toml` spec enables SDK generation for any language.

Value lost without sibling engines: none for the core use case. ShrouDB is a general-purpose versioned encrypted store. Engines add domain-specific logic (transit encryption, blind indexing, policy evaluation) but ShrouDB's value proposition — encrypted-at-rest KV with version history and tenant isolation — stands alone.

### Target persona if standalone
- Security-conscious infrastructure teams needing encrypted secrets/config storage
- Compliance-driven organizations requiring audit trails and cryptographic tenant isolation
- Platform teams building multi-tenant SaaS needing per-tenant data isolation without separate databases
- Teams currently using Vault/Consul for secrets but wanting simpler operations (single binary, no cluster)

### Pricing model fit if standalone
- **Open core + commercial support/hosting** fits best. The MIT/Apache-2.0 base drives adoption; revenue from managed hosting, enterprise support, or the commercial engines built on top.
- Usage-based (per-namespace, per-operation) feasible for hosted offering.
- Not a good fit for seat-based pricing — it's infrastructure, not a user-facing product.

---

## Deployment Profile

- **Single static binary** (musl-linked, multi-arch: x86_64 + aarch64)
- **Docker image** (Alpine 3.21, multi-arch, ghcr.io/shroudb/shroudb)
- **Helm chart** (single-replica Deployment, PVC for WAL, ConfigMap for TOML, Secret for master key)
- **systemd service file** with security hardening (ProtectSystem=strict, MemoryDenyWriteExecute, mlock limits)
- **Ports**: 6399 (RESP3), 9090 (Prometheus metrics)
- **Infrastructure dependencies**: filesystem for WAL/snapshots. Nothing else. No external database, no ZooKeeper, no etcd.
- **Self-hostable**: Yes, trivially. Single binary + master key + data directory.

---

## Monetization Signals

- **Tenant scoping**: Built-in. Per-namespace HKDF key derivation, per-token tenant binding, ACL grants scoped to namespace + scope.
- **Quota enforcement**: Rate limiting per connection (token bucket). No per-tenant quota counters.
- **API key validation**: Token-based auth with constant-time validation, expiry checking, platform/regular token distinction.
- **Usage counters**: Prometheus metrics for commands, connections, cache usage, WAL size. No billing-oriented metering.
- **No license key checks** or entitlement verification in the code.

Present but not monetized: the tenant isolation and token system are infrastructure for multi-tenancy, not billing gates.

---

## Architectural Moat (Component-Level)

**What is non-trivial to reproduce:**

1. **HKDF key derivation tree with per-namespace isolation**: Each namespace gets its own encryption key derived from the master key. Compromise of one namespace's data reveals nothing about others. This is the core security property and it's baked into every WAL entry.

2. **WAL-based encrypted storage with crash recovery**: Append-only WAL with CRC32 integrity, periodic snapshot compaction, and full crash recovery (tested under SIGKILL). The bounded index (LRU cache over WAL) decouples memory from dataset size while maintaining recovery guarantees.

3. **Store trait as platform contract**: The trait abstraction that lets engines run identically in-process (EmbeddedStore) or over the network (RemoteStore) is the architectural fulcrum of the entire platform. Replacing this means rewriting every engine.

4. **Operational completeness**: Master key rotation (`rekey`), encrypted export/import, health diagnostics (`doctor`), config hot-reload, webhook delivery with HMAC signing, structured audit logging, Prometheus metrics — this is production infrastructure, not a prototype.

5. **RESP3 as transport layer**: Using a well-specified binary protocol means any language can build a client without an SDK. The `protocol.toml` machine-readable spec makes client generation mechanical.

The moat is partly component-level (crypto constructs, WAL engine) and partly platform-level (Store trait contract that every engine depends on).

---

## Gaps and Liabilities

1. **Rekey requires downtime**: No online key rotation. Server must be stopped, `rekey` replays all WAL entries. Blocking for large datasets in production.

2. **Replication is pre-decision**: Phase 0 instrumentation exists (metrics), but no WAL streaming, no replica bootstrap, no failover. Single-node only. REPLICATION_PLAN.md is thorough but unimplemented.

3. **Namespace drop TOCTOU**: Non-force DROP has a race window — concurrent PUT between key count check and WAL write can succeed then lose data. Documented in ISSUES.md.

4. ~~**CONFIG SET has no schema enforcement**~~: Resolved — schema registry wired up with type-checked keys (max_segment_bytes, max_segment_entries, snapshot_entry_threshold, snapshot_time_threshold_secs). Unknown keys rejected at runtime.

5. **RemoteStore subscription stub**: `subscribe()` returns None. Streaming over RemoteStore not implemented — engines using subscriptions must use EmbeddedStore or handle streaming separately.

6. **No LICENSE file in repo root**: License declared in Cargo.toml and Dockerfile labels but no LICENSE or LICENSE-MIT/LICENSE-APACHE file. Could block adoption by compliance-conscious users.

7. **Helm chart at v0.1.0**: Single-replica Deployment (not StatefulSet), no PodDisruptionBudget, no ServiceMonitor for Prometheus Operator. Minimal for production Kubernetes.

8. **Fuzz targets exist but untested in CI**: 6 targets present but need registry credentials to run. No evidence of corpus-building or CI integration.

9. **LIST cursor pagination**: Invalid cursors silently skip results. No cursor validation or error reporting.

---

## Raw Signals for Evaluator

- **Test coverage is strong**: 40 integration tests covering data path, auth, crash recovery, TLS, webhooks, cache eviction, audit logging, timing attacks. Tests use real server processes, not mocks.
- **V2 scaling strategy is credible**: Bounded index (done) → Moat refactor → namespace-level scale-to-zero → namespace-based sharding. Each phase has clear triggers and non-triggers.
- **Engine namespace bindings are documented**: V2_PLANS.md maps each engine to its namespace prefixes (e.g., `keep.secrets`, `sentry.policies`), confirming the platform design.
- **Commons crates are published**: shroudb-store v0.1.2, shroudb-acl v0.2.0, shroudb-storage v0.2.11, shroudb-crypto v0.1.1 — all on the private registry. The trait contract is stabilizing.
- **No multi-region, no horizontal scaling, no automatic failover** — the replication plan explicitly defers these and documents when they'd be justified. Honest engineering.
- **The RESP3 choice is defensible and well-documented**: CLAUDE.md explicitly prohibits Redis comparisons. The protocol is used as binary framing, not for Redis compatibility. The command set is entirely different.
- **Security hardening is thorough**: mlock, core dump disable, zeroize-on-drop, constant-time token comparison, fail-closed on clock error, rate limiting starts at 1 token (no burst after idle). These are not afterthoughts.
- **Webhook HMAC signing and exponential backoff** suggest the system is designed for integration into larger event-driven architectures.
- **The `protocol.toml` → codegen pipeline** means SDK generation is mechanical. This is a multiplier for ecosystem growth.
