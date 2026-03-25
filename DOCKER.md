# ShrouDB

Encrypted credential vault with key rotation, RESP3 protocol, and WAL storage.

## Quick Start

```sh
# Dev mode (ephemeral master key, data lost on restart)
docker run -p 6399:6399 -p 9090:9090 shroudb/shroudb

# Production (persistent storage + master key)
docker run -d \
  -p 6399:6399 -p 9090:9090 \
  -v shroudb-data:/data \
  -e SHROUDB_MASTER_KEY="$(openssl rand -hex 32)" \
  shroudb/shroudb
```

## Ports

| Port | Purpose |
|------|---------|
| `6399` | RESP3 command protocol |
| `9090` | Prometheus metrics (`/metrics`) |

## Volumes

| Path | Purpose |
|------|---------|
| `/data` | WAL segments, snapshots, and audit logs |

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `SHROUDB_MASTER_KEY` | Yes (production) | 64 hex characters. Encrypts all data at rest. |
| `SHROUDB_MASTER_KEY_FILE` | Alternative | Path to a file containing the master key. |
| `LOG_LEVEL` | No | `info`, `debug`, `warn`. Default: `info`. |

Without a master key the server starts in dev mode — data will not survive restarts.

## Configuration

Mount a config file and pass `--config`:

```sh
docker run -d \
  -p 6399:6399 -p 9090:9090 \
  -v shroudb-data:/data \
  -v ./config.toml:/config.toml:ro \
  -e SHROUDB_MASTER_KEY="$(openssl rand -hex 32)" \
  shroudb/shroudb --config /config.toml
```

## Docker Compose

```yaml
services:
  shroudb:
    image: shroudb/shroudb
    ports:
      - "6399:6399"
      - "9090:9090"
    environment:
      - SHROUDB_MASTER_KEY=${SHROUDB_MASTER_KEY}
    volumes:
      - shroudb-data:/data
    restart: unless-stopped

volumes:
  shroudb-data:
```

## CLI

A command-line client is available as a separate image:

```sh
docker run --rm -it shroudb/cli --uri shroudb://host.docker.internal:6399
```

## Image Details

- **Base image:** Alpine 3.21
- **User:** `shroudb` (UID 65532)
- **Architectures:** `linux/amd64`, `linux/arm64`
- **License:** MIT OR Apache-2.0

## Links

- [GitHub](https://github.com/shroudb/shroudb)
- [Documentation](https://github.com/shroudb/shroudb/blob/main/README.md)
- [Homebrew](https://github.com/shroudb/homebrew-tap) — `brew install shroudb/tap/shroudb`
