# ShrouDB

An encrypted key-value database built in Rust. Namespaced, versioned, with tombstone deletes and encrypted-at-rest storage. RESP3 wire protocol on port 6399.

## Features

- **Encrypted at rest:** AES-256-GCM with per-namespace derived keys (HKDF-SHA256), WAL + snapshots
- **Versioned:** Every PUT increments the version. Full version history accessible via VERSIONS
- **Tombstone deletes:** DELETE writes a tombstone — version history preserved, auditable
- **Namespaces:** Logical data partitions with per-namespace MetaSchema validation
- **Multi-tenant:** HKDF-derived key isolation per tenant. Tenant resolved from auth token
- **Wire protocol:** RESP3 on port 6399 with pipelining support
- **TLS and mTLS** with Unix socket support
- **Access control:** Token-based auth with namespace-scoped read/write/admin grants
- **Metadata schemas:** Optional typed, validated metadata on all entries with immutable field support
- **Security hardened:** Zeroize-on-drop for key material, mlock-pinned secrets, core dumps disabled

## Quick Start

```sh
# Run (dev mode — ephemeral master key, no auth)
cargo run

# Connect with the CLI
cargo run --bin shroudb-cli

# Or with a config file
cargo run -- --config config.toml
```

The server listens on `0.0.0.0:6399` by default. Zero config required for development.

## Connection String

```
shroudb://[token@]host[:port]
shroudb+tls://[token@]host[:port]
```

Examples:

```
shroudb://localhost                        # plain TCP, default port 6399
shroudb+tls://prod.example.com            # TLS
shroudb://mytoken@localhost:6399           # with auth token
```

```sh
shroudb-cli --uri shroudb://localhost:6399
```

## Configuration

ShrouDB follows the database convention: config file first, with env vars for secrets and deployment overrides.

| Setting | CLI flag | Env var | Default |
|---------|----------|---------|---------|
| Config file | `-c, --config` | `SHROUDB_CONFIG` | `config.toml` |
| Master key | — | `SHROUDB_MASTER_KEY` | ephemeral (dev) |
| Master key file | — | `SHROUDB_MASTER_KEY_FILE` | — |
| Data directory | `--data-dir` | `SHROUDB_DATA_DIR` | `./data` |
| Bind address | `--bind` | `SHROUDB_BIND` | `0.0.0.0:6399` |
| Log level | `--log-level` | `SHROUDB_LOG_LEVEL` | `info` |

Precedence: CLI flag > env var > TOML config > default.

```sh
cp config.example.toml config.toml
./target/release/shroudb --config config.toml
```

See [`config.example.toml`](config.example.toml) for all TOML options including auth tokens, TLS, storage tuning, and metrics.

### Master Key

```sh
# Generate a key
openssl rand -hex 32

# Set via environment variable
export SHROUDB_MASTER_KEY="<64-hex-chars>"

# Or via file
export SHROUDB_MASTER_KEY_FILE="/etc/shroudb/master.key"
```

Without a master key, the server starts in dev mode with an ephemeral key — data will not survive restarts.

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
| `NAMESPACE LIST [CURSOR <c>] [LIMIT <n>]` | List namespaces |
| `NAMESPACE INFO <name>` | Namespace metadata |
| `NAMESPACE ALTER <name> [SCHEMA <json>]` | Update config |
| `NAMESPACE VALIDATE <name>` | Check entries against schema |
| **Batch** | |
| `PIPELINE <count>` | Atomic batch (rollback on failure) |
| **Streaming** | |
| `SUBSCRIBE <ns> [KEY <k>] [EVENTS <types>]` | Subscribe to changes |
| `UNSUBSCRIBE` | End subscription |
| **Operational** | |
| `HEALTH` | Health check |
| `CONFIG GET <key>` | Read config value |
| `CONFIG SET <key> <value>` | Set config value (admin) |
| `COMMAND LIST` | List all commands |

### Example Session

```
shroudb> NAMESPACE CREATE myapp.users
OK

shroudb> PUT myapp.users user:1 alice
OK
version: 1

shroudb> PUT myapp.users user:1 alice-updated
OK
version: 2

shroudb> GET myapp.users user:1
key: user:1
value: alice-updated
version: 2

shroudb> GET myapp.users user:1 VERSION 1
key: user:1
value: alice
version: 1

shroudb> DELETE myapp.users user:1
OK
version: 3

shroudb> VERSIONS myapp.users user:1
versions:
  version: 3, state: Deleted
  version: 2, state: Active
  version: 1, state: Active
```

## Access Control

When `auth.method = "token"` is configured, clients must `AUTH` before any data command. Each token carries:

- **Tenant** — crypto boundary (HKDF key derivation)
- **Actor** — audit trail identity
- **Grants** — namespace-scoped `read` / `write` permissions
- **Platform flag** — unrestricted admin + cross-tenant access

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

ACL is enforced at the dispatcher level — every command declares its required scope, the compiler ensures exhaustive coverage, and handlers never see unauthorized requests.

> **Note:** Token management is currently static (config file, restart to change). Dynamic token CRUD will be available when Sentry v0.2 implements the `TokenValidator` trait with live policy evaluation.

## Installation

### Docker

```sh
docker run -p 6399:6399 \
  -e SHROUDB_MASTER_KEY="$(openssl rand -hex 32)" \
  -v shroudb-data:/data \
  ghcr.io/shroudb/shroudb
```

### Docker Compose

```yaml
services:
  shroudb:
    image: ghcr.io/shroudb/shroudb
    ports:
      - "6399:6399"
    environment:
      - SHROUDB_MASTER_KEY=${SHROUDB_MASTER_KEY}
      - SHROUDB_LOG_LEVEL=info
    volumes:
      - shroudb-data:/data
      - ./config.toml:/config.toml:ro
    command: ["--config", "/config.toml"]
    restart: unless-stopped

volumes:
  shroudb-data:
```

```sh
export SHROUDB_MASTER_KEY="$(openssl rand -hex 32)"
docker compose up -d
```

### Homebrew

```sh
brew install shroudb/tap/shroudb
```

### Binary

Download prebuilt binaries from [GitHub Releases](https://github.com/shroudb/shroudb/releases). Available for Linux (x86_64, aarch64) and macOS (x86_64, Apple Silicon).

## Architecture

- **Storage:** Write-ahead log (WAL) with periodic snapshots, AES-256-GCM encrypted at rest with per-namespace derived keys (HKDF-SHA256)
- **Wire protocol:** RESP3 with 20 commands, ACL middleware, and per-connection rate limiting
- **Workspace crates:** `shroudb-server`, `shroudb-protocol`, `shroudb-client`, `shroudb-cli`
- **Commons crates:** `shroudb-store` (Store trait), `shroudb-acl` (ACL primitives), `shroudb-storage` (WAL/index/snapshots), `shroudb-crypto` (AEAD/HKDF/HMAC)

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full crate map and data flow.

## License

MIT OR Apache-2.0
