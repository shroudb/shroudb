# ShrouDB v1 Release Checklist

Status: Implementation complete, not yet committed or tagged.

## Pre-commit

- [ ] Run fmt/clippy/test on commons workspace
  ```bash
  cd commons
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  cargo deny check
  ```
- [ ] Run fmt/clippy/test on shroudb workspace
  ```bash
  cd shroudb
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
- [ ] Verify zero TODOs/stubs/dead_code
  ```bash
  grep -r "TODO\|FIXME\|todo!\|unimplemented!\|dead_code" commons/shroudb-store/src commons/shroudb-acl/src commons/shroudb-storage/src shroudb/shroudb-server/src shroudb/shroudb-protocol/src shroudb/shroudb-client/src shroudb/shroudb-cli/src
  ```
- [ ] Verify no local path patches in any Cargo.toml that gets committed
- [ ] Remove `shroudb/Dockerfile.test` and `shroudb/.registry-token` if present
- [ ] Verify Docker build works: `docker build --target shroudb -t shroudb:v1 .`
- [ ] Remove `shroudb-wal-tool` Cargo.toml version pin (still references shroudb-storage 0.1.0)

## Commit — commons repo

### Changed crates

| Crate | Old Version | New Version | Change |
|-------|-------------|-------------|--------|
| `shroudb-store` | new | 0.1.0 | New crate: Store trait, Entry, Namespace, MetaSchema types |
| `shroudb-acl` | new | 0.1.1 | New crate: Token model, scopes, grants, AuthContext, TokenValidator |
| `shroudb-storage` | 0.1.0 | 0.2.0 | Breaking: KV data model replaces credential model |
| `shroudb-core` | 0.1.0 | unchanged | Still at 0.1.0, engines at v0.1.x pin to it |
| `shroudb-crypto` | 0.1.0 | unchanged | No changes |
| `shroudb-protocol-wire` | 0.1.0 | unchanged | No changes |
| `shroudb-telemetry` | 0.1.0 | unchanged | No changes |

### Files added
- `shroudb-store/` — entire crate (7 files)
- `shroudb-acl/` — entire crate (6 files)
- `shroudb-storage/tests/store_tests.rs` — EmbeddedStore integration tests
- `shroudb-storage/tests/store_invariants.rs` — property-based invariant tests

### Files rewritten
- `shroudb-storage/src/wal/entry.rs` — KV OpType + WalPayload (was credential types)
- `shroudb-storage/src/index/kv.rs` — KvIndex (was credential indexes)
- `shroudb-storage/src/recovery.rs` — KV recovery logic
- `shroudb-storage/src/engine.rs` — KV engine
- `shroudb-storage/src/snapshot/format.rs` — KV snapshot format
- `shroudb-storage/src/embedded_store.rs` — Store trait implementation
- `shroudb-storage/tests/recovery_tests.rs` — rewritten for KV model
- `shroudb-storage/tests/replication_tests.rs` — rewritten for KV model
- `shroudb-storage/benches/recovery_bench.rs` — rewritten for KV model

### Files removed
- `shroudb-storage/src/engine_handler.rs` — replaced by Store trait
- `shroudb-storage/src/index/api_key.rs` — credential-specific
- `shroudb-storage/src/index/password.rs` — credential-specific
- `shroudb-storage/src/index/refresh_token.rs` — credential-specific
- `shroudb-storage/src/index/revocation.rs` — credential-specific
- `shroudb-storage/src/index/signing_key.rs` — credential-specific

### Already published
- `shroudb-store` v0.1.0 ✓
- `shroudb-acl` v0.1.1 ✓
- `shroudb-storage` v0.2.0 ✓

## Commit — shroudb repo

### Files rewritten
- `shroudb-server/src/main.rs` — KV server with doctor + rekey subcommands, startup banner
- `shroudb-server/src/config.rs` — KV config model (no keyspace types)
- `shroudb-server/src/connection.rs` — AuthContext from shroudb-acl, TokenValidator
- `shroudb-server/src/server.rs` — generic over Store + TokenValidator
- `shroudb-server/src/scheduler.rs` — simplified (no credential reapers)
- `shroudb-protocol/src/command.rs` — 20-command enum with ACL requirements
- `shroudb-protocol/src/dispatch.rs` — generic dispatcher with ACL middleware
- `shroudb-protocol/src/error.rs` — KV error types
- `shroudb-protocol/src/acl.rs` — re-exports from shroudb-acl
- `shroudb-protocol/src/lib.rs` — updated exports
- `shroudb-protocol/src/resp3/parse_command.rs` — 20-command parser
- `shroudb-protocol/src/resp3/serialize.rs` — updated test
- `shroudb-protocol/src/handlers/*` — 9 new handler files (put, get, delete, list, versions, namespace, health, config, command_list)
- `shroudb-client/src/lib.rs` — KV API
- `shroudb-client/src/response.rs` — cleaned, credential types removed
- `shroudb-client/tests/smoke.rs` — end-to-end smoke test
- `shroudb-cli/src/main.rs` — updated commands and help text
- `config.example.toml` — KV config format
- `protocol.toml` — v1 command spec
- `README.md` — full rewrite
- `ARCHITECTURE.md` — full rewrite
- `CLAUDE.md` — updated dependencies
- `Dockerfile` — updated labels
- `docker-compose.yml` — SHROUDB_LOG_LEVEL
- `Cargo.toml` — workspace exclude fuzz, clap env feature, shroudb-acl/store deps

### Files added
- `shroudb-protocol/src/acl.rs`
- `shroudb-protocol/src/handlers/put.rs`
- `shroudb-protocol/src/handlers/get.rs`
- `shroudb-protocol/src/handlers/delete.rs`
- `shroudb-protocol/src/handlers/list.rs`
- `shroudb-protocol/src/handlers/versions.rs`
- `shroudb-protocol/src/handlers/namespace.rs`
- `shroudb-protocol/src/handlers/health.rs`
- `shroudb-protocol/src/handlers/config.rs`
- `shroudb-protocol/src/handlers/command_list.rs`
- `shroudb-client/tests/smoke.rs`
- `fuzz/fuzz_targets/command_parser.rs`
- `fuzz/fuzz_targets/acl_check.rs`
- `fuzz/fuzz_targets/meta_schema_validate.rs`
- `fuzz/rust-toolchain.toml`
- `V1_RELEASE.md` (this file — remove after release)

### Files removed
- `shroudb-protocol/src/auth.rs` — replaced by shroudb-acl
- `shroudb-protocol/src/events.rs` — not needed for v1
- `shroudb-protocol/src/idempotency.rs` — not needed for v1
- `shroudb-protocol/src/webhooks.rs` — not needed for v1
- `shroudb-protocol/src/handlers/*.rs` (all old credential handlers)
- `shroudb-client/src/builder.rs` — credential builders
- `shroudb-client/src/builders.rs` — credential builders
- `shroudb-client/tests/common/` — old integration test helpers
- `shroudb-client/tests/integration.rs` — old credential integration tests

## Tags

- commons: tag as `v0.2.0` (shroudb-storage breaking change)
- shroudb: tag as `v1.0.0`

## Registry publishing

Already published:
- `shroudb-store` v0.1.0 ✓
- `shroudb-acl` v0.1.1 ✓
- `shroudb-storage` v0.2.0 ✓

Not yet published (and may not need to be — these are server crates, not libraries consumed by other repos):
- `shroudb-protocol` — only if Moat or other products import it directly
- `shroudb-client` — yes, publish for engines using remote Store mode
- `shroudb-cli` — no, distributed as binary only

Decision needed: bump `shroudb-client` and `shroudb-protocol` to v1.0.0 to match the server?

## CI updates needed

### commons/.github/workflows/
- [ ] Add `shroudb-store` and `shroudb-acl` to the test matrix
- [ ] Update `shroudb-storage` version references
- [ ] Add `cargo-audit` job
- [ ] Add `cargo-geiger` job (or deny.toml unsafe audit)

### shroudb/.github/workflows/
- [ ] Update test job to use `shroudb-storage` v0.2.0 from registry (no patches)
- [ ] Add fuzz target build check (nightly, no run — just compile)
- [ ] Add `cargo-audit` job
- [ ] Verify Docker build in CI
- [ ] Update release workflow for new binary name/structure

## Post-release

- [ ] Update `secure-notes-app` if it references ShrouDB directly (it uses engines, not core ShrouDB)
- [ ] Update Moat to use Store trait when embedding ShrouDB
- [ ] Write threat model document (priority 4 from security testing plan)
- [ ] Set up fuzz CI with nightly (priority 1 — targets exist, need CI runner)
- [ ] TLS testing with testssl.sh (priority 5)
- [ ] Timing analysis on auth paths (priority 6)

## Test counts

| Location | Tests |
|----------|-------|
| `shroudb-store` (unit) | 6 |
| `shroudb-acl` (unit) | 18 |
| `shroudb-storage` (unit) | 51 |
| `shroudb-storage` (recovery integration) | 23 |
| `shroudb-storage` (replication integration) | 9 |
| `shroudb-storage` (Store trait integration) | 30 |
| `shroudb-storage` (property/invariant) | 9 (~2,300 cases) |
| `shroudb-protocol` (unit) | 31 |
| `shroudb-client` (unit + smoke) | 8 |
| **Total** | **185** |

Fuzz targets: 6 (compile verified, need nightly CI to run)
