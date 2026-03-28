# ShrouDB

A versioned, encrypted state store where tenancy, access control, and cryptographic isolation are built into the data model — not layered on top.

## Why ShrouDB

Most systems treat encryption, access control, multi-tenancy, and auditability as separate concerns layered above storage.

ShrouDB makes them properties of the storage layer itself:

- Every write is **versioned** and auditable — no blind overwrites, no data erasure ambiguity
- Every tenant is **cryptographically isolated** via HKDF-derived keys — unlike traditional multi-tenant systems that rely on logical separation, ShrouDB derives independent encryption keys per tenant and namespace, making isolation a cryptographic property rather than a policy decision
- Every access is **scoped and enforced** at the protocol boundary — before handlers run
- Every change is emitted as a **structured event** with version metadata, enabling real-time replication, indexing, and downstream processing without external CDC systems

This enables building higher-level systems — authentication, secret management, policy enforcement — on a shared, consistent foundation.

## Core guarantees

- **Versioned state** — every PUT increments the version. Full history queryable via VERSIONS. DELETE writes a tombstone, not an erasure.
- **Cryptographic tenant isolation** — HKDF-SHA256 derives a unique AES-256-GCM key per namespace. Tenant identity resolved from auth token. Compromise of one namespace does not expose others.
- **Tombstone deletes** — deletions are auditable events with version history, not silent data removal.

## Security model

- **Token-scoped access** with namespace-level read/write/admin grants, enforced at dispatch
- **TLS and mTLS** with constant-time token validation
- **Zeroize-on-drop** and mlock-pinned key material. Core dumps disabled.
- **Audit trail** — every write operation logged to structured audit file. Reads are not logged.

## System capabilities

- **RESP3 wire protocol** — Redis-compatible clients work out of the box. 20 commands, pipelining, push frames.
- **SUBSCRIBE** — real-time change stream with namespace, key, and event type filtering
- **Webhooks** — HMAC-SHA256 signed HTTP delivery with retry and backoff
- **Namespaces** with optional JSON metadata schemas (enforced on write, validated on demand)
- **Export/Import** — encrypted namespace bundles for migration and backup
- **Hot-reload** — auth tokens and rate limits reload from config without restart

## Quick Start

```sh
# Run (dev mode — ephemeral key, no auth)
cargo run

# Connect with the CLI
cargo run --bin shroudb-cli

# Or with Docker
docker run -p 6399:6399 \
  -e SHROUDB_MASTER_KEY="$(openssl rand -hex 32)" \
  -v shroudb-data:/data \
  ghcr.io/shroudb/shroudb
```

The server listens on `0.0.0.0:6399`. Zero config required for development.

## Example Session

```
shroudb> NAMESPACE CREATE myapp.users
OK

shroudb> PUT myapp.users user:1 alice
OK version: 1

shroudb> PUT myapp.users user:1 alice-updated
OK version: 2

shroudb> GET myapp.users user:1 VERSION 1
key: user:1  value: alice  version: 1

shroudb> DELETE myapp.users user:1
OK version: 3

shroudb> VERSIONS myapp.users user:1
version: 3  state: deleted
version: 2  state: active
version: 1  state: active
```

## Example: Secure Multi-Tenant Backend

- Store per-tenant application data in isolated namespaces
- Use tokens to enforce tenant and actor identity
- Stream changes to downstream services via SUBSCRIBE
- Maintain full audit history via versioned keys and tombstones

Reduces or eliminates the need for external secrets managers, policy engines, and CDC pipelines.

## Connection String

```
shroudb://[token@]host[:port]
shroudb+tls://[token@]host[:port]
```

## Configuration

| Setting | CLI flag | Env var | Default |
|---------|----------|---------|---------|
| Config file | `-c, --config` | `SHROUDB_CONFIG` | `config.toml` |
| Master key | — | `SHROUDB_MASTER_KEY` | ephemeral (dev) |
| Master key file | — | `SHROUDB_MASTER_KEY_FILE` | — |
| Data directory | `--data-dir` | `SHROUDB_DATA_DIR` | `./data` |
| Bind address | `--bind` | `SHROUDB_BIND` | `0.0.0.0:6399` |
| Log level | `--log-level` | `SHROUDB_LOG_LEVEL` | `info` |

Precedence: CLI flag > env var > TOML config > default.

### Master Key

```sh
openssl rand -hex 32
export SHROUDB_MASTER_KEY="<64-hex-chars>"
```

Without a master key, the server starts in dev mode with an ephemeral key — data will not survive restarts.

See [`config.example.toml`](config.example.toml) for all options including auth tokens, TLS, rate limits, webhooks, and storage tuning.

## Commands

20 commands across six categories:

| Command | Description |
|---------|-------------|
| **Connection** | |
| `AUTH <token>` | Authenticate connection |
| `PING` | Test connectivity |
| **Data** | |
| `PUT <ns> <key> <value> [META <json>]` | Store a value (auto-increments version) |
| `GET <ns> <key> [VERSION <n>] [META]` | Retrieve a value |
| `DELETE <ns> <key>` | Delete (tombstone) |
| `LIST <ns> [PREFIX <p>] [CURSOR <c>] [LIMIT <n>]` | List active keys |
| `VERSIONS <ns> <key> [LIMIT <n>] [FROM <v>]` | Version history |
| **Namespace** | |
| `NAMESPACE CREATE <name> [SCHEMA <json>]` | Create namespace |
| `NAMESPACE DROP <name> [FORCE]` | Drop namespace |
| `NAMESPACE LIST` | List namespaces |
| `NAMESPACE INFO <name>` | Namespace metadata |
| `NAMESPACE ALTER <name> [SCHEMA <json>]` | Update config |
| `NAMESPACE VALIDATE <name>` | Check entries against schema |
| **Batch** | |
| `PIPELINE [REQUEST_ID <id>] <commands...>` | Execute multiple commands as a single unit. If any command fails, no partial writes are committed. Optional REQUEST_ID for idempotent retries. |
| **Streaming** | |
| `SUBSCRIBE <ns> [KEY <k>] [EVENTS <types>]` | Subscribe to changes |
| `UNSUBSCRIBE` | End subscription |
| **Operational** | |
| `HEALTH` | Health check |
| `CONFIG GET/SET` | Runtime config |
| `COMMAND LIST` | List all commands |

## Access Control

When `auth.method = "token"` is configured, clients must `AUTH` before any data command. Each token carries:

- **Tenant** — cryptographic boundary (HKDF key derivation context)
- **Actor** — audit trail identity
- **Grants** — namespace-scoped `read` / `write` permissions
- **Platform flag** — unrestricted cross-tenant access

```toml
[auth]
method = "token"

[auth.tokens.my-app-token]
tenant = "tenant-a"
actor = "my-app"
grants = [
    { namespace = "myapp.users", scopes = ["read", "write"] },
    { namespace = "myapp.sessions", scopes = ["read", "write"] },
]
```

ACL is enforced at the dispatcher level — handlers never see unauthorized requests.

## Installation

### Docker

```sh
docker run -p 6399:6399 \
  -e SHROUDB_MASTER_KEY="$(openssl rand -hex 32)" \
  -v shroudb-data:/data \
  ghcr.io/shroudb/shroudb
```

### Homebrew

```sh
brew install shroudb/tap/shroudb
```

### Binary

Download from [GitHub Releases](https://github.com/shroudb/shroudb/releases). Linux (x86_64, aarch64) and macOS (x86_64, Apple Silicon).

## Architecture

- **Storage:** WAL with periodic snapshots, AES-256-GCM encrypted at rest with per-namespace HKDF-derived keys
- **Wire protocol:** RESP3, 20 commands, ACL middleware, per-connection rate limiting
- **Deployment:** Embedded (in-process via Store trait) or remote (TCP/TLS via RemoteStore)
- **Crates:** `shroudb-store` (trait), `shroudb-storage` (engine), `shroudb-acl` (auth), `shroudb-crypto` (AEAD/HKDF), `shroudb-client` (TCP client + RemoteStore)

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full crate map and data flow.

## Scope

ShrouDB is not designed for large-scale analytical workloads or full relational querying. It is optimized for operational state, security-sensitive data, and system coordination.

## Building on ShrouDB

ShrouDB is designed as a foundation for higher-level systems:

- Authentication and token services
- Secret management
- Encryption-as-a-service
- Policy enforcement and audit

For example, a token service can issue credentials stored in ShrouDB, enforce access via namespace-scoped grants, and stream audit events to downstream systems — all without external dependencies.

These systems run on top of the ShrouDB protocol — embedded or remote — through the `Store` trait.

## License

MIT OR Apache-2.0
