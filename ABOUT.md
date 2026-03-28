# Understanding ShrouDB

---

## For Everyone: What ShrouDB Does

Applications need a reliable way to store structured data that stays private. General-purpose databases can do this, but encryption is an afterthought -- bolted on via plugins, disk-level wrappers, or application code. Keys are managed separately, schemas are enforced elsewhere, and history is either absent or expensive to query.

**ShrouDB is a standalone encrypted key-value database.** Every value is encrypted at rest with AES-256-GCM before it touches disk. Keys are derived per namespace from a single master key using HKDF, so compromising one namespace's storage reveals nothing about another. There is no unencrypted mode.

**What it provides:**

- **Encrypted storage** -- All data encrypted at rest. A disk breach yields ciphertext, not values.
- **Version history** -- Every key retains its change history with configurable retention, so you can retrieve or audit previous values.
- **Namespace isolation** -- Data is partitioned into namespaces, each with its own encryption context and optional JSON schema validation on writes.
- **Tombstone compaction** -- Deleted keys are retained as tombstones for a configurable period, then cleaned up automatically.
- **Event subscriptions** -- Clients can subscribe to key changes in real time and configure webhook delivery for external integrations.

**Why it matters:**

- Encryption is not optional -- it is the only mode of operation.
- Version history and tombstone retention are built into the storage engine, not layered on top.
- Namespace-level schemas enforce data integrity at the database level, not in application code.
- A single master key protects all namespaces through derived keys, simplifying key management.

---

## For Technical Leaders: Architecture and Trade-offs

### The Problem

Encrypting application data at rest typically requires stitching together a database, a key management layer, schema validation middleware, and audit logging. Each seam is a potential failure point. Teams either accept the operational complexity or skip encryption for "internal" data that turns out to be sensitive.

### What ShrouDB Is

ShrouDB is a **standalone encrypted key-value database** -- not a credential vault, not a secrets manager, not an auth server. It stores arbitrary key-value data encrypted at rest and exposes it over a RESP3 wire protocol. Think of it as an encrypted data store that applications talk to directly over TCP/TLS.

### Key Architectural Decisions

| Decision | Rationale |
|----------|-----------|
| **Standalone server, not a library** | Data lives in a single hardened process with its own durability guarantees, not scattered across application instances. |
| **Custom storage engine** | WAL + snapshot persistence with crash recovery. No dependency on an external database. Reduces attack surface and operational complexity. |
| **Encrypted at rest by default** | AES-256-GCM with HKDF-derived per-namespace keys from a master key. Not optional -- there is no unencrypted mode. |
| **RESP3 wire protocol** | Binary-safe, widely understood, efficient for pipelining. Runs over TCP with optional TLS. |
| **Fail-closed design** | ShrouDB refuses service rather than serving potentially incorrect results. Corrupt data halts recovery rather than silently skipping entries. |

### Operational Model

- **Authentication:** Token-based auth with ACL grants scoped per namespace. Tokens are configured statically and applied on restart; hot-reload is supported for token and rate-limit changes.
- **Durability:** Write-ahead log with periodic snapshots. Crash recovery replays the WAL from the last consistent snapshot.
- **Idempotency:** Pipeline requests carry request IDs for exactly-once semantics on retries.
- **Export/Import:** Namespaces can be exported and imported for migration between instances.
- **Observability:** Prometheus metrics, OpenTelemetry export, audit file logging, structured tracing.
- **Deployment:** Single static binary. TLS supported natively. No external dependencies at runtime.

### Ecosystem

ShrouDB is the core database. Around it:

- **shroudb-auth** -- Authentication engine: signup, login, session management, token refresh, and revocation.
- **shroudb-transit** -- Encryption-as-a-service: encrypt, decrypt, sign, and verify data without exposing keys to application code.
- **shroudb-veil** -- Encrypted search engine that queries over end-to-end encrypted data without exposing plaintext.
- **shroudb-mint** -- Internal Certificate Authority for issuing short-lived TLS certificates and mTLS bootstrapping.
- **shroudb-sentry** -- Policy-based authorization engine that returns cryptographically signed decision tokens.
- **shroudb-keep** -- Encrypted secrets manager with versioning, path-based access control, and rotation.
- **shroudb-courier** -- Secure notification delivery pipeline with encrypted recipients.
- **shroudb-pulse** -- Centralized audit event ingestion and analytics across all ShrouDB engines.
- **shroudb-moat** -- Single-binary hub that embeds all engines with one config file and one set of auth tokens.
- **shroudb-codegen** -- Generates typed client SDKs from `protocol.toml` specifications.

All engines implement ShrouDB's Store trait and use ShrouDB as their persistence layer.
