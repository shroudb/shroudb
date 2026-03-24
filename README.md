# ShrouDB

A credential management server built in Rust. Manages JWT signing keys, API keys, HMAC secrets, refresh tokens, and passwords with encrypted-at-rest storage, automatic key rotation, and a RESP3 wire protocol.

## Features

- **Five keyspace types:** JWT, API key, HMAC, refresh token, and password
- **JWT algorithms:** ES256, ES384, RS256, RS384, RS512, EdDSA with automatic key rotation
- **Password hashing:** Argon2id, bcrypt, scrypt with lockout and transparent rehash
- **Encrypted storage:** AES-256-GCM at rest, per-keyspace derived keys (HKDF-SHA256), WAL + snapshots
- **Dual protocol:** RESP3 (port 6399) and REST API (port 8080)
- **TLS and mTLS** on the RESP3 protocol, with Unix socket support
- **Access control:** token-based auth with per-policy keyspace and command restrictions
- **Metadata schemas:** optional typed, validated metadata on credentials with immutable field support
- **Pub/sub:** real-time event notifications on keyspace channels
- **Pipelining:** batch multiple commands in a single round-trip
- **Security hardened:** `mlock`-pinned secrets, zeroize-on-drop, core dumps disabled, constant-time comparisons

## Quick Start

```sh
# Build and run (dev mode — ephemeral master key, human-readable logs)
cargo run

# Connect with the CLI
cargo run --bin shroudb-cli

# Or with a config file
cargo run -- --config config.toml
```

The server listens on `0.0.0.0:6399` (RESP3) and `0.0.0.0:8080` (REST) by default.

## Connection String

```
shroudb://[token@]host[:port][/keyspace]
shroudb+tls://[token@]host[:port][/keyspace]
```

Examples:

```
shroudb://localhost                        # plain TCP, default port 6399
shroudb+tls://prod.example.com            # TLS
shroudb://mytoken@localhost:6399           # with auth token
shroudb+tls://tok@host:6399/sessions      # TLS + auth + default keyspace
```

```sh
shroudb-cli --uri shroudb://localhost:6399
```

## Configuration

Copy and edit the example config:

```sh
cp config.example.toml config.toml
./target/release/shroudb --config config.toml
```

Environment variables can be interpolated with `${VAR_NAME}` syntax.

```toml
[server]
bind = "0.0.0.0:6399"
rest_bind = "0.0.0.0:8080"
# tls_cert = "/path/to/cert.pem"
# tls_key = "/path/to/key.pem"
# tls_client_ca = "/path/to/ca.pem"  # mTLS

[storage]
data_dir = "./data"
wal_fsync_mode = "batched"
wal_fsync_interval_ms = 10

[keyspaces.auth-tokens]
type = "jwt"
algorithm = "ES256"
rotation_days = 90
default_ttl = "15m"

[keyspaces.service-keys]
type = "api_key"
prefix = "sk"

[keyspaces.sessions]
type = "refresh_token"
token_ttl = "30d"

[keyspaces.users]
type = "password"
algorithm = "argon2id"
max_failed_attempts = 5
lockout_duration = "15m"

# [auth]
# method = "token"
# [auth.policies.admin]
# token = "${SHROUDB_ADMIN_TOKEN}"
# keyspaces = ["*"]
# commands = ["*"]
```

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

## Keyspace Types

| Type | Description |
|------|-------------|
| `jwt` | Asymmetric signing keys with automatic rotation and JWKS endpoint |
| `api_key` | Bearer tokens with SHA-256 hashed storage and optional prefix |
| `hmac` | Symmetric HMAC keys (SHA-256/384/512) with rotation |
| `refresh_token` | Rotating refresh tokens with family-based revocation and chain tracking |
| `password` | Argon2id/bcrypt/scrypt password hashing with lockout and transparent rehash |

## Commands (RESP3)

| Command | Description |
|---------|-------------|
| `ISSUE <ks> [CLAIMS <json>] [META <json>] [TTL <s>]` | Issue a credential |
| `VERIFY <ks> <token> [PAYLOAD <data>] [CHECKREV]` | Verify a credential |
| `REVOKE <ks> <id> \| FAMILY <fid> \| BULK <ids...>` | Revoke credentials |
| `REFRESH <ks> <token>` | Rotate a refresh token |
| `UPDATE <ks> <id> META <json>` | Update credential metadata |
| `INSPECT <ks> <id>` | Get full credential details |
| `ROTATE <ks> [FORCE] [NOWAIT] [DRYRUN]` | Rotate signing keys |
| `JWKS <ks>` | Get the JSON Web Key Set |
| `KEYSTATE <ks>` | Show key ring state |
| `KEYS <ks> [CURSOR <c>] [MATCH <p>] [STATE <s>] [COUNT <n>]` | List credentials |
| `SUSPEND / UNSUSPEND <ks> <id>` | Suspend or reactivate a credential |
| `SCHEMA <ks>` | Display metadata schema |
| `PASSWORD SET <ks> <uid> <pw> [META <json>]` | Set a password |
| `PASSWORD VERIFY <ks> <uid> <pw>` | Verify a password |
| `PASSWORD CHANGE <ks> <uid> <old> <new>` | Change a password |
| `PASSWORD IMPORT <ks> <uid> <hash> [META <json>]` | Import a pre-hashed password |
| `SUBSCRIBE <channel>` | Subscribe to events |
| `PIPELINE ... END` | Batch commands |
| `AUTH <token>` | Authenticate connection |
| `CONFIG GET / SET <key> [<value>]` | Runtime config |
| `HEALTH [<ks>]` | Health check |

See [PROTOCOL.md](PROTOCOL.md) for the full wire protocol specification.

## REST API

Available when `rest_bind` is configured:

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/v1/{ks}/issue` | Issue a credential |
| `POST` | `/v1/{ks}/verify` | Verify a credential |
| `POST` | `/v1/{ks}/revoke` | Revoke a credential |
| `POST` | `/v1/{ks}/refresh` | Rotate a refresh token |
| `GET` | `/v1/{ks}/jwks` | JSON Web Key Set |
| `GET` | `/v1/{ks}/keys` | List credentials |
| `GET` | `/v1/{ks}/{id}` | Inspect a credential |
| `PUT` | `/v1/{ks}/{id}` | Update credential metadata |
| `DELETE` | `/v1/{ks}/{id}` | Revoke a credential |
| `GET` | `/health` | Health check |
| `GET` | `/metrics` | Prometheus metrics |

## Operational Commands

```sh
# Health check without starting the server
shroudb doctor --config config.toml

# Re-key (rotate master encryption key — server must be stopped)
shroudb rekey --old-key <old> --new-key <new> --config config.toml

# Export a keyspace to an encrypted bundle
shroudb export my-keyspace --output backup.kvex --config config.toml

# Import into another instance (same master key required)
shroudb import --file backup.kvex --keyspace my-keyspace --config config.toml

# Purge a keyspace
shroudb purge my-keyspace --config config.toml
```

## Docker

```sh
docker build -t shroudb .
docker run -p 6399:6399 -p 8080:8080 \
  -e SHROUDB_MASTER_KEY="$(openssl rand -hex 32)" \
  -v shroudb-data:/data \
  shroudb
```

Or with Docker Compose:

```sh
docker compose up -d
```

A systemd unit file is provided in `shroudb.service`.

## Architecture

- **Storage:** Write-ahead log (WAL) with periodic snapshots, AES-256-GCM encrypted at rest with per-keyspace derived keys (HKDF-SHA256)
- **RESP3 protocol:** Battle-tested framing with ShrouDB's own command set — not Redis-compatible
- **REST:** Axum-based HTTP API on a separate port
- **Workspace crates:** `shroudb-server`, `shroudb-protocol`, `shroudb-client`, `shroudb-cli`

## License

MIT OR Apache-2.0
