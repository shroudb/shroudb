# The ShrouDB Platform

ShrouDB is a family of self-hosted, security-first products for managing credentials, encryption, and sessions. Each product shares a common foundation — RESP3 protocol, AES-256-GCM encryption, WAL-based durability, automatic key rotation — but serves a distinct purpose.

## The Three Products

| | **ShrouDB** | **ShrouDB Transit** | **ShrouDB Session** |
|---|---|---|---|
| **One-liner** | Credential database | Encryption-as-a-service | Session middleware |
| **Answers** | "Where do my auth credentials live?" | "How do I encrypt data without managing keys?" | "How do users log in?" |
| **Stores** | JWTs, API keys, refresh tokens, HMAC keys, passwords | Encryption keys only — never your data | Nothing — stateless middleware |
| **Ships as** | Rust binary | Rust binary | npm package (`@shroudb/session`) |
| **Talks to** | Your application (RESP3) | Your application (RESP3) | Your application (middleware) + ShrouDB (TCP) |

```
┌─────────────────────────────────────────────────────────────┐
│  Your Application                                           │
├──────────┬───────────────────┬──────────────────────────────┤
│          │                   │                              │
│  @shroudb/session              │                              │
│  (middleware)                │                              │
│     │                       │                              │
│     ▼                       ▼                 ▼            │
│  ┌──────────┐      ┌──────────────┐    ┌────────────────┐  │
│  │  ShrouDB   │      │ShrouDB Transit │    │  Your storage  │  │
│  │  :6399   │      │    :6400     │    │  (Postgres,    │  │
│  │          │      │              │    │   S3, Redis)   │  │
│  └──────────┘      └──────────────┘    └────────────────┘  │
│   Credentials       Encrypt/Decrypt     Stores ciphertext  │
│   & passwords       with managed keys   from Transit       │
└─────────────────────────────────────────────────────────────┘
```

---

## ShrouDB — Credential Database

A single Rust binary that manages the cryptographic credentials your auth system runs on.

### What It Manages

- **JWT Signing Keys** — Asymmetric key pairs (ES256, RS256, EdDSA, etc.) with automatic rotation, drain periods, and a built-in JWKS endpoint.
- **API Keys** — Bearer tokens with SHA-256 hashed storage (plaintext never persisted), optional prefixes, and metadata attachment.
- **HMAC Keys** — Symmetric signing keys for webhook verification with rotation support.
- **Refresh Tokens** — Rotating tokens with family-based revocation, reuse detection, and chain length limiting.
- **Passwords** — Argon2id, bcrypt, and scrypt hashing with rate limiting, account lockout, and transparent rehashing.

### Core Commands

| Command | Description |
|---|---|
| `ISSUE` | Create a new credential (JWT, API key, refresh token) |
| `VERIFY` | Check if a credential is valid and decode it |
| `REVOKE` | Invalidate a credential (single, family, or bulk) |
| `REFRESH` | Exchange a refresh token for a new one (with reuse detection) |
| `ROTATE` | Trigger signing key rotation |
| `JWKS` | Get the JSON Web Key Set for a JWT keyspace |
| `PASSWORD SET/VERIFY/CHANGE` | Full password lifecycle |

---

## ShrouDB Transit — Encryption as a Service

A key management and encryption API. Your application sends plaintext, gets back ciphertext encrypted with a managed key. You store the ciphertext wherever you want. Key material never leaves ShrouDB Transit.

### How It Works

```
App sends plaintext ──→ Transit encrypts ──→ App stores ciphertext in Postgres/S3/Redis
App sends ciphertext ──→ Transit decrypts ──→ App receives plaintext
```

The server never sees or stores your data. It manages the keys and performs cryptographic operations — that's it.

### Commands

| Command | Description |
|---|---|
| `ENCRYPT <keyring> <plaintext>` | Encrypt with the active key version, return ciphertext + key version |
| `DECRYPT <keyring> <ciphertext>` | Decrypt using the embedded key version |
| `REWRAP <keyring> <ciphertext>` | Decrypt with the old version, re-encrypt with the current version |
| `GENERATE_DATA_KEY <keyring>` | Envelope encryption — return a random DEK (plaintext + wrapped). Encrypt locally, store the wrapped key. Unwrap later via DECRYPT. |
| `SIGN <keyring> <data>` | Detached signature with the active key |
| `VERIFY_SIGNATURE <keyring> <data> <sig>` | Verify a detached signature |
| `HASH <algorithm> <data>` | One-way hash (SHA-256, SHA-384, SHA-512) |
| `ROTATE <keyring>` | Rotate key version: Staged → Active → Draining → Retired |
| `KEY_INFO <keyring>` | Key metadata, versions, and state |

### What's Different from ShrouDB

- **No credential storage.** Transit doesn't store API keys, tokens, or passwords. It stores encryption keys and performs operations with them.
- **Keyrings instead of keyspaces.** A keyring holds versioned encryption keys. Same lifecycle state machine, different purpose.
- **Ciphertext format.** Output of ENCRYPT includes the key version so DECRYPT knows which key to use — the caller doesn't track it.
- **Envelope encryption.** `GENERATE_DATA_KEY` enables the caller to encrypt locally at high throughput while Transit manages the key hierarchy.

---

## ShrouDB Session — Session Middleware

A stateless middleware library that composes ShrouDB primitives (JWT + refresh tokens + passwords) into a complete auth flow with cookie management. Ships as `@shroudb/session` for Node.js.

### Setup

```typescript
import { shroudbSession } from '@shroudb/session';

app.use(shroudbSession({
  shroudb: 'shroudb://localhost:6399',
  jwtKeyspace: 'sessions',
  passwordKeyspace: 'users',
  cookieName: 'sid',
  csrfProtection: true,
  rememberMe: { ttl: '30d' },
}));
```

### Endpoints

| Endpoint | Method | Description |
|---|---|---|
| `/session/signup` | POST | Create user → create session → set cookies |
| `/session/login` | POST | Verify password → issue tokens → set cookies |
| `/session/logout` | POST | Revoke refresh token → clear cookies |
| `/session/logout-all` | POST | Revoke entire token family → clear cookies |
| `/session/validate` | GET | Extract cookie → verify JWT → return user context |
| `/session/refresh` | POST | Silent token rotation → new cookies |
| `/session/password/change` | POST | Change password via ShrouDB |
| `/session/password/reset` | POST | Generate reset token → send email (webhook) |
| `/session/csrf` | GET | Generate CSRF token |
| `/session/oauth/:provider/start` | GET | Redirect to OAuth provider |
| `/session/oauth/:provider/callback` | GET | Token exchange → create session |

### What Session Owns

- Cookie lifecycle (HttpOnly, SameSite, Secure, Path, Domain, Max-Age)
- CSRF token generation and validation
- "Remember me" semantics (long vs short refresh TTL)
- Device/IP binding (metadata on refresh tokens)
- OAuth token exchange (GitHub, Google, etc.)
- Password reset flow
- HTTP-layer rate limiting

### What Session Delegates to ShrouDB

- Password hashing
- Token generation and verification
- Credential storage
- Key rotation

---

## How They Compare to Auth0 / Clerk

| | **Auth0 / Clerk** | **ShrouDB Platform** |
|---|---|---|
| **Scope** | Full identity SaaS (login UI, social login, user management) | Credential infrastructure + encryption + session middleware |
| **Hosting** | Their servers | Your servers — single binaries, zero external dependencies |
| **Data ownership** | They hold your keys, tokens, and user data | You own everything — encrypted on your disk |
| **Latency** | Network hop to their API on every auth check | In-process memory lookups; sub-millisecond VERIFY |
| **Key rotation** | Manual or limited automation | Automatic rotation with drain periods across all products |
| **Encryption** | Not offered | ShrouDB Transit — full encryption-as-a-service with envelope encryption |
| **Cost model** | Per-MAU pricing (scales with users) | Free — your infrastructure |
| **Vendor lock-in** | Significant (proprietary SDKs, hosted user store) | RESP3 wire protocol, standard crypto, export-ready |
| **Session management** | Built-in (their way) | ShrouDB Session — same DX, your infrastructure, full control |

### When to Use Auth0 / Clerk

If you want a turnkey login experience — social login, hosted UI, user management, passwordless flows — and don't need fine-grained control over credential lifecycle. They solve a higher-level problem and get you to market faster when identity _is_ the product concern.

### When to Use ShrouDB

- **You need data sovereignty.** Credentials and encryption keys never leave your infrastructure.
- **You need encryption-as-a-service.** Transit lets you encrypt application data without managing keys.
- **You need multiple credential types.** Instead of stitching together Vault + Redis + your database + bcrypt, ShrouDB unifies them.
- **You want auth0/Clerk DX without the vendor lock-in.** ShrouDB Session gives you signup/login/logout/refresh with one middleware call, backed by infrastructure you own.
- **You need audit compliance.** Every operation is logged with actor tracking, structured for compliance requirements.

### Using Them Together

ShrouDB pairs well _alongside_ an identity provider:

- **Clerk** handles user-facing login (social auth, MFA, hosted UI).
- **ShrouDB** manages the signing keys, API keys, and refresh tokens your backend issues _after_ authentication.
- **ShrouDB Transit** encrypts sensitive application data (PII, medical records, financial data) at rest.

---

## Shared Foundation

All three products share ~80% of their code:

- **RESP3 protocol** — Battle-tested wire format with binary safety
- **AES-256-GCM encryption** — Per-keyspace/keyring derived keys via HKDF
- **WAL-based durability** — Append-only log with crash recovery and snapshots
- **Automatic key rotation** — Staged → Active → Draining → Retired lifecycle
- **mlock-pinned memory** — Secrets never swap to disk
- **Zeroize-on-drop** — Key material is wiped when no longer needed
- **Policy-based auth** — Token → policy → keyspace/keyring + command ACLs
- **Audit logging** — Structured JSON with actor tracking
- **RESP3 wire protocol** — Purpose-built command protocol with pipelining
