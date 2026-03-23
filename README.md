# Keyva

A credential management server built in Rust. Keyva manages JWT signing keys, API keys, HMAC secrets, and refresh tokens with encrypted-at-rest storage, automatic key rotation, and a RESP3 wire protocol.

## Quickstart

```bash
# 1. Build
cargo build --release

# 2. Run (dev mode — ephemeral master key, human-readable logs)
./target/release/keyva

# 3. Connect with the CLI
cargo run --bin keyva-cli
```

The server listens on `0.0.0.0:6399` (RESP3) and `0.0.0.0:8080` (REST) by default.

## Connection String

Keyva uses a URI scheme for connection strings:

```
keyva://[token@]host[:port][/keyspace]
keyva+tls://[token@]host[:port][/keyspace]
```

| Scheme        | Transport |
|---------------|-----------|
| `keyva://`    | Plain TCP |
| `keyva+tls://`| TLS       |

**Examples:**

```
keyva://localhost                        # plain TCP, default port 6399
keyva://localhost:6399                   # explicit port
keyva+tls://prod.example.com            # TLS, default port
keyva://mytoken@localhost:6399           # with auth token
keyva+tls://tok@host:6399/sessions      # TLS + auth + default keyspace
```

The CLI supports `--uri` as an alternative to `--host`/`--port`/`--tls`:

```bash
keyva-cli --uri keyva://localhost:6399
keyva-cli --uri keyva+tls://mytoken@prod.example.com/sessions
```

## Version

```bash
keyva --version
keyva-cli --version
```

## Health Check

Run `keyva doctor` to verify system health without starting the server:

```bash
keyva doctor --config config.toml
```

```
Config:     PASS  (config.toml parsed, 4 keyspaces defined)
Master Key: PASS  (loaded from KEYVA_MASTER_KEY)
Data Dir:   PASS  (./data exists, writable)
WAL:        PASS  (3 segments, 15234 entries, no corruption)
Snapshot:   PASS  (latest: snap_20240322_143000_abcd1234.bin, 4 keyspaces, 12345 credentials)
```

The command exits with code 0 if all checks pass, 1 if any check fails.

## Production Setup

### Master Key

Set a 32-byte hex-encoded master key before starting:

```bash
# Generate a key
openssl rand -hex 32

# Set via environment variable
export KEYVA_MASTER_KEY="<64-hex-chars>"

# Or via file
echo -n "<64-hex-chars>" > /etc/keyva/master.key
export KEYVA_MASTER_KEY_FILE="/etc/keyva/master.key"
```

### Configuration

Copy and edit the example config:

```bash
cp config.example.toml config.toml
./target/release/keyva --config config.toml
```

### Docker

```bash
docker build -t keyva .
docker run -p 6399:6399 -p 8080:8080 \
  -e KEYVA_MASTER_KEY="$(openssl rand -hex 32)" \
  -v keyva-data:/data \
  keyva
```

Or with Docker Compose:

```bash
docker compose up -d
```

### Docker Compose

See `docker-compose.yml` for a ready-to-use configuration with persistent volume.

### systemd

See `keyva.service` for a production systemd unit file. Install with:

```bash
sudo cp target/release/keyva /usr/local/bin/
sudo cp keyva.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now keyva
```

## Keyspace Types

| Type | Description |
|------|-------------|
| `jwt` | Asymmetric signing keys (ES256, ES384, RS256, RS384, RS512, EdDSA) with automatic rotation |
| `api_key` | Bearer tokens with SHA-256 hashed storage, optional prefix, and metadata |
| `hmac` | Symmetric HMAC keys (SHA-256/384/512) with rotation support |
| `refresh_token` | Rotating refresh tokens with family-based revocation and chain tracking |
| `password` | Argon2id/bcrypt/scrypt password hashing with rate limiting and lockout |

## Commands (RESP3 Protocol)

```
ISSUE <keyspace> [CLAIMS <json>] [META <json>] [TTL <secs>]
VERIFY <keyspace> <token> [PAYLOAD <data>] [CHECKREV]
REVOKE <keyspace> <id>
REFRESH <keyspace> <token>
UPDATE <keyspace> <credential_id> META <json>
INSPECT <keyspace> <credential_id>
ROTATE <keyspace> [FORCE] [NOWAIT] [DRYRUN]
JWKS <keyspace>
KEYSTATE <keyspace>
HEALTH [<keyspace>]
KEYS <keyspace> [CURSOR <c>] [PATTERN <glob>] [STATE <state>] [COUNT <n>]
SUSPEND <keyspace> <credential_id>
UNSUSPEND <keyspace> <credential_id>
SCHEMA <keyspace>
PASSWORD SET <keyspace> <user_id> <password> [META <json>]
PASSWORD VERIFY <keyspace> <user_id> <password>
PASSWORD CHANGE <keyspace> <user_id> <old_password> <new_password>
PASSWORD IMPORT <keyspace> <user_id> <hash> [META <json>]
```

## REST API

When `rest_bind` is configured, the REST API is available:

```
POST   /v1/{keyspace}/issue
POST   /v1/{keyspace}/verify
POST   /v1/{keyspace}/revoke
POST   /v1/{keyspace}/refresh
GET    /v1/{keyspace}/jwks
GET    /v1/{keyspace}/keys
GET    /v1/{keyspace}/{credential_id}
PUT    /v1/{keyspace}/{credential_id}
DELETE /v1/{keyspace}/{credential_id}
GET    /health
GET    /metrics
```

## Operational Commands

### Re-key (rotate master encryption key)

```bash
keyva rekey --old-key <old-hex-key> --new-key <new-hex-key> --config config.toml
```

This re-encrypts all WAL segments and snapshots. The server must be stopped first. After rekeying, update `KEYVA_MASTER_KEY` to the new key before restarting.

### Export / Import

```bash
# Export a keyspace to an encrypted bundle
keyva export my-keyspace --output backup.kvex --config config.toml

# Import into another instance (same master key required)
keyva import --file backup.kvex --keyspace my-keyspace --config config.toml
```

### Purge

```bash
keyva purge my-keyspace --config config.toml
```

## Architecture

- **Storage:** Write-ahead log (WAL) with periodic snapshots. All data is AES-256-GCM encrypted at rest with per-keyspace derived keys (HKDF-SHA256).
- **Protocol:** RESP3 wire protocol — chosen for its battle-tested framing, binary safety, and familiar ergonomics. Keyva speaks its own command set (ISSUE, VERIFY, REVOKE, etc.) over RESP3; it is not a Redis-compatible server and Redis clients will not work against it.
- **REST:** Axum-based HTTP API running on a separate port.
- **Security:** `mlock`-pinned secret memory, zeroize-on-drop, core dump disabled, constant-time comparisons.

## TLS

TLS is supported for the RESP3 protocol via `tls_cert` and `tls_key` in the server config. Mutual TLS (mTLS) is supported via `tls_client_ca`.

For the REST API, TLS termination should be handled by a reverse proxy (nginx, Caddy, or a cloud load balancer) in production. The REST server runs on a separate port and is typically behind a load balancer.

## License

MIT OR Apache-2.0
