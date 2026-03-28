# ShrouDB

A versioned, encrypted state store where tenancy, access control, and cryptographic isolation are built into the data model — not layered on top.

## Why ShrouDB

Most systems treat encryption, access control, and multi-tenancy as separate concerns bolted onto storage. ShrouDB makes them properties of the storage layer itself — every namespace gets its own HKDF-derived encryption key, every access is scoped to an authenticated identity, and every mutation is versioned and emitted as a structured event.

This means isolation is cryptographic (not just logical), audit trails are inherent (not bolted on), and change propagation is built in (not requiring external CDC). The result is a minimal set of primitives — versioned keys, namespaces, identity-scoped tokens, and event streams — from which higher-level systems can be composed.

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

Listens on `0.0.0.0:6399`. Zero config for development.

## Example

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

Every PUT creates a new version. DELETE writes a tombstone — the full history remains queryable. The current state is always derivable from its version history, but ShrouDB exposes both the latest value and historical versions directly.

## What makes this different

**Cryptographic tenant isolation.** Each namespace gets a unique AES-256-GCM key derived via HKDF-SHA256 from the master key and tenant context. Compromise of one namespace's data or derived key does not expose others. This is not row-level filtering — it is independent key material per boundary.

**Versioned state with tombstones.** No blind overwrites. No silent deletes. Every mutation is a new version. Every deletion is an auditable event. Full history retained until compacted.

**Identity at the protocol layer.** Auth tokens carry tenant identity, actor name, and namespace-scoped grants. ACL enforcement happens at dispatch — handlers never see unauthorized requests. Tenant identity feeds into key derivation, binding crypto to access control.

**Durable writes.** Every write is persisted to the write-ahead log before acknowledgment. Periodic snapshots compact the log. Recovery replays WAL entries using the original timestamps — no clock-dependent corruption.

**Built-in change stream.** Every mutation emits a structured event (namespace, key, version, operation, actor, metadata). SUBSCRIBE filters by namespace, key, or event type. Webhooks deliver HMAC-signed events to HTTP endpoints with retry. No external CDC pipeline needed.

## Security

- AES-256-GCM encryption at rest with per-namespace HKDF-derived keys
- Constant-time token validation (no timing side-channels)
- Zeroize-on-drop and mlock-pinned key material
- TLS and mTLS support
- Core dumps disabled
- Structured audit log for all write operations (reads are not logged)

## Commands

20 commands across six categories:

| Category | Command | |
|----------|---------|---|
| Connection | `AUTH <token>` | Authenticate |
| | `PING` | Connectivity check |
| Data | `PUT <ns> <key> <value> [META <json>]` | Store (auto-increments version) |
| | `GET <ns> <key> [VERSION <n>] [META]` | Retrieve |
| | `DELETE <ns> <key>` | Tombstone |
| | `LIST <ns> [PREFIX <p>] [CURSOR <c>] [LIMIT <n>]` | List active keys |
| | `VERSIONS <ns> <key> [LIMIT <n>] [FROM <v>]` | Version history |
| Namespace | `NAMESPACE CREATE <name> [SCHEMA <json>]` | Create with optional metadata schema |
| | `NAMESPACE DROP <name> [FORCE]` | Drop |
| | `NAMESPACE LIST / INFO / ALTER / VALIDATE` | Manage |
| Batch | `PIPELINE [REQUEST_ID <id>] <commands...>` | Atomic batch — no partial writes on failure |
| Streaming | `SUBSCRIBE <ns> [KEY <k>] [EVENTS <types>]` | Real-time event stream |
| | `UNSUBSCRIBE` | End subscription |
| Operational | `HEALTH` / `CONFIG GET/SET` / `COMMAND LIST` | |

## Access Control

Tokens carry tenant identity, actor name, and namespace-scoped grants:

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

Tenant identity is the HKDF derivation context — the crypto boundary is the same as the access boundary.

## Configuration

| Setting | CLI flag | Env var | Default |
|---------|----------|---------|---------|
| Config file | `-c, --config` | `SHROUDB_CONFIG` | `config.toml` |
| Master key | — | `SHROUDB_MASTER_KEY` | ephemeral (dev) |
| Master key file | — | `SHROUDB_MASTER_KEY_FILE` | — |
| Data directory | `--data-dir` | `SHROUDB_DATA_DIR` | `./data` |
| Bind address | `--bind` | `SHROUDB_BIND` | `0.0.0.0:6399` |
| Log level | `--log-level` | `SHROUDB_LOG_LEVEL` | `info` |

Precedence: CLI > env > TOML > default. Auth tokens and rate limits hot-reload from config without restart.

```sh
openssl rand -hex 32                        # generate master key
export SHROUDB_MASTER_KEY="<64-hex-chars>"  # set it
```

Without a master key, dev mode uses an ephemeral key — data won't survive restarts.

See [`config.example.toml`](config.example.toml) for all options.

## Installation

**Docker:** `docker run -p 6399:6399 -e SHROUDB_MASTER_KEY="$(openssl rand -hex 32)" -v shroudb-data:/data ghcr.io/shroudb/shroudb`

**Homebrew:** `brew install shroudb/tap/shroudb`

**Binary:** [GitHub Releases](https://github.com/shroudb/shroudb/releases) — Linux (x86_64, aarch64) and macOS (x86_64, Apple Silicon).

**Connection:** `shroudb://[token@]host[:port]` or `shroudb+tls://[token@]host[:port]`

## Architecture

Writes go to an encrypted WAL, then to an in-memory index. Periodic snapshots compact the log. The RESP3 protocol handles framing — ShrouDB is not Redis and does not aim for Redis API compatibility.

Two deployment modes: **embedded** (in-process via `Store` trait) or **remote** (TCP/TLS via `RemoteStore`). Engine code is identical in both modes.

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full crate map and data flow.

## Scope

ShrouDB is optimized for operational state, security-sensitive data, and system coordination. It is not designed for analytical workloads or relational querying.

## Building on ShrouDB

ShrouDB is designed as a foundation for higher-level systems and shared infrastructure components — authentication, secret management, encryption-as-a-service, policy enforcement.

A token service, for example, stores credentials in a namespace, enforces access via grants, and streams audit events — all through ShrouDB primitives, with no external dependencies for secrets, policy, or change propagation.

The `Store` trait is the extension point. Embed ShrouDB in-process or connect over TCP/TLS — the engine code doesn't change.

## License

MIT OR Apache-2.0
