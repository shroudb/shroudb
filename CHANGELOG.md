# Changelog

All notable changes to ShrouDB are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/).

## [v1.0.11] - 2026-04-11

### Added

- `REKEY <new_key_hex>` command for online zero-downtime master key rotation (HIGH-10)
- `REKEY STATUS` command to query rekey progress
- Protocol spec updated with REKEY and REKEY STATUS definitions

### Changed

- Upgraded `shroudb-storage` to v0.3.0 for dual-key KeyManager and background re-encryption
- Offline `rekey` subcommand now notes availability of online `REKEY` command

## [v1.0.10] - 2026-04-11

### Changed

- Helm chart upgraded to v1.0.0 for production readiness (LOW-05)
- Deployment replaced with StatefulSet for stable pod identity and ordered rollout
- Added PodDisruptionBudget (enabled when replicas > 1)
- Added ServiceMonitor for Prometheus Operator integration
- Added NetworkPolicy for ingress restriction
- Added headless Service for StatefulSet DNS
- Added ServiceAccount with automountServiceAccountToken disabled
- Added pod and container security contexts (non-root, read-only rootfs, drop all capabilities, seccomp)
- Added pod anti-affinity (soft by default, configurable to hard)
- Added startup probe for slow-starting pods
- Added config checksum annotation for automatic rollout on config change
- Added NOTES.txt post-install instructions
- Added helm test (TCP connection check)
- Chart appVersion updated to 1.0.9

## [v1.0.9] - 2026-04-11

### Changed

- Update shroudb-storage dependency to v0.2.18 (snapshot replication bootstrap, MED-05)

## [v1.0.8] - 2026-04-11

### Added

- RemoteStore streaming subscriptions via dedicated TCP connections (LOW-02)
- `Connection::read_response_streaming()` for timeout-free push frame reading
- 6 unit tests for push frame parsing, 3 integration tests for subscription lifecycle

### Changed

- `RemoteStore::new()` now requires URI for spawning subscription connections
- `RemoteStore::subscribe()` opens a dedicated connection, sends SUBSCRIBE, and relays events through an mpsc channel

## [v1.0.4] - 2026-04-09

### Added

- wire fuzz targets into CI with corpus-building (LOW-04)

### Fixed

- set registry token env vars globally in fuzz workflow

## [v1.0.3] - 2026-04-09

### Added

- CONFIG SET schema enforcement (LOW-01) and LIST cursor validation (LOW-03)

### Fixed

- use shared master key sources, eliminate duplicated EnvMasterKey

## [v1.0.2] - 2026-04-04

### Changed

- use shared ServerAuthConfig from shroudb-acl

## [v1.0.1] - 2026-04-02

### Fixed

- use entrypoint script to fix volume mount permissions

### Other

- Add AGENTS.md
- Harden server: expect context on unwraps (v1.0.1)
- Use portable disable_core_dumps() instead of Linux-only libc::prctl
- Harden unwraps: recover from poisoned locks, bail on malformed imports
- Document cache config, bump shroudb-storage to v0.2.10
- Wire vlog lifecycle, add restart test, bump shroudb-storage to v0.2.9
- Add cache integration tests, bump shroudb-storage to v0.2.8
- Add periodic cache metrics to scheduler
- Add cache config, bump shroudb-storage to v0.2.7

## [v1.0.0] - 2026-03-28

### Other

- Fix pre-commit hook: run ALL workspace tests, not just unit tests
- Fix smoke test: version state is lowercase (deleted/active not Deleted/Active)
- Add pre-commit hook matching CI checks
- Regenerate Cargo.lock from clean registry (no local patches)
- Restructure README — eliminate repetition, Quick Start before preamble
- Final README refinements + CLAUDE.md identity rule
- Sharpen README positioning per review feedback
- Rewrite README — lead with outcome, not implementation
- Add RemoteStore — Store trait over TCP/TLS for engine remote mode
- Update V1_REMAINING status: proven with 34 integration tests
- Add security hardening tests, fix recovery mode bug (34 total)
- Add TLS, rekey, doctor, webhook, and hot-reload integration tests (29 total)
- Add integration test suite — 22 tests against real server
- Split v1 test plan from v2 distribution plans
- Add V2_PLANS.md — honest audit of v1 state + roadmap
- Fix all remaining open items from V1_REMAINING audit
- Rewrite PIPELINE as single self-contained RESP3 frame
- Rewrite documentation and update infrastructure for v1
- Add config hot-reload and export/import subcommands
- Add telemetry, tombstone compaction, and pipeline idempotency
- Add SUBSCRIBE connection handling and webhook delivery
- v1 KV server — 20-command protocol, security fixes, v0.1 test cleanup

## [v0.1.1] - 2026-03-27

### Other

- v0.1.1 — add PING and COMMAND LIST

## [v0.1.0] - 2026-03-27

### Other

- Remove local patch overrides, regenerate Cargo.lock
- Regenerate Cargo.lock from clean Cargo.toml
- Clean v0.1.0 release — all deps on private registry
- Fix: shroudb-cli must use workspace dep for shroudb-client (needed for publish)
- Fix Dockerfile: use secret mount instead of ARG for registry token; add provenance
- Fix Dockerfile: configure shroudb registry for cargo build
- Fix release.yml: correct package name shroudb (not shroudb-server)
- Remove tag trigger from CI — release workflow handles tags
- Fix formatting
- Release v0.1.0 — migrate to shroudb private registry
- Standardize release workflow using reusable rust-release.yml
- Add security posture requirements to CLAUDE.md
- Split CI and release workflows, switch to self-hosted runners
- Update README: add CONFIG commands, migrate telemetry references
- Upgrade CONFIG from read-only stub to functional runtime config
- Merge metadata as top-level JWT claims in ISSUE handler
- Support UPDATE and schema validation for password credentials
- Replace third-party Docker Hub README action with direct API call
- WORKDIR /data, .dockerignore, Docker Hub overview
- Switch to alpine:3.21 base for Docker Scout A grade
- Switch to scratch base, OCI labels, use cross for aarch64 builds
- Use more.musl.cc mirror for aarch64 cross toolchain
- Fix aarch64 musl cross-compilation, add GHCR to releases, add attestations
- Reset to v0.1.0: multi-arch Docker, Homebrew tap, Docker Hub releases
- Fix JWT RFC 7519/7517 compliance
- Fix HEALTH state casing, add JWT revocation, add round-trip tests
- Fix server response fields to match protocol.toml spec
- Log server/spec response field mismatches found by codegen sandbox
- Fix clippy needless_return and fuzz dep on commons
- Use LOG_LEVEL instead of RUST_LOG for log configuration
- Update Docker section with image, ports, volume, config, and compose
- Remove k6 REST tests and orphaned docs
- Remove REST/HTTP references — REST is handled by shroudb-rest
- client: achieve full parity with shroudb-protocol command set
- docs: update README and config to match actual implementation
- cli: fix JSON panic, improve shell parser, update help text
- client: fix panicking unwraps, add error variants, improve response validation
- server: wire webhooks, expose metrics, fix rotation error handling
- protocol: fix stubs, stale docs, and defensive error handling
- Update README: streamline structure, add endpoint tables and command summary
- Remove shroudb-codegen crate (moved to shroudb/shroudb-codegen)
- Remove accidentally committed codegen target dir
- Fix Docker builds: use musl-cross image, exclude codegen
- Add .dockerignore to exclude .cargo/ from builds
- Fix Docker builds for private git deps
- Add git auth for private cross-repo deps in CI
- Run cargo fmt after rename
- Rename Keyva to ShrouDB
- Parameterize codegen on protocol name — no more hardcoded "keyva"
- Gitignore .cargo/ (local dev overrides only)
- Switch to git deps, remove cross-repo checkout from CI/Docker
- Fix formatting
- Remove embedded REST and auth — now separate repos
- Bundle server + cli in release tarball (like redis + redis-cli)
- Separate Docker images per binary
- Fix release: drop aarch64-linux-musl (cross incompatible with symlinked deps)
- Fix Docker/release builds: use -p flag, ensure musl target
- Fix CI: add commons checkout to all jobs, support private repos
- Fix CI: commons checkout symlink, cargo-deny with path deps
- Initial keyva: credential management server + auth server

