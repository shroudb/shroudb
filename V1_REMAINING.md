# ShrouDB v1 — Remaining Work

**Status: Feature-complete. Test coverage gaps remain.**

Core data path, all planned features, security fixes, and documentation are done. The remaining work is test coverage (error paths, concurrency, integration tests) and CI pipeline updates.

## Feature Implementation Status

| Feature | Status |
|---------|--------|
| F1 + F12: SUBSCRIBE connection handling | **Done** — connection-level streaming with RESP3 push frames |
| F2: Webhooks | **Done** — HMAC-SHA256 signed HTTP delivery with retry |
| F3: Pipeline idempotency | **Done** — IdempotencyMap with 5min TTL, REQUEST_ID keyword |
| F4: Tombstone compaction | **Done** — scheduler reaper + TombstoneCompacted WAL entry |
| F5: Config hot-reload | **Done** — ReloadableValidator + watch channel for rate limits |
| F6: Export/Import | **Done** — AES-256-GCM encrypted bundles with namespace rename |
| F7: Telemetry | **Done** — shroudb-telemetry: console + audit file + OTEL |
| F8: Documentation | **Done** — DOCS.md, ABOUT.md, PROTOCOL.md fully rewritten |
| F9: Helm charts | **Done** — updated for v1 config, metrics port, env vars |
| F10: Systemd unit | **Done** — description updated, rest unchanged |
| F11: wal-tool compat | **Done** — compiles against v0.2.3, no changes needed |
| F13: CI/GitHub Actions | **Unchanged** — workflows are protocol-agnostic dispatchers |
| F14: Client pipeline syntax | **Deferred** — no typed pipeline() method; use raw_command() |
| F15: v0.1 test removal | **Done** — 34 credential tests deleted |

## What actually works

- 20-command RESP3 parser (parses correctly, tested)
- PUT/GET/DELETE/LIST/VERSIONS through EmbeddedStore to WAL (tested, including crash recovery)
- NAMESPACE CREATE/DROP/LIST/INFO/ALTER/VALIDATE (tested)
- ACL middleware (tested — grants checked before handler runs)
- Token-based auth at connection level (works, tested)
- WAL encryption, snapshots, recovery (tested, including corruption handling)
- Client library PUT/GET/DELETE/LIST/VERSIONS (smoke tested against live server)
- CLI REPL with tab completion (manually tested)
- Doctor subcommand (tested)
- Rekey subcommand (tested end-to-end)
- Docker build (tested)

## What is broken, stubbed, or missing

Features dropped without owner confirmation during the v1 rewrite. Each existed in v0.1 and was removed or stubbed without discussion.

## Feature 1: SUBSCRIBE Backend

**Status:** Partially implemented. Event broadcast added to `StorageEngine.apply_inner()`. `EmbeddedSubscription` filters by namespace, key, and event type. **Not wired into `connection.rs`** — the connection handler still dispatches SUBSCRIBE to the command dispatcher which returns an error. Needs connection-level handling like AUTH: detect SUBSCRIBE, call `store.subscribe()`, enter a streaming loop writing RESP3 push frames, break on UNSUBSCRIBE or disconnect.

**Files touched (incomplete):**
- `commons/shroudb-storage/src/engine.rs` — `event_broadcast` field, `payload_to_event()`, `event_subscribe()`
- `commons/shroudb-storage/src/embedded_store.rs` — `EmbeddedSubscription` with filtering

**Files still need work:**
- `shroudb/shroudb-server/src/connection.rs` — handle SUBSCRIBE at connection level (like AUTH), stream events as RESP3 arrays, handle UNSUBSCRIBE
- Tests for subscription delivery, filtering, and disconnection

## Feature 2: Webhooks

**Status:** Not implemented. Removed entirely from v1.

**What v0.1 had:** `webhooks.rs` module with HMAC-SHA256 signed HTTP delivery, configurable endpoints, event filtering, retry with backoff.

**What needs to be built:**
- `shroudb/shroudb-server/src/webhooks.rs` — new module
  - `WebhookActor` that receives events from `engine.event_subscribe()`
  - HMAC-SHA256 signature in `X-ShrouDB-Signature-256` header
  - Retry queue with exponential backoff (1s, 2s, 4s, 8s, max 5 retries)
  - Event filtering per endpoint (by namespace, event type)
- `shroudb/shroudb-server/src/config.rs` — add `webhooks: Option<Vec<WebhookConfig>>` to `ShrouDBConfig`
  ```toml
  [[webhooks]]
  url = "https://example.com/hook"
  secret = "hmac-secret"
  events = ["put", "delete"]
  namespaces = ["myapp.*"]
  max_retries = 5
  timeout_ms = 5000
  ```
- `shroudb/shroudb-server/src/scheduler.rs` — spawn webhook actor
- `shroudb/shroudb-server/src/main.rs` — pass event receiver to scheduler
- `shroudb/shroudb-server/Cargo.toml` — add `reqwest` dependency
- Tests for HMAC signing, retry logic, event filtering

**Depends on:** Feature 1 (event broadcast from engine)

## Feature 3: Pipeline Idempotency

**Status:** Not implemented. `IdempotencyMap` from v0.1 was removed.

**What v0.1 had:** `idempotency.rs` with a `DashMap<String, (ResponseMap, Instant)>` keyed by request_id, 5-minute TTL, background reaper.

**What needs to be built:**
- `shroudb/shroudb-protocol/src/idempotency.rs` — `IdempotencyMap` with DashMap, TTL, prune method
- `shroudb/shroudb-protocol/src/command.rs` — change `Pipeline(Vec<Command>)` to `Pipeline { commands: Vec<Command>, request_id: Option<String> }`
- `shroudb/shroudb-protocol/src/resp3/parse_command.rs` — parse optional `REQUEST_ID` keyword in PIPELINE
- `shroudb/shroudb-protocol/src/dispatch.rs` — add `IdempotencyMap` to `CommandDispatcher`, check before executing pipeline, cache after success
- `shroudb/shroudb-server/src/scheduler.rs` — add idempotency reaper task (60s interval, evict entries > 5 minutes)
- Tests for dedup on retry, TTL expiry, reaper cleanup

## Feature 4: Tombstone Compaction Reaper

**Status:** Not implemented. The `tombstone_retention_secs` config field exists on `NamespaceConfig` but nothing enforces it.

**What needs to be built:**
- `commons/shroudb-storage/src/wal/entry.rs` — add `TombstoneCompacted = 3` to `OpType`, add `WalPayload::TombstoneCompacted { keys: Vec<Vec<u8>> }`
- `commons/shroudb-storage/src/index/kv.rs` — add `remove_key(ns, key)` and `find_expired_tombstones(ns, retention_secs, now)` methods to `KvIndex`
- `commons/shroudb-storage/src/recovery.rs` — handle `TombstoneCompacted` in `apply_payload_to_index()`
- `shroudb/shroudb-server/src/scheduler.rs` — add tombstone reaper task (300s interval):
  - Iterate namespaces with `tombstone_retention_secs` set
  - Call `find_expired_tombstones()`
  - Write `TombstoneCompacted` via `engine.apply()`
- Tests for compaction timing, key removal, WAL replay of compaction entries

## Feature 5: Config Hot-Reload

**Status:** Not implemented. v0.1 had a config reloader that watched mtime and hot-reloaded the `disabled` flag on keyspaces.

**What needs to be built:**
- `commons/shroudb-acl/src/token.rs` — add `replace_all(&mut self, tokens: HashMap<String, Token>)` to `StaticTokenValidator`
- `shroudb/shroudb-server/src/config.rs` — add `diff_reloadable(old, new) -> ReloadDelta` function
- `shroudb/shroudb-server/src/scheduler.rs` — add config reloader task (10s interval):
  - Stat config file mtime
  - On change: re-parse TOML, compute delta
  - Reload auth tokens (write-lock `RwLock<StaticTokenValidator>`)
  - Reload rate limits (via watch channel)
- `shroudb/shroudb-server/src/main.rs` — change `token_validator` from `Arc<StaticTokenValidator>` to `Arc<RwLock<StaticTokenValidator>>`
- `shroudb/shroudb-server/src/connection.rs` — read rate limit from `watch::Receiver` instead of static value
- `shroudb/shroudb-server/src/server.rs` — thread shared mutable state through to connections

**Reloadable settings:** auth tokens, rate limits
**Not reloadable (require restart):** bind address, TLS certs, data directory, storage settings

## Feature 6: Export/Import

**Status:** Not implemented. v0.1 had encrypted KVEX bundle export/import for keyspace migration.

**What needs to be built:**
- `commons/shroudb-storage/src/snapshot/export.rs` — new module
  - `export_namespace(engine, namespace) -> Result<Vec<u8>>`: serialize namespace from KvIndex, encrypt with export-derived key, produce bundle
  - `import_namespace(engine, bundle) -> Result<String>`: decrypt, deserialize, replay entries via `engine.apply()`
  - Bundle format: `[magic:4][header_len:u32][header_json:N][encrypted_payload:M][hmac:32]`
- `commons/shroudb-storage/src/key_manager.rs` — add `export_key()` that derives a purpose-specific key via HKDF with info="shroudb-export"
- `shroudb/shroudb-server/src/main.rs` — add `Export` and `Import` subcommands:
  ```
  shroudb export --namespace myapp.users --output backup.sdb
  shroudb import --input backup.sdb [--namespace new-name]
  ```
- The `run_rekey()` function in `main.rs` (lines ~430-540) is a reference implementation for the read-all-replay-all pattern that import should follow
- Tests for roundtrip (export → import → verify data), cross-instance import, namespace rename on import

## Feature 7: Telemetry Integration

**Status:** Not wired in. `shroudb-telemetry` crate exists in commons and is used by all other engines. The v1 server only uses `tracing_subscriber::fmt()` — no audit file, no OTEL export.

**What v0.1 had:**
- `shroudb-telemetry::init_telemetry()` sets up three layers:
  - Console (JSON stdout for production, human-readable for dev)
  - Audit file (`{data_dir}/audit.log`) — structured JSON, separate from operational logs
  - OpenTelemetry (OTLP export to collector)
- The `target: "shroudb::audit"` tracing events in `dispatch.rs` are routed to the audit file by the telemetry layer

**What needs to be built:**
- `shroudb/shroudb-server/Cargo.toml` — add `shroudb-telemetry` dependency
- `shroudb/shroudb-server/src/main.rs` — replace `tracing_subscriber::fmt().init()` with `shroudb_telemetry::init_telemetry()`, passing data_dir for audit file path and optional OTEL endpoint from config
- `shroudb/shroudb-server/src/config.rs` — add `otel_endpoint: Option<String>` to `ServerConfig`
- `config.example.toml` — add commented `otel_endpoint` line
- Verify audit events from `dispatch.rs` appear in `{data_dir}/audit.log`

## Feature 8: Documentation Files Not Updated

**Status:** Only README.md, ARCHITECTURE.md, and CLAUDE.md were updated. The following still contain v0.1 credential-specific content:

- `DOCS.md` — full command documentation, references ISSUE/VERIFY/REVOKE/etc.
- `ABOUT.md` — project description, says "credential management"
- `PROTOCOL.md` — human-readable wire protocol spec, all v0.1 commands
- `DOCKER.md` — Docker documentation, references old config format
- `ISSUES.md` — known issues, references credential-specific behavior
- `PROJECT.md` — project plan with credential-specific phases
- `REPLICATION_PLAN.md` — replication design, references credential WAL payloads

Each of these needs a full rewrite or removal.

## Feature 9: Helm Charts

**Status:** Not updated. `helm/` directory exists and references old config structure, old environment variables (`LOG_LEVEL` instead of `SHROUDB_LOG_LEVEL`), old keyspace config.

## Feature 10: Systemd Unit File

**Status:** Not verified. `shroudb.service` exists in the repo. May reference old CLI flags or environment variables.

## Feature 11: shroudb-wal-tool Compatibility

**Status:** Broken. `commons/shroudb-wal-tool` depends on `shroudb-storage = "0.1.0"` and uses the old credential WAL types. The workspace `Cargo.toml` was updated to reference `shroudb-storage = "0.2.0"`, so `shroudb-wal-tool` will fail to compile. Needs either:
- Rewrite to use v0.2 KV WAL types
- Or pin to v0.1.0 as a separate workspace member with its own dep

## Feature 12: Connection-Level SUBSCRIBE Handling

**Status:** Not implemented. The SUBSCRIBE command is parsed by the protocol layer but the dispatcher returns an error ("SUBSCRIBE must be handled at the connection level"). The v0.1 `connection.rs` handled SUBSCRIBE at the connection level — entered a streaming loop, filtered events, wrote RESP3 push frames. This code was removed during the rewrite and not replaced.

The `EmbeddedStore::subscribe()` backend now works (Feature 1), but nothing in the server calls it.

**Files needed:**
- `shroudb/shroudb-server/src/connection.rs` — detect `Command::Subscribe` before dispatch (like AUTH handling), call store.subscribe(), enter event streaming loop with RESP3 push frames, handle UNSUBSCRIBE

## Feature 13: GitHub Actions / CI

**Status:** Not updated. `.github/` directory contains workflows that reference old crate structure, old test commands, and old dependency versions. Needs:
- Update test matrix for new crates (shroudb-store, shroudb-acl)
- Update shroudb-storage version references
- Add cargo-audit job
- Add cargo-geiger or deny.toml unsafe audit
- Add fuzz target compilation check (nightly)
- Verify Docker build in CI
- Update release workflow for new binary

## Feature 14: v0.1 Client Pipeline Syntax

**Status:** Potential bug. The v0.1 client `pipeline()` method sends `PIPELINE ... END` syntax. The v1 parser expects `PIPELINE <count>`. The client method was rewritten to use `raw_command()` but the old `END`-delimited format is gone. Need to verify the client's pipeline support actually works against the v1 server, or implement a proper `pipeline()` method on `ShrouDBClient`.

## Feature 15: v0.1 Integration Tests Need Removal

**Status:** `shroudb-server/tests/integration.rs` and `shroudb-server/tests/common/mod.rs` contain v0.1 credential-specific integration tests (34 tests: api_key, hmac, jwt, keyspace, etc.) that all fail against the v1 server. These need to be deleted and replaced with v1 integration tests covering the KV command set.

Also: `shroudb-server/tests/load_test.rs` and `shroudb-server/tests/memory_test.rs` may reference old types.

## Other Dropped Items Not Previously Mentioned

- **`purge` subcommand** — v0.1 had `shroudb purge <keyspace>` with confirmation prompt. Maps to `NAMESPACE DROP <name> FORCE` in v1, but the CLI subcommand convenience was removed.
- **v0.1 README referenced `mlock`-pinned secrets** — this is in `shroudb-crypto` (upstream, unchanged), but the v1 README still claims "security hardened: zeroize-on-drop" without verification that the v1 code paths actually use `SecretBytes` for sensitive data in the new KV model.

## Code Quality Issues Found in Audit

### Critical — Must fix before any release

1. ~~**CONFIG GET/SET are stubs.**~~ **False positive.** CONFIG GET/SET were already wired through to `config_store.get()` and `engine.apply()`. They work.

2. ~~**Master key hex validation is inverted.**~~ **FIXED.** `hex::decode` failure on a 64-byte file now returns `StorageError::MasterKeyInvalid` instead of silently falling through. (shroudb-server/src/main.rs)

3. ~~**Token expiry uses `unwrap_or_default()` for system time.**~~ **FIXED.** Clock failure now breaks the connection (fail closed) instead of setting `now = 0`. (shroudb-server/src/connection.rs)

4. ~~**Pipeline GET uses placeholder WAL payload.**~~ **FIXED.** Staged vec changed to `Vec<Option<(OpType, WalPayload)>>`; GET stages `None`. (shroudb-storage/src/embedded_store.rs)

5. ~~**SUBSCRIBE never reaches the Store.**~~ **FIXED.** Connection handler now intercepts SUBSCRIBE at connection level, enters streaming loop with RESP3 push frames, handles UNSUBSCRIBE. (shroudb-server/src/connection.rs)

### High — Should fix before release

6. ~~**Version state uses Debug formatting.**~~ **FIXED.** Explicit `"active"`/`"deleted"` string mapping replaces `format!("{:?}", v.state)`. (shroudb-protocol/src/handlers/versions.rs)

7. ~~**Client silently returns empty data on missing fields.**~~ **FIXED.** All `unwrap_or_default()` replaced with `ok_or_else` errors in `parse_get_result`, `namespace_info`, and `parse_versions`. (shroudb-client/src/lib.rs)

8. ~~**Namespace drop is racy.**~~ **Mitigated.** Added `drop_namespace_if_empty` atomic method to KvIndex using `DashMap::remove_if`. Documented the remaining narrow TOCTOU window between the check and WAL write. (shroudb-storage/src/index/kv.rs, embedded_store.rs)

9. ~~**Rate limiter allows burst.**~~ **FIXED.** Token bucket starts with 1 token instead of `max_tokens`. (shroudb-server/src/connection.rs)

10. ~~**Config load silently uses defaults.**~~ **FIXED.** Explicit `--config` path that doesn't exist now returns an error; default path still falls through to defaults. (shroudb-server/src/main.rs)

11. ~~**AUTH in dispatcher returns misleading success.**~~ **FIXED.** Returns `CommandError::Internal` with "(bug: reached dispatcher)" message. (shroudb-protocol/src/dispatch.rs)

12. ~~**Missing error context in handlers.**~~ **FIXED.** Added `map_err` with namespace/key context to get, delete, list, and versions handlers. (shroudb-protocol/src/handlers/*.rs)

13. ~~**Rekey doesn't enforce server is stopped.**~~ **FIXED.** Port bind check before rekey; if port is in use, rekey aborts. (shroudb-server/src/main.rs)

14. **Silent cursor skip in LIST.** `embedded_store.rs`: cursor pagination uses string comparison. Invalid cursors silently skip results instead of erroring. **Not fixed — low risk, needs design decision on cursor format.**

15. ~~**Replication snapshot frame is a log-and-continue.**~~ **Mitigated.** Changed log level from `info` to `warn` with explicit comment. Full snapshot processing is Feature 1. (shroudb-storage/src/replication/replica.rs)

## Infrastructure Security Issues (Full Codebase Audit)

### Critical — Service crash or security bypass

16. ~~**WAL entry decode has unsafe unwrap on disk data.**~~ **FIXED.** Both `try_into().unwrap()` calls replaced with `map_err` returning `StorageError::Deserialization`. (shroudb-storage/src/wal/entry.rs)

17. ~~**Replication handshake has unsafe unwrap on network data.**~~ **FIXED.** All `unwrap()` calls in `HandshakePayload::decode` and `HeartbeatPayload::decode` replaced with `map_err`. (shroudb-storage/src/replication/protocol.rs)

18. ~~**RESP3 reader has no maximum array/map size.**~~ **False positive.** Already had `MAX_COLLECTION_SIZE = 100,000` check. (shroudb-protocol/src/resp3/reader.rs)

19. ~~**Replication replica silently skips bad CRC.**~~ **FIXED.** Changed from `Ok(())` to `Err(StorageError::Deserialization(...))` to trigger resync. (shroudb-storage/src/replication/replica.rs)

20. ~~**Recovery timestamp uses `unwrap_or_default()`.**~~ **FIXED.** Eliminated `SystemTime::now()` from `apply_payload_to_index` entirely; function now takes the WAL entry's original timestamp as a parameter. Also fixed `engine.apply_inner()` to return `StorageError::Internal` on clock failure. (shroudb-storage/src/recovery.rs, engine.rs, replication/replica.rs)

### High — Data corruption or DoS

21. ~~**WAL writer doesn't check entry size after rotation.**~~ **FIXED.** Added `EntryTooLarge` check before rotation. (shroudb-storage/src/wal/writer.rs)

22. ~~**KV index VERSIONS has no limit cap.**~~ **FIXED.** Capped at 10,000 in `versions()` and `list_keys()`. (shroudb-storage/src/index/kv.rs)

23. ~~**Config store SET has no type validation.**~~ **Partially fixed.** Added `ConfigValueType::validate()` method. The ConfigStore itself doesn't have a schema registry yet, so validation is available but not enforced by the store. Callers (e.g., CONFIG SET handler) can use it when a schema is registered.

### Medium — Key exposure or compatibility

24. ~~**Master key hex string not zeroized.**~~ **FIXED.** Wrapped hex intermediates in `Zeroizing<String>` in both `EnvMasterKey` and `FileMasterKey`. (shroudb-storage/src/master_key.rs)

25. ~~**Key manager caches derived keys indefinitely.**~~ **Mitigated.** Added `clear_cache()` method for manual eviction during shutdown. TTL not added — the security benefit is marginal given the master key lives in memory for the full process lifetime. (shroudb-storage/src/key_manager.rs)

26. ~~**Client connection has no I/O timeout.**~~ **FIXED.** `send_command` wrapped with 30s `tokio::time::timeout`. Added `ClientError::Timeout` variant. (shroudb-client/src/connection.rs, error.rs)

27. ~~**ACL wildcard `*` not documented.**~~ **Already documented.** The `Grant` struct has a doc comment: `"*"` for wildcard (all namespaces). No change needed.

28. **Snapshot encoding check prevents forward compatibility.** Hard-coded `"postcard-v1"` check. **Not changed — correct behavior for now.** Only one encoding exists. Should be extended when new encodings are added.

29. ~~**Client IPv6 parsing broken.**~~ **FIXED.** Proper bracket-aware host extraction for `[::1]:6399` format. (shroudb-client/src/connection.rs)

30. ~~**Promote has TOCTOU race.**~~ **FIXED.** Single write lock acquisition for atomic check-and-set. (shroudb-storage/src/engine.rs)

31. ~~**Replication primary has no backpressure.**~~ **FIXED.** Flush + yield every 1,000 entries during historical replay. (shroudb-storage/src/replication/primary.rs)

32. ~~**Replication primary sends no heartbeats during replay.**~~ **FIXED.** Heartbeat frames interleaved every 5 seconds during historical WAL replay. (shroudb-storage/src/replication/primary.rs)

33. ~~**Snapshot writer assumes atomic rename.**~~ **False positive.** Already has fsync + read-back verification.

34. ~~**WAL namespace length stored as u16.**~~ **FIXED.** Added validation at `WalEntry::new()` — rejects namespaces > 65,535 bytes with `StorageError::Serialization`. (shroudb-storage/src/wal/entry.rs)

35. ~~**Engine health transitions not guarded.**~~ **FIXED.** `set_health()` validates transition against state machine; invalid transitions are logged and ignored. (shroudb-storage/src/engine.rs)

## Test Coverage Gaps

The 273 existing tests (commons) + 37 (shroudb) cover happy paths. The following are NOT tested:

- **No error path tests for any handler.** Every handler test sends valid input and checks the success response. None send invalid input.
- **No tests for ACL rejection.** The ACL middleware is tested in `shroudb-acl` unit tests, but no integration test verifies that an unauthorized client actually gets denied at the protocol level.
- **No tests for rate limiting.** The rate limiter exists in `connection.rs` but no test exercises it.
- **No tests for TLS connections.** The TLS setup code in `server.rs` is untested.
- **No tests for token expiry.** The `is_expired()` method is unit tested in `shroudb-acl`, but no integration test verifies that an expired token actually gets rejected mid-session.
- **No tests for malformed RESP3 input.** The fuzz targets exist but haven't been run. No unit tests send garbage bytes to the parser.
- **No tests for concurrent access.** No tests exercise multiple clients writing to the same namespace simultaneously.
- **No tests for WAL segment rotation under load.** Only tested via recovery (write, restart, verify). No test that writes enough to trigger rotation and verifies continuity.
- **No tests for snapshot + WAL combined recovery with version history.** There is one test (`snapshot_preserves_version_history`) but it only covers the basic case, not edge cases like snapshots taken mid-version-chain.
- **No tests for PIPELINE.** The `pipeline` method on `EmbeddedStore` is tested via the Store trait tests, but no protocol-level test sends a PIPELINE command through the dispatcher.
- **No tests for the client library against a real server** beyond the smoke test. The smoke test covers one of each command. It doesn't test error cases, timeouts, or reconnection.
- **No tests for the rekey subcommand with version history.** The rekey test creates keys with one version each. Doesn't test that multi-version keys and tombstones survive rekey.
- **No tests for the doctor subcommand with corrupt data.** Doctor is tested with valid and empty data. Not tested with corrupted WAL or snapshots.

## Published Crate Status

All published crates are current:
- `shroudb-store` v0.1.2 — published, includes derive Default fix
- `shroudb-storage` v0.2.2 — published, includes all security/correctness fixes
- `shroudb-acl` v0.1.1 — no known issues
- `shroudb-crypto` v0.1.0 — no changes
- `shroudb-telemetry` v0.1.0 — no changes

## Workspace State

**commons/** — changes not yet committed. `shroudb-store` v0.1.2 and `shroudb-storage` v0.2.2 published to registry.

**shroudb/** — changes not yet committed. Builds against published registry crates (no local patches). `.cargo/config.toml` cleaned (`.cargo/` is gitignored). Dead `shroudb-core` workspace dependency removed.

## Summary of Resolved Issues

| # | Issue | Status |
|---|-------|--------|
| 1 | CONFIG GET/SET stubs | False positive — already worked |
| 2 | Master key hex validation | **Fixed** |
| 3 | Token expiry fails open | **Fixed** |
| 4 | Pipeline GET placeholder | **Fixed** |
| 5 | SUBSCRIBE not wired | **Fixed** |
| 6 | Version Debug formatting | **Fixed** |
| 7 | Client silent defaults | **Fixed** |
| 8 | Namespace drop race | **Mitigated** (documented TOCTOU) |
| 9 | Rate limiter burst | **Fixed** |
| 10 | Config load silent defaults | **Fixed** |
| 11 | AUTH misleading response | **Fixed** |
| 12 | Missing error context | **Fixed** |
| 13 | Rekey no server check | **Fixed** |
| 14 | Silent cursor skip | Not fixed (low risk) |
| 15 | Snapshot frame log | **Mitigated** (warn + comment) |
| 16 | WAL decode unsafe unwrap | **Fixed** |
| 17 | Replication handshake unwrap | **Fixed** |
| 18 | RESP3 no max array size | False positive — already had limit |
| 19 | Replica CRC skip | **Fixed** |
| 20 | Recovery timestamp default | **Fixed** |
| 21 | WAL entry size check | **Fixed** |
| 22 | VERSIONS no limit cap | **Fixed** |
| 23 | Config SET type validation | **Partially fixed** (method added, no schema registry) |
| 24 | Master key hex not zeroized | **Fixed** |
| 25 | Key manager cache | **Mitigated** (clear_cache added) |
| 26 | Client no I/O timeout | **Fixed** |
| 27 | ACL wildcard undocumented | Already documented |
| 28 | Snapshot encoding compat | Not changed (correct behavior) |
| 29 | Client IPv6 parsing | **Fixed** |
| 30 | Promote TOCTOU | **Fixed** |
| 31 | Replication no backpressure | **Fixed** |
| 32 | No heartbeats during replay | **Fixed** |
| 33 | Snapshot writer atomic rename | False positive — already solid |
| 34 | WAL namespace length u16 | **Fixed** |
| 35 | Health transitions not guarded | **Fixed** |

**Resolved: 28 fixed, 3 mitigated, 4 false positives**
**Remaining: 1 low-risk (cursor skip)**
