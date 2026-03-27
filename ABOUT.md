# Understanding ShrouDB

---

## For Everyone: What ShrouDB Does

Every application that has users needs to manage sensitive information: passwords, login tokens, API keys. This information is critical — if it leaks or is mishandled, users get hacked, accounts get stolen, and companies face breaches.

Most applications store this sensitive data alongside everything else in a general-purpose database. That's like keeping your house keys, passport, and cash in the same junk drawer as your mail and receipts. It works, but it's not designed for security.

**ShrouDB is a dedicated credential storage engine.** It does one thing and does it well: securely store, issue, verify, and rotate the secrets that applications depend on. Everything inside ShrouDB is encrypted. If someone steals the storage files, they get meaningless noise — not passwords or keys.

**What it manages:**

- **Passwords** — Stores them using the strongest available algorithms and automatically upgrades older ones when users log in.
- **Login sessions** — Issues and tracks refresh tokens so users stay logged in securely, and detects if a token is stolen and reused.
- **API keys** — Generates and verifies bearer tokens for service-to-service communication.
- **Signing keys** — Manages the cryptographic keys used to create and verify JWTs (the tokens behind "Sign in with..." buttons).
- **HMAC keys** — Symmetric keys for verifying data integrity (e.g., webhook signatures).

**Why it matters:**

- Credentials are encrypted at rest — a disk breach yields nothing useful.
- Keys rotate automatically — no more "we forgot to rotate the signing key for three years."
- Stolen refresh tokens are detected and the entire session family is revoked.
- Passwords are automatically re-hashed to stronger algorithms over time — no migration scripts needed.

---

## For Technical Leaders: Architecture and Trade-offs

### The Problem

Credential management is typically scattered across application code, general-purpose databases, and ad-hoc scripts. This leads to inconsistent security posture, operational blind spots, and fragile key rotation procedures. Teams either build bespoke solutions (expensive, error-prone) or accept the risks of storing secrets in PostgreSQL columns.

### What ShrouDB Is

ShrouDB is a **dedicated credential storage engine** — closer in spirit to HashiCorp Vault's secrets engine than to a traditional database or auth library. It runs as a standalone server with a focused command set for credential lifecycle operations.

### Key Architectural Decisions

| Decision | Rationale |
|----------|-----------|
| **Standalone server, not a library** | Credentials live in a single hardened process with its own durability guarantees, not scattered across application instances. |
| **Custom storage engine** | No dependency on PostgreSQL or any external database. Reduces attack surface and operational complexity. |
| **Encrypted at rest by default** | AES-256-GCM with per-keyspace derived keys. Not optional — there is no unencrypted mode. |
| **Fail-closed design** | ShrouDB refuses service rather than serving potentially incorrect results. Corrupt data halts recovery rather than silently skipping entries. |

### Performance Profile

ShrouDB is optimized for **verify-heavy workloads** (the realistic pattern for credential systems):

- **44,000+ verifications/second** with 1.28ms median latency (embedded mode)
- **33,000+ verifications/second** in remote TCP mode
- Password operations are intentionally slow (~266ms for signup) — bounded by Argon2id, which is the point.

### Ecosystem

ShrouDB is the core engine. Around it:

- **shroudb-auth** — A REST API server that wraps ShrouDB for standard signup/login/refresh/logout flows.
- **shroudb-transit** — Encryption-as-a-service: encrypt, decrypt, sign, and verify data without exposing keys to application code.
- **shroudb-keep** — Encrypted secrets manager with versioning, path-based access control, and rotation.
- **shroudb-mint** — Lightweight internal Certificate Authority for issuing short-lived TLS certificates and mTLS bootstrapping.
- **shroudb-sentry** — Policy-based authorization engine that returns cryptographically signed decision tokens.
- **shroudb-veil** — Encrypted search engine that queries over end-to-end encrypted data without exposing plaintext.
- **shroudb-courier** — Secure notification delivery pipeline with encrypted recipients.
- **shroudb-pulse** — Centralized audit event ingestion and analytics across all ShrouDB engines.
- **shroudb-moat** — Unified single-binary hub that runs all engines with one config file and one set of auth tokens.
- **shroudb-codegen** — Generates typed client SDKs (Python, TypeScript, Go, Ruby) from the protocol specification.
- **shroudb-cli** — Interactive REPL and scripting tool for operations.

### Operational Model

- **Configuration:** TOML file with environment variable interpolation. Per-keyspace policies for rotation intervals, TTLs, hash parameters, and lockout rules.
- **Observability:** Structured tracing, audit log, OpenTelemetry export, webhook notifications with HMAC-signed delivery.
- **Deployment:** Single static binary. TLS and mTLS supported natively. No external dependencies at runtime.
