# ShrouDB Documentation

ShrouDB is an encrypted credential management server. It stores, issues, verifies, rotates, and revokes cryptographic credentials — JWT tokens, API keys, HMAC secrets, refresh tokens, and passwords — with encrypted-at-rest storage, automatic key rotation, and fine-grained access control.

---

## Table of Contents

- [Installation](#installation)
- [Quick Start](#quick-start)
- [Connection String](#connection-string)
- [Configuration](#configuration)
  - [Server](#server)
  - [Storage](#storage)
  - [Keyspaces](#keyspaces)
  - [Access Control](#access-control)
  - [Master Key](#master-key)
  - [Webhooks](#webhooks)
- [Keyspace Types](#keyspace-types)
  - [JWT](#jwt)
  - [API Key](#api-key)
  - [HMAC](#hmac)
  - [Refresh Token](#refresh-token)
  - [Password](#password)
- [Commands](#commands)
  - [Credential Operations](#credential-operations)
  - [Key Management](#key-management)
  - [Password Operations](#password-operations)
  - [Query and Inspection](#query-and-inspection)
  - [Configuration Commands](#configuration-commands)
  - [Events and Subscriptions](#events-and-subscriptions)
  - [Pipelining](#pipelining)
- [CLI Reference](#cli-reference)
- [Metadata Schemas](#metadata-schemas)
- [Operational Commands](#operational-commands)
- [Docker](#docker)
- [Observability](#observability)
- [Security Model](#security-model)

---

## Installation

### Homebrew

```sh
brew install shroudb/tap/shroudb
```

This installs both `shroudb` (server) and `shroudb-cli` (client).

### Docker

```sh
docker pull shroudb/shroudb      # Server
docker pull shroudb/cli          # CLI client
```

### Prebuilt Binaries

Download from [GitHub Releases](https://github.com/shroudb/shroudb/releases). Available for:

- Linux x86_64
- Linux ARM64 (aarch64)
- macOS x86_64
- macOS Apple Silicon

### Build from Source

Requires Rust 1.92 or later.

```sh
git clone https://github.com/shroudb/shroudb
cd shroudb
cargo build --release
```

The server binary is at `./target/release/shroudb` and the CLI at `./target/release/shroudb-cli`.

---

## Quick Start

Start the server in dev mode (ephemeral master key, data lost on restart):

```sh
shroudb
```

Connect with the CLI:

```sh
shroudb-cli
```

Issue and verify a JWT:

```sh
shroudb> ROTATE auth-tokens FORCE
shroudb> ROTATE auth-tokens FORCE
shroudb> ISSUE auth-tokens CLAIMS {"sub":"user1","aud":"my-app"} TTL 3600
# => {status: OK, credential_id: "cred_abc123", token: "eyJhbGc..."}

shroudb> VERIFY auth-tokens eyJhbGc...
# => {status: OK, credential_id: "cred_abc123", ...}
```

Issue and verify an API key:

```sh
shroudb> ISSUE service-keys
# => {status: OK, credential_id: "cred_xyz", token: "sk_abc123def..."}

shroudb> VERIFY service-keys sk_abc123def
# => {status: OK, credential_id: "cred_xyz"}
```

For production use, provide a [master key](#master-key) and a [configuration file](#configuration).

---

## Connection String

```
shroudb://[token@]host[:port][/keyspace]
shroudb+tls://[token@]host[:port][/keyspace]
```

| Example | Description |
|---------|-------------|
| `shroudb://localhost` | Plain TCP, default port 6399 |
| `shroudb+tls://prod.example.com` | TLS-encrypted connection |
| `shroudb://mytoken@localhost:6399` | With authentication token |
| `shroudb+tls://tok@host:6399/sessions` | TLS + auth + default keyspace |

```sh
shroudb-cli --uri shroudb://localhost:6399
```

---

## Configuration

ShrouDB is configured with a TOML file. Copy `config.example.toml` as a starting point:

```sh
cp config.example.toml config.toml
shroudb --config config.toml
```

Environment variables can be interpolated with `${VAR_NAME}` syntax anywhere in the config file.

### Server

```toml
[server]
bind = "0.0.0.0:6399"              # Listen address (TCP)
metrics_bind = "0.0.0.0:9090"      # Prometheus metrics endpoint
# tls_cert = "/path/to/cert.pem"   # Enable TLS
# tls_key = "/path/to/key.pem"
# tls_client_ca = "/path/to/ca.pem"  # Enable mutual TLS (mTLS)
# unix_socket = "/var/run/shroudb.sock"
# rate_limit = 1000                 # Max commands per second per connection
# otel_endpoint = "http://localhost:4317"  # OpenTelemetry OTLP endpoint
```

### Storage

```toml
[storage]
data_dir = "./data"                        # WAL segments and snapshots
wal_fsync_mode = "batched"                 # "per_write", "batched", or "periodic"
wal_fsync_interval_ms = 10                 # Flush interval for batched mode
wal_segment_max_bytes = 67_108_864         # 64 MiB max per WAL segment
snapshot_interval_entries = 100_000        # Snapshot every N write entries
snapshot_interval_minutes = 60             # Snapshot every N minutes
```

**Fsync modes:**

| Mode | Durability | Throughput |
|------|-----------|------------|
| `per_write` | Every write is durable before response | Lowest |
| `batched` | Writes are flushed on a timer (default: 10ms) | Balanced |
| `periodic` | Fsync at larger intervals | Highest |

### Keyspaces

Each keyspace defines a credential type and its behavior. Keyspaces are defined under `[keyspaces.<name>]`.

```toml
[keyspaces.auth-tokens]
type = "jwt"
algorithm = "ES256"
rotation_days = 90
drain_days = 30
pre_stage_days = 7
default_ttl = "15m"

[keyspaces.service-keys]
type = "api_key"
prefix = "sk"

[keyspaces.webhooks]
type = "hmac"
algorithm = "sha256"
rotation_days = 180
drain_days = 14

[keyspaces.sessions]
type = "refresh_token"
token_ttl = "30d"
max_chain_length = 100
family_ttl = "90d"

[keyspaces.users]
type = "password"
algorithm = "argon2id"
max_failed_attempts = 5
lockout_duration = "15m"
```

Set `disabled = true` on a keyspace to soft-disable it. Commands will be rejected but data is retained.

### Access Control

Without an `[auth]` section, all connections are unrestricted. To require authentication:

```toml
[auth]
method = "token"

[auth.policies.admin]
token = "${SHROUDB_ADMIN_TOKEN}"
keyspaces = ["*"]
commands = ["*"]

[auth.policies.reader]
token = "${SHROUDB_READER_TOKEN}"
keyspaces = ["auth-tokens", "service-keys"]
commands = ["VERIFY", "INSPECT", "JWKS", "HEALTH", "KEYS"]
```

Each policy maps a bearer token to a set of allowed keyspaces and commands. Use `["*"]` to grant access to all keyspaces or all commands.

Authenticate a connection with:

```sh
shroudb> AUTH mytoken
```

### Master Key

The master key encrypts all data at rest. In production, always set a master key.

```sh
# Generate a key
openssl rand -hex 32

# Option 1: Environment variable
export SHROUDB_MASTER_KEY="<64-hex-characters>"

# Option 2: Key file
export SHROUDB_MASTER_KEY_FILE="/etc/shroudb/master.key"
```

Without a master key, the server starts in **dev mode** with an ephemeral key. Data will not survive restarts.

To rotate the master key (server must be stopped):

```sh
shroudb rekey --old-key <old_hex> --new-key <new_hex> --config config.toml
```

### Webhooks

ShrouDB can deliver HMAC-signed HTTP notifications when credential lifecycle events occur.

```toml
[[webhooks]]
url = "https://api.example.com/webhooks"
secret = "${WEBHOOK_SECRET}"
events = ["rotate", "revoke"]
max_retries = 3
```

Events are delivered asynchronously with exponential backoff on failure.

---

## Keyspace Types

### JWT

Asymmetric signing keys with automatic rotation and a JWKS endpoint.

**Algorithms:** ES256, ES384, RS256, RS384, RS512, EdDSA

**Key lifecycle:**

```
Staged --> Active --> Draining --> Retired
```

- **Staged:** Key exists but is not yet used for signing. Created `pre_stage_days` before the active key expires.
- **Active:** Current signing key. Tokens are signed with this key.
- **Draining:** Previous active key. No longer signs new tokens, but still verifies existing ones.
- **Retired:** Fully decommissioned. No longer used for verification.

**Configuration:**

```toml
[keyspaces.auth-tokens]
type = "jwt"
algorithm = "ES256"
rotation_days = 90        # How long a key stays active
drain_days = 30           # How long a retired key still verifies tokens
pre_stage_days = 7        # How early to stage the next key
default_ttl = "15m"       # Default token lifetime
# required_claims = { aud = "my-service" }
```

**Usage:**

```sh
# Initialize signing keys (two rotations required for first active key)
ROTATE auth-tokens FORCE
ROTATE auth-tokens FORCE

# Issue a token
ISSUE auth-tokens CLAIMS {"sub":"user1","role":"admin"} TTL 3600

# Verify a token
VERIFY auth-tokens eyJhbGciOi...

# Get the public key set (for external verification)
JWKS auth-tokens

# View key ring state
KEYSTATE auth-tokens
```

### API Key

Bearer tokens with hashed storage and an optional prefix.

**Format:** `prefix_base62(random)` (e.g., `sk_a1B2c3D4...`)

**Configuration:**

```toml
[keyspaces.service-keys]
type = "api_key"
prefix = "sk"                # Optional prefix
hash_algorithm = "sha256"    # Hash algorithm for storage
```

API keys are hashed on storage — the plaintext key is only returned once at issuance and cannot be retrieved again.

**Usage:**

```sh
# Issue a key
ISSUE service-keys
# => {token: "sk_abc123...", credential_id: "cred_xyz"}

# Verify a key
VERIFY service-keys sk_abc123...

# Temporarily disable
SUSPEND service-keys cred_xyz

# Re-enable
UNSUSPEND service-keys cred_xyz

# Permanently revoke
REVOKE service-keys cred_xyz
```

### HMAC

Symmetric signing keys for webhook signatures, request signing, and message authentication.

**Algorithms:** SHA-256, SHA-384, SHA-512

**Configuration:**

```toml
[keyspaces.webhooks]
type = "hmac"
algorithm = "sha256"
rotation_days = 180
drain_days = 14
```

**Usage:**

```sh
# Initialize keys
ROTATE webhooks FORCE
ROTATE webhooks FORCE

# Issue a credential
ISSUE webhooks

# Verify a signature
VERIFY webhooks <credential_id> PAYLOAD <data_to_verify>

# View key state
KEYSTATE webhooks
```

### Refresh Token

Rotating tokens with family-based revocation and reuse detection.

**Key concepts:**

- **Family:** A chain of tokens originating from a single issuance. Each refresh produces a new token in the same family.
- **Reuse detection:** If a consumed (already-refreshed) token is used again, the entire family is revoked. This detects token theft.
- **Chain limit:** Prevents infinite refresh chains.

**Lifecycle:**

```
Active --> Consumed (via REFRESH)
  |            |
  v            v (reuse detected)
Revoked    Entire family revoked
```

**Configuration:**

```toml
[keyspaces.sessions]
type = "refresh_token"
token_ttl = "30d"          # Lifetime of each token
max_chain_length = 100     # Max refreshes in one family
family_ttl = "90d"         # Max lifetime of an entire family
```

**Usage:**

```sh
# Issue a refresh token
ISSUE sessions
# => {token: "rt_abc...", family_id: "fam_123"}

# Rotate the token (old token consumed, new token returned)
REFRESH sessions rt_abc
# => {token: "rt_def...", family_id: "fam_123"}

# Revoke by credential ID
REVOKE sessions cred_xyz

# Revoke an entire family
REVOKE sessions FAMILY fam_123
```

### Password

Hashed password storage with rate limiting, lockout, and transparent rehashing.

**Algorithms:** Argon2id (recommended), bcrypt, scrypt

**Features:**

- **Rate limiting:** Configurable `max_failed_attempts` and `lockout_duration` to protect against brute-force attacks.
- **Transparent rehashing:** When hash parameters change (e.g., increased cost), passwords are automatically rehashed on next successful verification.
- **Import support:** Migrate existing password hashes from other systems.

**Configuration:**

```toml
[keyspaces.users]
type = "password"
algorithm = "argon2id"
max_failed_attempts = 5    # Lock after 5 failures
lockout_duration = "15m"   # Lockout period
```

**Usage:**

```sh
# Set a password
PASSWORD SET users alice mysecretpassword

# Set with metadata
PASSWORD SET users alice mysecretpassword META {"role":"admin"}

# Verify a password
PASSWORD VERIFY users alice mysecretpassword

# Change a password (requires old password)
PASSWORD CHANGE users alice oldpassword newpassword

# Import a pre-hashed password
PASSWORD IMPORT users bob "$argon2id$v=19$m=65536,t=3,p=4$..."
```

---

## Commands

### Credential Operations

#### ISSUE

Create a new credential.

```
ISSUE <keyspace> [CLAIMS <json>] [META <json>] [TTL <seconds>] [IDEMPOTENCY_KEY <key>]
```

- `CLAIMS` — JWT claims (merged as top-level fields in the token).
- `META` — Metadata attached to the credential. Validated against the keyspace schema if one is configured.
- `TTL` — Time-to-live in seconds. Overrides the keyspace `default_ttl`.
- `IDEMPOTENCY_KEY` — Prevents duplicate issuance on retries (5-minute deduplication window).

**Returns:** `credential_id`, `token` (the issued credential), and type-specific fields.

#### VERIFY

Verify a credential's validity.

```
VERIFY <keyspace> <token> [PAYLOAD <data>] [CHECKREV]
```

- `PAYLOAD` — For HMAC verification, the data that was signed.
- `CHECKREV` — Explicitly check the revocation list (some types check automatically).

**Returns:** Verification status, `credential_id`, and type-specific fields (expiration, claims, etc.).

#### REVOKE

Revoke one or more credentials.

```
REVOKE <keyspace> <credential_id>
REVOKE <keyspace> FAMILY <family_id>
REVOKE <keyspace> BULK <id1> <id2> ...
```

- Single revocation by credential ID.
- `FAMILY` — Revoke all tokens in a refresh token family.
- `BULK` — Revoke multiple credentials in one call.

Revoked credentials cannot be un-revoked.

#### REFRESH

Rotate a refresh token. The old token is consumed and a new one is returned.

```
REFRESH <keyspace> <token>
```

If the provided token has already been consumed, reuse is detected and the entire token family is revoked.

**Returns:** New `token` and `family_id`.

#### UPDATE

Update metadata on a credential.

```
UPDATE <keyspace> <credential_id> META <json>
```

Fields marked as `immutable` in the metadata schema cannot be changed after initial issuance.

#### SUSPEND / UNSUSPEND

Temporarily disable or re-enable a credential.

```
SUSPEND <keyspace> <credential_id>
UNSUSPEND <keyspace> <credential_id>
```

Suspended credentials fail verification but are not permanently revoked.

### Key Management

#### ROTATE

Trigger a signing key rotation (JWT and HMAC keyspaces).

```
ROTATE <keyspace> [FORCE] [NOWAIT] [DRYRUN]
```

- `FORCE` — Rotate immediately regardless of schedule.
- `NOWAIT` — Return immediately; rotation happens in the background.
- `DRYRUN` — Preview what would happen without making changes.

Two forced rotations are required to initialize a new keyspace (staged, then promoted to active).

#### JWKS

Retrieve the JSON Web Key Set for a JWT keyspace. Use this to distribute public keys for external token verification.

```
JWKS <keyspace>
```

**Returns:** A standard JWKS document with all active and draining public keys.

#### KEYSTATE

Show the current state of the key ring.

```
KEYSTATE <keyspace>
```

**Returns:** Each key's ID, state (staged/active/draining/retired), algorithm, and age.

### Password Operations

#### PASSWORD SET

```
PASSWORD SET <keyspace> <user_id> <password> [META <json>]
```

#### PASSWORD VERIFY

```
PASSWORD VERIFY <keyspace> <user_id> <password>
```

Returns success or failure. If the account is locked due to too many failed attempts, returns an error with the remaining lockout duration.

#### PASSWORD CHANGE

```
PASSWORD CHANGE <keyspace> <user_id> <old_password> <new_password>
```

Requires the current password for verification before setting the new one.

#### PASSWORD IMPORT

```
PASSWORD IMPORT <keyspace> <user_id> <hash> [META <json>]
```

Import a pre-hashed password from another system. Useful for migrations.

### Query and Inspection

#### INSPECT

Get full details about a credential.

```
INSPECT <keyspace> <credential_id>
```

**Returns:** Credential state, metadata, creation time, expiration, and type-specific details.

#### KEYS

List credentials in a keyspace with cursor-based pagination.

```
KEYS <keyspace> [CURSOR <cursor>] [MATCH <pattern>] [STATE <state>] [COUNT <n>]
```

- `CURSOR` — Pagination cursor from a previous `KEYS` response. Use `0` to start.
- `MATCH` — Filter by credential ID pattern.
- `STATE` — Filter by state (e.g., `active`, `suspended`, `revoked`).
- `COUNT` — Hint for how many results to return per page.

**Returns:** A list of credential IDs and a `next_cursor` (0 when no more results).

#### SCHEMA

Display the metadata validation schema for a keyspace.

```
SCHEMA <keyspace>
```

#### HEALTH

Check server or keyspace health.

```
HEALTH
HEALTH <keyspace>
```

### Configuration Commands

#### CONFIG GET / SET / LIST

Query or modify runtime configuration.

```
CONFIG GET <key>
CONFIG SET <key> <value>
CONFIG LIST
```

Changes made with `CONFIG SET` are persisted and survive restarts.

### Events and Subscriptions

#### SUBSCRIBE

Subscribe to real-time lifecycle events.

```
SUBSCRIBE <channel>
```

Events include: `rotate`, `revoke`, `issue`, `suspend`, `unsuspend`, `reuse_detected`, `family_revoked`.

### Pipelining

Send multiple commands in a single batch for reduced round-trip latency.

```
PIPELINE
ISSUE service-keys
ISSUE service-keys
VERIFY service-keys sk_abc123
END
```

All commands in a pipeline are executed sequentially and their responses are returned together.

---

## CLI Reference

The `shroudb-cli` provides both interactive and non-interactive modes.

### Interactive Mode

```sh
shroudb-cli --uri shroudb://localhost:6399
```

Features:
- Tab completion for commands and keyspace names
- Command history
- Built-in help (`help` command)

### Non-Interactive Mode

Execute a single command and exit:

```sh
shroudb-cli --uri shroudb://localhost:6399 -- HEALTH
```

### Output Formats

```sh
shroudb-cli --format json    # JSON output
shroudb-cli --format human   # Human-readable (default)
shroudb-cli --format raw     # Raw protocol output
```

### Connection Options

```sh
shroudb-cli --uri shroudb://localhost:6399
shroudb-cli --host localhost --port 6399
shroudb-cli --host prod.example.com --tls
shroudb-cli --token mytoken --host localhost
```

---

## Metadata Schemas

Keyspaces can enforce a typed schema on credential metadata. This is useful for ensuring that all API keys have an `org_id`, or that all passwords have a `role`.

### Defining a Schema

```toml
[keyspaces.service-keys.meta_schema]
enforce = true

[keyspaces.service-keys.meta_schema.fields.org_id]
type = "string"
required = true
immutable = true         # Cannot be changed after issuance

[keyspaces.service-keys.meta_schema.fields.plan]
type = "string"
required = true
enum = ["free", "pro", "enterprise"]

[keyspaces.service-keys.meta_schema.fields.note]
type = "string"
```

### Field Types

| Type | Description |
|------|-------------|
| `string` | Text value |
| `integer` | Whole number |
| `float` | Decimal number |
| `boolean` | `true` or `false` |
| `array` | List of values |

### Field Options

| Option | Description |
|--------|-------------|
| `required` | Field must be present on `ISSUE` |
| `immutable` | Field cannot be changed after creation |
| `enum` | Restrict values to a specific set |
| `default` | Auto-filled value if not provided |

### Usage

```sh
# This succeeds — all required fields provided
ISSUE service-keys META {"org_id":"acme","plan":"pro"}

# This fails — missing required field "org_id"
ISSUE service-keys META {"plan":"pro"}

# This fails — "plan" not in enum
ISSUE service-keys META {"org_id":"acme","plan":"unlimited"}

# This fails — "org_id" is immutable
UPDATE service-keys cred_xyz META {"org_id":"other"}
```

---

## Operational Commands

These subcommands are run directly on the `shroudb` binary (not through the TCP protocol).

### doctor

Run a health check without starting the server. Validates configuration, storage integrity, and master key.

```sh
shroudb doctor --config config.toml
```

### rekey

Rotate the master encryption key. The server must be stopped. All data is re-encrypted with the new key.

```sh
shroudb rekey --old-key <old_hex> --new-key <new_hex> --config config.toml
```

### export

Export a keyspace to an encrypted bundle file.

```sh
shroudb export my-keyspace --output backup.kvex --config config.toml
```

### import

Import credentials from an encrypted bundle into a keyspace.

```sh
shroudb import --file backup.kvex --keyspace my-keyspace --config config.toml
```

### purge

Permanently delete all data for a keyspace. This is irreversible.

```sh
shroudb purge my-keyspace --yes --config config.toml
```

---

## Docker

### Ports

| Port | Purpose |
|------|---------|
| 6399 | Command protocol (TCP) |
| 9090 | Prometheus metrics |

### Quick Start

```sh
# Dev mode (ephemeral key, data lost on restart)
docker run -p 6399:6399 shroudb/shroudb

# Production
docker run -d \
  -p 6399:6399 -p 9090:9090 \
  -v shroudb-data:/data \
  -e SHROUDB_MASTER_KEY="$(openssl rand -hex 32)" \
  shroudb/shroudb
```

### With Config File

```sh
docker run -p 6399:6399 \
  -e SHROUDB_MASTER_KEY="$(openssl rand -hex 32)" \
  -v shroudb-data:/data \
  -v ./config.toml:/config.toml:ro \
  shroudb/shroudb --config /config.toml
```

### Docker Compose

```yaml
services:
  shroudb:
    image: shroudb/shroudb
    ports:
      - "6399:6399"
      - "9090:9090"
    environment:
      - SHROUDB_MASTER_KEY=${SHROUDB_MASTER_KEY}
      - LOG_LEVEL=info
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

### Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `SHROUDB_MASTER_KEY` | Yes (production) | 64 hex characters. Encrypts all data at rest. |
| `SHROUDB_MASTER_KEY_FILE` | Alternative | Path to a file containing the master key. |
| `LOG_LEVEL` | No | `info`, `debug`, or `warn`. Default: `info`. |

### Volume

Mount `/data` for persistent storage. Without a volume, data is lost when the container stops.

### CLI via Docker

```sh
docker run --rm -it --network host shroudb/cli --uri shroudb://localhost:6399
```

---

## Observability

### Prometheus Metrics

ShrouDB exposes a Prometheus-compatible metrics endpoint (default: `0.0.0.0:9090`).

Available metrics include:

- **Commands:** Total count, latency distribution, and error rate, broken down by command and keyspace
- **Credentials:** Active credential count per keyspace
- **Storage:** Write-ahead log write duration and segment size
- **Revocations:** Active revocation count per keyspace
- **Passwords:** Verification failures, lockouts, and rehashes
- **Key age:** Time since last rotation per keyspace

### Audit Logging

All credential operations are logged to `{data_dir}/audit.log` in structured JSON format. This includes the operation performed, the keyspace, credential ID, timestamp, and outcome.

### OpenTelemetry

ShrouDB supports OpenTelemetry Protocol (OTLP) export for integration with observability platforms.

```toml
[server]
otel_endpoint = "http://localhost:4317"
```

### Structured Logging

Server logs are output in JSON format to stdout, suitable for log aggregation systems.

Set the log level with the `LOG_LEVEL` environment variable:

```sh
LOG_LEVEL=debug shroudb --config config.toml
```

---

## Security Model

### Encryption at Rest

All data is encrypted with AES-256-GCM. Encryption keys are derived per-keyspace from the master key using HKDF-SHA256, ensuring that a compromise of one keyspace's derived key does not affect others.

### Memory Protection

- Secret values are pinned in memory (`mlock`) to prevent swapping to disk.
- All secrets are zeroized when they go out of scope.
- Core dumps are disabled at startup.

### Constant-Time Operations

All credential verification (password hashes, HMAC signatures, token comparisons) uses constant-time algorithms to prevent timing side-channel attacks.

### Fail-Closed Design

ShrouDB defaults to denying operations when in doubt:

- If the master key is unavailable, the server refuses to start (except in dev mode).
- If storage corruption is detected, affected entries are rejected rather than silently served.
- If authentication is configured, unauthenticated connections cannot execute commands.

### Password Security

- Passwords are never stored in plaintext. Only salted hashes are persisted.
- Rate limiting and lockout protect against brute-force attacks.
- Transparent rehashing ensures passwords are upgraded to stronger parameters over time.

### API Key Security

- API keys are stored as SHA-256 hashes. The plaintext is returned once at issuance and cannot be retrieved again.

### Network Security

- **TLS** encryption for all client connections.
- **Mutual TLS (mTLS)** for environments requiring client certificate authentication.
- **Unix domain sockets** for local-only access without network exposure.
- **Per-connection rate limiting** to protect against abuse.
