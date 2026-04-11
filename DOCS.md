# ShrouDB Documentation

## Installation

### Homebrew

```sh
brew install shroudb/tap/shroudb
```

Installs `shroudb` (server) and `shroudb-cli`.

### Docker

```sh
docker pull shroudb/shroudb
```

A CLI image is also available:

```sh
docker pull shroudb/cli
```

### Binary

Download prebuilt binaries from [GitHub Releases](https://github.com/shroudb/shroudb/releases). Available for Linux (x86_64, aarch64) and macOS (x86_64, Apple Silicon).

---

## Quick Start

```sh
# Start the server (dev mode -- ephemeral master key, human-readable logs)
shroudb

# Or with a config file
shroudb --config config.toml

# Connect with the CLI
shroudb-cli
```

The server listens on `0.0.0.0:6399` by default.

---

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
shroudb+tls://tok@host:6399               # TLS + auth
```

```sh
shroudb-cli --uri shroudb://localhost:6399
```

---

## Data Model

ShrouDB is an encrypted, versioned key-value database. Data is organized into **namespaces**, each containing keys that hold opaque byte values and optional JSON metadata.

- **Namespace** -- A named container for keys. Namespaces can enforce a JSON schema on metadata, cap version history, and control tombstone retention.
- **Key** -- A UTF-8 string within a namespace.
- **Value** -- Arbitrary bytes. Encrypted at rest with the master key.
- **Version** -- Auto-incrementing integer per key. Every PUT creates a new version; previous versions are retained up to the namespace's MAX_VERSIONS limit.
- **Metadata** -- Optional JSON object attached to each version. Validated against the namespace's schema if one is configured.
- **Tombstone** -- A DELETE writes a tombstone version. The key is excluded from LIST results but its version history is preserved until compaction.

---

## Configuration

Copy and edit the example config:

```sh
cp config.example.toml config.toml
shroudb --config config.toml
```

Environment variables can be interpolated with `${VAR_NAME}` syntax. Precedence: CLI flag > env var > config file > defaults.

### Config Sections

#### [server]

```toml
[server]
bind = "0.0.0.0:6399"
# tls_cert = "/etc/shroudb/tls/cert.pem"
# tls_key = "/etc/shroudb/tls/key.pem"
# tls_client_ca = "/etc/shroudb/tls/ca.pem"   # mTLS
# unix_socket = "/var/run/shroudb.sock"
# rate_limit_per_second = 10000
# metrics_bind = "0.0.0.0:9090"
```

#### [storage]

```toml
[storage]
data_dir = "./data"
# fsync_mode = "batched"          # per_write | batched | periodic
# fsync_interval_ms = 10          # for batched/periodic modes
# max_segment_bytes = 67108864    # 64 MiB
# max_segment_entries = 100000
# snapshot_interval_entries = 100000
# snapshot_interval_minutes = 60
```

#### [storage.cache]

Controls the bounded index — how much memory the KV index uses for value storage. By default, all values are held in memory. When a budget is configured, cold values are evicted to disk and recovered transparently on access.

```toml
[storage.cache]
memory_budget = "256mb"    # explicit byte limit
# memory_budget = "70%"   # fraction of system RAM
# memory_budget = "auto"  # 50% of system RAM, capped at 4 GiB
```

Omit this section entirely to keep all values in memory (no eviction).

**Eviction behavior:**

- **Version-level (automatic):** Only the two most recent versions (N and N-1) of each key stay resident. Older versions are evicted on write.
- **Key-level (LRU):** When total resident memory exceeds the budget, entire keys are evicted based on least-recent access.

Evicted values are recovered from the value log (vlog) or WAL on read. Cache misses are transparent to clients — `GET` always returns the correct value.

**Tuning:** Choose a budget based on your instance size, not your dataset size. On managed platforms (Fly, Railway) where container memory is the constraint, `"70%"` or `"auto"` means zero tuning — the cache sizes itself to the instance.

**Metrics:**

| Metric | Type | Description |
|--------|------|-------------|
| `shroudb_cache_hit_total` | counter | Reads served from memory (per namespace) |
| `shroudb_cache_miss_total` | counter | Reads recovered from disk (per namespace) |
| `shroudb_cache_eviction_total` | counter | Keys evicted by LRU (per namespace) |
| `shroudb_cache_memory_bytes` | gauge | Current resident value memory |
| `shroudb_cache_resident_keys` | gauge | Number of keys with resident values |
| `shroudb_cache_budget_bytes` | gauge | Configured memory budget |

Monitor `shroudb_cache_miss_total` to determine if the budget is well-sized. A climbing miss rate means the working set exceeds the budget.

**Compaction interaction:** The cache evicts values but retains key metadata (version numbers, timestamps, state). Compaction (`max_versions` and `tombstone_retention_secs` on namespace config) removes entire version records and tombstones, reducing metadata overhead. Tighter compaction settings complement the cache budget — fewer retained versions means less per-key metadata even for evicted keys.

#### [auth]

When `auth.method = "token"`, clients must `AUTH` with a configured token before any command except `PING` is accepted. Each token defines a tenant, actor identity, and namespace grants.

```toml
[auth]
method = "token"

[auth.tokens.my-app-token]
tenant = "tenant-a"
actor = "my-app"
grants = [
    { namespace = "app.users", scopes = ["read", "write"] },
    { namespace = "app.sessions", scopes = ["read", "write"] },
]

[auth.tokens.admin-token]
tenant = "platform"
actor = "admin"
platform = true    # unrestricted access, cross-tenant capable
```

#### [[webhooks]]

HTTP endpoints that receive signed event notifications. Payloads are signed with HMAC-SHA256 in the `X-ShrouDB-Signature-256` header.

```toml
[[webhooks]]
url = "https://example.com/shroudb-hook"
secret = "your-hmac-secret"
events = ["put", "delete"]           # empty = all events
namespaces = ["myapp.*"]             # empty = all namespaces, supports trailing *
max_retries = 5
timeout_ms = 5000
```

See [`config.example.toml`](config.example.toml) for all options.

### Master Key

```sh
# Generate a key
openssl rand -hex 32

# Set via environment variable
export SHROUDB_MASTER_KEY="<64-hex-chars>"

# Or via file
export SHROUDB_MASTER_KEY_FILE="/etc/shroudb/master.key"
```

Without a master key, the server starts in dev mode with an ephemeral key -- data will not survive restarts.

---

## Commands

### Data Commands

| Command | Description |
|---------|-------------|
| `PUT ns key [VALUE value] [META json]` | Store a value. Auto-increments version. Returns the new version number. |
| `GET ns key [VERSION n] [META]` | Retrieve a key's value. Optionally request a specific version or include metadata. |
| `DELETE ns key` | Soft-delete a key (writes a tombstone). Returns the tombstone version. |
| `LIST ns [PREFIX p] [CURSOR c] [LIMIT n]` | List active keys in a namespace. Paginated via cursor. |
| `VERSIONS ns key [LIMIT n] [FROM version]` | List version history for a key (most recent first). |

### Namespace Commands

| Command | Description |
|---------|-------------|
| `NAMESPACE CREATE name [SCHEMA json] [MAX_VERSIONS n] [TOMBSTONE_RETENTION n]` | Create a namespace. Optional JSON schema for metadata validation. |
| `NAMESPACE DROP name [FORCE]` | Drop a namespace. Requires FORCE if the namespace contains keys. |
| `NAMESPACE LIST [CURSOR c] [LIMIT n]` | List namespaces visible to the current token. |
| `NAMESPACE INFO name` | Get namespace metadata (key count, creation time). |
| `NAMESPACE ALTER name [SCHEMA json] [MAX_VERSIONS n] [TOMBSTONE_RETENTION n]` | Update namespace configuration. Schema changes apply on next write. |
| `NAMESPACE VALIDATE name` | Check existing entries against the current metadata schema. |

### Batch and Streaming

| Command | Description |
|---------|-------------|
| `PIPELINE [REQUEST_ID id] <cmd1> <cmd2> ...` | Atomic batch (nested command arrays). All succeed or all roll back. Idempotent with REQUEST_ID. |
| `SUBSCRIBE ns [KEY key] [EVENTS PUT\|DELETE]` | Subscribe to change events on a namespace. Optionally filter by key or event type. |
| `UNSUBSCRIBE` | End the current subscription. |

### Connection and Operational

| Command | Description |
|---------|-------------|
| `AUTH token` | Authenticate the connection. |
| `PING` | Test connectivity. |
| `HEALTH` | Check server health. |
| `CONFIG GET key` | Read a runtime configuration value. |
| `CONFIG SET key value` | Set a runtime configuration value (admin only). Only registered keys are accepted; values are type-checked. |
| `REKEY new_key_hex` | Begin online zero-downtime master key rotation (admin only). Background re-encrypts WAL segments and takes a fresh snapshot. |
| `REKEY STATUS` | Query progress of an in-flight rekey operation. Returns progress percentage, segments completed, and whether rekey is still running. |
| `COMMAND LIST` | List all supported commands. |

See [`protocol.toml`](protocol.toml) for the machine-readable protocol specification.

---

## CLI Subcommands

```sh
# Health check without starting the server
shroudb doctor --config config.toml

# Offline re-key (rotate master encryption key -- server must be stopped)
shroudb rekey --old-key <old> --new-key <new> --config config.toml

# Export a namespace to an encrypted bundle
shroudb export --namespace my-ns --output backup.kvex --config config.toml

# Import from a bundle (same master key required)
shroudb import --input backup.kvex [--namespace new-name] --config config.toml
```

---

## Docker Deployment

### Ports

| Port | Purpose |
|------|---------|
| `6399` | ShrouDB protocol |

### Volume

Mount a volume at `/data` for durable storage. Without a volume, data is lost when the container stops.

### Environment

| Variable | Required | Description |
|----------|----------|-------------|
| `SHROUDB_MASTER_KEY` | Yes (production) | 64 hex characters. Encrypts all data at rest. |
| `SHROUDB_MASTER_KEY_FILE` | Alternative | Path to a file containing the master key. |
| `SHROUDB_LOG_LEVEL` | No | Log level (`info`, `debug`, `warn`). Default: `info`. |

Without a master key the server starts in dev mode with an ephemeral key -- data will not survive restarts.

### Docker Compose

```yaml
services:
  shroudb:
    image: shroudb/shroudb
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

---

## Telemetry

ShrouDB provides four telemetry channels:

- **Console** -- Structured JSON logs to stdout. Configurable via `SHROUDB_LOG_LEVEL`.
- **Audit log** -- All data operations are written to `{data_dir}/audit.log` for compliance and forensic review.
- **OpenTelemetry** -- OTLP export of traces and metrics to any OpenTelemetry-compatible backend. Configure with `otel_endpoint` in the server config.
- **Prometheus** -- Metrics exposed via HTTP endpoint. Configure with `metrics_bind` in the server config.

All telemetry is handled by the shared `shroudb-telemetry` library.
