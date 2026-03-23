# Keyva Command Protocol Specification

This document defines the wire protocol and command set for Keyva. SDK authors should use this as the reference for building client libraries.

---

## Connection

### URI Scheme

```
keyva://[token@]host[:port][/keyspace]
keyva+tls://[token@]host[:port][/keyspace]
```

**Examples:**

```
keyva://localhost                     # plain TCP, default port 6399
keyva://localhost:6399                # explicit port
keyva+tls://prod.example.com         # TLS, default port 6399
keyva://mytoken@localhost:6399       # with auth token
keyva://mytoken@localhost/sessions   # with auth and default keyspace
keyva+tls://tok@host:6399/keys      # full form
```

### Default Port

**6399** (TCP and TLS).

### Transport

Plain TCP or TLS. The server advertises TLS support via the `tls_cert` and `tls_key` configuration options. Mutual TLS (mTLS) is supported via `tls_client_ca`.

---

## Wire Format

Keyva uses a **RESP3 subset** as its wire format. RESP3 was chosen because it is a well-established, binary-safe framing protocol with clean semantics for strings, integers, maps, and errors — not because Keyva is related to Redis. Keyva has its own command set (ISSUE, VERIFY, REVOKE, ROTATE, etc.) and is not compatible with Redis clients. SDK authors should implement RESP3 framing directly (it is a simple protocol) or use a standalone RESP3 codec library.

### Supported Types (7)

| Prefix | Type         | Example                          |
|--------|--------------|----------------------------------|
| `+`    | Simple String| `+OK\r\n`                        |
| `-`    | Error        | `-NOTFOUND credential not found\r\n` |
| `:`    | Integer      | `:42\r\n`                        |
| `$`    | Bulk String  | `$5\r\nhello\r\n`               |
| `*`    | Array        | `*2\r\n$3\r\nfoo\r\n$3\r\nbar\r\n` |
| `%`    | Map          | `%1\r\n+key\r\n+val\r\n`        |
| `_`    | Null         | `_\r\n`                          |

### Request Format

Clients send commands as RESP3 arrays of bulk strings:

```
*3\r\n
$5\r\nISSUE\r\n
$6\r\ntokens\r\n
$3\r\nTTL\r\n
$4\r\n3600\r\n
```

---

## Authentication

When authentication is configured on the server, the **first command** on any new connection must be `AUTH <token>`. All other commands will return `-DENIED authentication required` until authentication succeeds.

```
> AUTH my-secret-token
< +OK
```

---

## Command Syntax

All commands follow the pattern:

```
VERB keyspace [KEY value]...
```

- **VERB**: uppercase command name (e.g., `ISSUE`, `VERIFY`, `REVOKE`)
- **keyspace**: the target keyspace name (alphanumeric + hyphens)
- **KEY value pairs**: optional named parameters in uppercase

---

## Response Envelope

### Success

Every successful response is a **RESP3 map** containing at minimum:

```
%N\r\n
+status\r\n
+OK\r\n
... additional fields ...
```

The `status` field is always `"OK"`.

### Error

Every error response is a **RESP3 error string**:

```
-CODE details\r\n
```

Where `CODE` is one of the defined error codes and `details` is a human-readable message.

---

## Error Codes

| Code               | Meaning                                                       |
|--------------------|---------------------------------------------------------------|
| `DENIED`           | Authentication required or insufficient permissions           |
| `NOTFOUND`         | Credential, keyspace, or resource does not exist              |
| `BADARG`           | Missing or malformed command argument                         |
| `VALIDATION_ERROR` | Metadata or claims failed schema validation                   |
| `WRONGTYPE`        | Operation not supported for this keyspace type                |
| `STATE_ERROR`      | Credential is in wrong state for this operation (e.g., already revoked) |
| `EXPIRED`          | Credential has expired                                        |
| `REUSE_DETECTED`   | Refresh token reuse detected — family revoked                 |
| `CHAIN_LIMIT`      | Refresh token chain limit exceeded                            |
| `LOCKED`           | Account temporarily locked due to too many failed attempts    |
| `DISABLED`         | Keyspace is disabled                                          |
| `NOTREADY`         | Server is not ready (still starting up)                       |
| `STORAGE`          | Storage engine error (WAL write failed, snapshot error, etc.) |
| `CRYPTO`           | Cryptographic operation failed                                |
| `INTERNAL`         | Unexpected internal error                                     |

---

## Command Reference

### ISSUE

Issue a new credential in the given keyspace.

**Syntax:**

```
ISSUE <keyspace> [CLAIMS <json>] [META <json>] [TTL <secs>] [IDEMPOTENCY_KEY <key>]
```

**Parameters:**

| Name             | Required | Type   | Description                                              |
|------------------|----------|--------|----------------------------------------------------------|
| `keyspace`       | Yes      | string | Target keyspace name                                     |
| `CLAIMS`         | No       | JSON   | JWT claims object (JWT keyspaces only)                   |
| `META`           | No       | JSON   | Metadata to attach to the credential                     |
| `TTL`            | No       | integer| Time-to-live in seconds (overrides keyspace default)     |
| `IDEMPOTENCY_KEY`| No       | string | Prevents duplicate issuance for the same key within 5 min|

**Response fields:**

| Field            | Type   | Description                                     |
|------------------|--------|-------------------------------------------------|
| `status`         | string | `"OK"`                                          |
| `credential_id`  | string | Unique credential identifier                    |
| `token`          | string | The issued token/key (JWT, API key, etc.)       |
| `expires_at`     | integer| Unix timestamp when the credential expires (if TTL set) |
| `family_id`      | string | Refresh token family ID (refresh_token keyspaces only) |

**Error cases:**

- `BADARG` — missing keyspace, invalid JSON, invalid TTL
- `VALIDATION_ERROR` — claims or metadata fail schema validation
- `WRONGTYPE` — CLAIMS used on non-JWT keyspace
- `DISABLED` — keyspace is disabled
- `STORAGE` — WAL write failed

**Example:**

```
> ISSUE tokens CLAIMS {"sub":"user123","role":"admin"} TTL 3600
< %4
<   +status    +OK
<   +credential_id    +cred_abc123
<   +token    +eyJhbGciOiJFUzI1NiJ9...
<   +expires_at    :1711152000
```

---

### VERIFY

Verify a credential (JWT token, API key, or HMAC signature).

**Syntax:**

```
VERIFY <keyspace> <token> [PAYLOAD <data>] [CHECKREV]
```

**Parameters:**

| Name       | Required | Type   | Description                                         |
|------------|----------|--------|-----------------------------------------------------|
| `keyspace` | Yes      | string | Target keyspace name                                |
| `token`    | Yes      | string | The token/key/signature to verify                   |
| `PAYLOAD`  | No       | string | Original message for HMAC verification              |
| `CHECKREV` | No       | flag   | Also check the revocation index (slower but definitive) |

**Response fields:**

| Field            | Type   | Description                                     |
|------------------|--------|-------------------------------------------------|
| `status`         | string | `"OK"`                                          |
| `credential_id`  | string | Credential identifier                           |
| `claims`         | map    | Decoded JWT claims (JWT keyspaces only)         |
| `meta`           | map    | Attached metadata                               |
| `state`          | string | Credential state (`active`, `suspended`)        |

**Error cases:**

- `BADARG` — missing keyspace or token
- `NOTFOUND` — credential does not exist
- `EXPIRED` — credential has expired
- `STATE_ERROR` — credential is suspended or revoked
- `WRONGTYPE` — PAYLOAD used on non-HMAC keyspace
- `DISABLED` — keyspace is disabled
- `CRYPTO` — signature verification failed

**Example:**

```
> VERIFY tokens eyJhbGciOiJFUzI1NiJ9... CHECKREV
< %3
<   +status    +OK
<   +credential_id    +cred_abc123
<   +claims    %2 +sub +user123 +role +admin
```

---

### REVOKE

Revoke a credential by ID, revoke an entire refresh token family, or bulk-revoke multiple credentials.

**Syntax (single):**

```
REVOKE <keyspace> <credential_id>
```

**Syntax (family):**

```
REVOKE <keyspace> FAMILY <family_id>
```

**Syntax (bulk):**

```
REVOKE <keyspace> BULK <id1> <id2> ...
```

**Parameters:**

| Name             | Required | Type   | Description                              |
|------------------|----------|--------|------------------------------------------|
| `keyspace`       | Yes      | string | Target keyspace name                     |
| `credential_id`  | Yes*     | string | Credential to revoke (single mode)       |
| `FAMILY`         | No       | flag   | Indicates family revocation mode         |
| `family_id`      | Yes*     | string | Family ID to revoke (family mode)        |
| `BULK`           | No       | flag   | Indicates bulk revocation mode           |
| `id1`, `id2`...  | Yes*     | string | Credential IDs to revoke (bulk mode)     |

\* Required depending on the revocation mode.

**Response fields:**

| Field    | Type    | Description                                      |
|----------|---------|--------------------------------------------------|
| `status` | string  | `"OK"`                                           |
| `revoked`| integer | Number of credentials revoked (bulk/family mode) |

**Error cases:**

- `BADARG` — missing keyspace or credential ID
- `NOTFOUND` — credential or family does not exist
- `STATE_ERROR` — credential is already revoked
- `DISABLED` — keyspace is disabled
- `STORAGE` — WAL write failed

**Example:**

```
> REVOKE tokens cred_abc123
< %1
<   +status    +OK

> REVOKE sessions FAMILY fam_xyz789
< %2
<   +status    +OK
<   +revoked   :3

> REVOKE tokens BULK cred_1 cred_2 cred_3
< %2
<   +status    +OK
<   +revoked   :3
```

---

### REFRESH

Exchange a refresh token for a new one. The old token is consumed.

**Syntax:**

```
REFRESH <keyspace> <token>
```

**Parameters:**

| Name       | Required | Type   | Description                  |
|------------|----------|--------|------------------------------|
| `keyspace` | Yes      | string | Target keyspace name         |
| `token`    | Yes      | string | Current refresh token        |

**Response fields:**

| Field            | Type   | Description                                     |
|------------------|--------|-------------------------------------------------|
| `status`         | string | `"OK"`                                          |
| `credential_id`  | string | New credential identifier                       |
| `token`          | string | New refresh token                               |
| `family_id`      | string | Family ID (unchanged)                           |
| `expires_at`     | integer| Unix timestamp when the new token expires       |

**Error cases:**

- `BADARG` — missing keyspace or token
- `NOTFOUND` — token does not exist
- `EXPIRED` — token has expired
- `REUSE_DETECTED` — token was already consumed (entire family is revoked)
- `CHAIN_LIMIT` — refresh chain has exceeded the configured limit
- `WRONGTYPE` — keyspace is not a refresh_token type
- `DISABLED` — keyspace is disabled

**Example:**

```
> REFRESH sessions rt_abc123def456...
< %4
<   +status    +OK
<   +credential_id    +cred_newxyz
<   +token    +rt_newtoken789...
<   +family_id    +fam_xyz789
```

---

### UPDATE

Update metadata on an existing credential.

**Syntax:**

```
UPDATE <keyspace> <credential_id> META <json>
```

**Parameters:**

| Name            | Required | Type   | Description                              |
|-----------------|----------|--------|------------------------------------------|
| `keyspace`      | Yes      | string | Target keyspace name                     |
| `credential_id` | Yes      | string | Credential to update                     |
| `META`          | Yes      | JSON   | Metadata fields to merge                 |

**Response fields:**

| Field    | Type   | Description |
|----------|--------|-------------|
| `status` | string | `"OK"`      |

**Error cases:**

- `BADARG` — missing arguments or invalid JSON
- `NOTFOUND` — credential does not exist
- `VALIDATION_ERROR` — metadata fails schema validation or attempts to change immutable fields
- `STATE_ERROR` — credential is revoked
- `DISABLED` — keyspace is disabled

**Example:**

```
> UPDATE keys cred_abc123 META {"plan":"pro","upgraded":true}
< %1
<   +status    +OK
```

---

### INSPECT

Retrieve full details about a credential.

**Syntax:**

```
INSPECT <keyspace> <credential_id>
```

**Parameters:**

| Name            | Required | Type   | Description                  |
|-----------------|----------|--------|------------------------------|
| `keyspace`      | Yes      | string | Target keyspace name         |
| `credential_id` | Yes      | string | Credential to inspect        |

**Response fields:**

| Field              | Type    | Description                              |
|--------------------|---------|------------------------------------------|
| `status`           | string  | `"OK"`                                   |
| `credential_id`    | string  | Credential identifier                    |
| `state`            | string  | `active`, `suspended`, `revoked`         |
| `created_at`       | integer | Unix timestamp of creation               |
| `expires_at`       | integer | Unix timestamp of expiration (if set)    |
| `last_verified_at` | integer | Unix timestamp of last verification      |
| `meta`             | map     | Attached metadata                        |
| `family_id`        | string  | Family ID (refresh_token keyspaces only) |

**Error cases:**

- `BADARG` — missing arguments
- `NOTFOUND` — credential does not exist
- `DISABLED` — keyspace is disabled

**Example:**

```
> INSPECT tokens cred_abc123
< %6
<   +status           +OK
<   +credential_id    +cred_abc123
<   +state            +active
<   +created_at       :1711148400
<   +expires_at       :1711152000
<   +meta             %1 +plan +free
```

---

### ROTATE

Trigger signing key rotation for a keyspace. The current active key enters drain mode and a new key becomes active.

**Syntax:**

```
ROTATE <keyspace> [FORCE] [NOWAIT] [DRYRUN]
```

**Parameters:**

| Name       | Required | Type   | Description                                          |
|------------|----------|--------|------------------------------------------------------|
| `keyspace` | Yes      | string | Target keyspace name                                 |
| `FORCE`    | No       | flag   | Rotate even if current key has not reached rotation age |
| `NOWAIT`   | No       | flag   | Return immediately without waiting for completion    |
| `DRYRUN`   | No       | flag   | Preview what would happen without making changes     |

**Response fields:**

| Field          | Type   | Description                              |
|----------------|--------|------------------------------------------|
| `status`       | string | `"OK"`                                   |
| `new_key_id`   | string | ID of the newly created key              |
| `old_key_id`   | string | ID of the key that entered drain mode    |
| `dryrun`       | string | `"true"` if this was a dry run           |

**Error cases:**

- `BADARG` — missing keyspace
- `WRONGTYPE` — keyspace does not support key rotation (e.g., api_key)
- `DISABLED` — keyspace is disabled
- `STORAGE` — WAL write failed

**Example:**

```
> ROTATE tokens FORCE
< %3
<   +status       +OK
<   +new_key_id   +key_20240322
<   +old_key_id   +key_20240101

> ROTATE tokens DRYRUN
< %3
<   +status       +OK
<   +new_key_id   +key_preview
<   +dryrun       +true
```

---

### JWKS

Return the JSON Web Key Set for a JWT keyspace. Includes all active and draining public keys.

**Syntax:**

```
JWKS <keyspace>
```

**Parameters:**

| Name       | Required | Type   | Description          |
|------------|----------|--------|----------------------|
| `keyspace` | Yes      | string | Target JWT keyspace  |

**Response fields:**

| Field    | Type   | Description                          |
|----------|--------|--------------------------------------|
| `status` | string | `"OK"`                               |
| `jwks`   | string | JSON-encoded JWKS document           |

**Error cases:**

- `BADARG` — missing keyspace
- `WRONGTYPE` — keyspace is not a JWT type
- `DISABLED` — keyspace is disabled

**Example:**

```
> JWKS tokens
< %2
<   +status    +OK
<   +jwks      +{"keys":[{"kty":"EC","crv":"P-256","kid":"key_20240322",...}]}
```

---

### KEYSTATE

Show the current key ring state for a keyspace.

**Syntax:**

```
KEYSTATE <keyspace>
```

**Parameters:**

| Name       | Required | Type   | Description          |
|------------|----------|--------|----------------------|
| `keyspace` | Yes      | string | Target keyspace      |

**Response fields:**

| Field    | Type  | Description                                       |
|----------|-------|---------------------------------------------------|
| `status` | string| `"OK"`                                            |
| `keys`   | array | Array of key info maps (key_id, state, created_at)|

**Error cases:**

- `BADARG` — missing keyspace
- `WRONGTYPE` — keyspace does not have a key ring
- `DISABLED` — keyspace is disabled

**Example:**

```
> KEYSTATE tokens
< %2
<   +status    +OK
<   +keys      *2
<     %3 +key_id +key_20240322 +state +active +created_at :1711065600
<     %3 +key_id +key_20240101 +state +draining +created_at :1704067200
```

---

### HEALTH

Check server or keyspace health.

**Syntax:**

```
HEALTH [<keyspace>]
```

**Parameters:**

| Name       | Required | Type   | Description                        |
|------------|----------|--------|------------------------------------|
| `keyspace` | No       | string | Check a specific keyspace's health |

**Response fields (server-level):**

| Field       | Type   | Description                              |
|-------------|--------|------------------------------------------|
| `status`    | string | `"OK"`                                   |
| `state`     | string | Engine state (e.g., `"ready"`)           |
| `keyspaces` | map    | Per-keyspace credential counts           |

**Error cases:**

- `NOTFOUND` — specified keyspace does not exist
- `NOTREADY` — server is still starting up

**Example:**

```
> HEALTH
< %3
<   +status      +OK
<   +state       +ready
<   +keyspaces   %2
<     +tokens    :1234
<     +sessions  :567

> HEALTH tokens
< %2
<   +status    +OK
<   +count     :1234
```

---

### KEYS

List credential IDs in a keyspace with optional filtering and cursor-based pagination.

**Syntax:**

```
KEYS <keyspace> [CURSOR <c>] [MATCH <p>] [STATE <s>] [COUNT <n>]
```

**Parameters:**

| Name       | Required | Type   | Description                                        |
|------------|----------|--------|----------------------------------------------------|
| `keyspace` | Yes      | string | Target keyspace name                               |
| `CURSOR`   | No       | string | Resume from a previous scan cursor                 |
| `MATCH`    | No       | string | Glob-style pattern to filter credential IDs        |
| `STATE`    | No       | string | Filter by state: `active`, `suspended`, `revoked`  |
| `COUNT`    | No       | integer| Max results to return (default: 100)               |

**Response fields:**

| Field    | Type   | Description                                      |
|----------|--------|--------------------------------------------------|
| `status` | string | `"OK"`                                           |
| `cursor` | string | Cursor for next page (`"0"` when scan is complete)|
| `keys`   | array  | Array of credential ID strings                   |

**Error cases:**

- `BADARG` — missing keyspace, invalid COUNT, invalid STATE
- `DISABLED` — keyspace is disabled

**Example:**

```
> KEYS tokens COUNT 50 STATE active
< %3
<   +status    +OK
<   +cursor    +abc123
<   +keys      *3 +cred_1 +cred_2 +cred_3

> KEYS tokens CURSOR abc123 COUNT 50
< %3
<   +status    +OK
<   +cursor    +0
<   +keys      *1 +cred_4
```

---

### SUSPEND

Temporarily suspend a credential. Suspended credentials fail verification but can be unsuspended.

**Syntax:**

```
SUSPEND <keyspace> <credential_id>
```

**Parameters:**

| Name            | Required | Type   | Description              |
|-----------------|----------|--------|--------------------------|
| `keyspace`      | Yes      | string | Target keyspace name     |
| `credential_id` | Yes      | string | Credential to suspend    |

**Response fields:**

| Field    | Type   | Description |
|----------|--------|-------------|
| `status` | string | `"OK"`      |

**Error cases:**

- `BADARG` — missing arguments
- `NOTFOUND` — credential does not exist
- `STATE_ERROR` — credential is already suspended or revoked
- `DISABLED` — keyspace is disabled

**Example:**

```
> SUSPEND tokens cred_abc123
< %1
<   +status    +OK
```

---

### UNSUSPEND

Reactivate a previously suspended credential.

**Syntax:**

```
UNSUSPEND <keyspace> <credential_id>
```

**Parameters:**

| Name            | Required | Type   | Description              |
|-----------------|----------|--------|--------------------------|
| `keyspace`      | Yes      | string | Target keyspace name     |
| `credential_id` | Yes      | string | Credential to unsuspend  |

**Response fields:**

| Field    | Type   | Description |
|----------|--------|-------------|
| `status` | string | `"OK"`      |

**Error cases:**

- `BADARG` — missing arguments
- `NOTFOUND` — credential does not exist
- `STATE_ERROR` — credential is not suspended
- `DISABLED` — keyspace is disabled

**Example:**

```
> UNSUSPEND tokens cred_abc123
< %1
<   +status    +OK
```

---

### SCHEMA

Display the metadata schema for a keyspace.

**Syntax:**

```
SCHEMA <keyspace>
```

**Parameters:**

| Name       | Required | Type   | Description          |
|------------|----------|--------|----------------------|
| `keyspace` | Yes      | string | Target keyspace      |

**Response fields:**

| Field    | Type   | Description                                      |
|----------|--------|--------------------------------------------------|
| `status` | string | `"OK"`                                           |
| `schema` | map    | Schema definition (fields, types, constraints)   |

**Error cases:**

- `BADARG` — missing keyspace
- `NOTFOUND` — keyspace does not exist
- `DISABLED` — keyspace is disabled

**Example:**

```
> SCHEMA tokens
< %2
<   +status    +OK
<   +schema    %2
<     +fields  *2
<       %3 +name +plan +type +string +required +true
<       %3 +name +role +type +string +required +false
```

---

### PASSWORD SET

Set a password for a user in a password keyspace. The plaintext is hashed using the keyspace's configured algorithm (e.g., Argon2id) before storage. The plaintext is never written to the WAL.

**Syntax:**

```
PASSWORD SET <keyspace> <user_id> <password> [META <json>]
```

**Parameters:**

| Name       | Required | Type   | Description                                      |
|------------|----------|--------|--------------------------------------------------|
| `keyspace` | Yes      | string | Target password keyspace                         |
| `user_id`  | Yes      | string | User identifier to associate with the password   |
| `password` | Yes      | string | Plaintext password (hashed before storage)       |
| `META`     | No       | JSON   | Metadata to attach to the password credential    |

**Response fields:**

| Field           | Type    | Description                              |
|-----------------|---------|------------------------------------------|
| `status`        | string  | `"OK"`                                   |
| `credential_id` | string  | Unique credential identifier             |
| `user_id`       | string  | The user ID                              |
| `algorithm`     | string  | Hash algorithm used (e.g., `"Argon2id"`) |
| `created_at`    | integer | Unix timestamp of creation               |

**Error cases:**

- `BADARG` — missing keyspace, user_id, or password
- `WRONGTYPE` — keyspace is not a password type
- `STATE_ERROR` — user already has a password (use PASSWORD CHANGE instead)
- `VALIDATION_ERROR` — metadata fails schema validation
- `DISABLED` — keyspace is disabled
- `STORAGE` — WAL write failed
- `CRYPTO` — hashing failed

**Example:**

```
> PASSWORD SET users user123 s3cureP@ss META {"role":"admin"}
< %4
<   +status         +OK
<   +credential_id  +cred_abc123
<   +user_id        +user123
<   +algorithm      +Argon2id
<   +created_at     :1711148400
```

---

### PASSWORD VERIFY

Verify a user's password. Returns whether the password is valid. Tracks failed attempts for lockout enforcement. On success, if the stored hash uses stale parameters, a transparent rehash is performed asynchronously.

**Syntax:**

```
PASSWORD VERIFY <keyspace> <user_id> <password>
```

**Parameters:**

| Name       | Required | Type   | Description                    |
|------------|----------|--------|--------------------------------|
| `keyspace` | Yes      | string | Target password keyspace       |
| `user_id`  | Yes      | string | User identifier                |
| `password` | Yes      | string | Plaintext password to verify   |

**Response fields:**

| Field           | Type    | Description                              |
|-----------------|---------|------------------------------------------|
| `status`        | string  | `"OK"`                                   |
| `valid`         | boolean | `true` (only returned on success)        |
| `credential_id` | string  | Credential identifier                    |
| `metadata`      | map     | Attached metadata                        |

**Error cases:**

- `BADARG` — missing keyspace, user_id, or password
- `WRONGTYPE` — keyspace is not a password type
- `NOTFOUND` — user does not exist in this keyspace
- `STATE_ERROR` — password credential is not in Active state
- `DENIED` — password is invalid (returned instead of `valid: false`)
- `LOCKED` — account temporarily locked due to too many failed attempts (includes `retry_after_secs`)
- `DISABLED` — keyspace is disabled
- `CRYPTO` — verification failed

**Example:**

```
> PASSWORD VERIFY users user123 s3cureP@ss
< %3
<   +status         +OK
<   +valid          :1
<   +credential_id  +cred_abc123
<   +metadata       %1 +role +admin

> PASSWORD VERIFY users user123 wrong_password
< -DENIED invalid password
```

---

### PASSWORD CHANGE

Change a user's password. Requires the old password to be verified first.

**Syntax:**

```
PASSWORD CHANGE <keyspace> <user_id> <old_password> <new_password>
```

**Parameters:**

| Name           | Required | Type   | Description                  |
|----------------|----------|--------|------------------------------|
| `keyspace`     | Yes      | string | Target password keyspace     |
| `user_id`      | Yes      | string | User identifier              |
| `old_password` | Yes      | string | Current plaintext password   |
| `new_password` | Yes      | string | New plaintext password       |

**Response fields:**

| Field           | Type    | Description                              |
|-----------------|---------|------------------------------------------|
| `status`        | string  | `"OK"`                                   |
| `credential_id` | string  | Credential identifier                    |
| `updated_at`    | integer | Unix timestamp of the change             |

**Error cases:**

- `BADARG` — missing arguments
- `WRONGTYPE` — keyspace is not a password type
- `NOTFOUND` — user does not exist in this keyspace
- `STATE_ERROR` — password credential is not in Active state
- `DENIED` — old password is incorrect
- `DISABLED` — keyspace is disabled
- `STORAGE` — WAL write failed
- `CRYPTO` — hashing failed

**Example:**

```
> PASSWORD CHANGE users user123 s3cureP@ss n3wP@ssw0rd!
< %2
<   +status         +OK
<   +credential_id  +cred_abc123
<   +updated_at     :1711152000
```

---

### PASSWORD IMPORT

Import a pre-hashed password for a user. Intended for migration from other systems. The hash format is validated and the algorithm is auto-detected from the hash prefix (e.g., `$argon2id$`, `$2b$`, `$scrypt$`).

**Syntax:**

```
PASSWORD IMPORT <keyspace> <user_id> <hash> [META <json>]
```

**Parameters:**

| Name       | Required | Type   | Description                                         |
|------------|----------|--------|-----------------------------------------------------|
| `keyspace` | Yes      | string | Target password keyspace                            |
| `user_id`  | Yes      | string | User identifier                                     |
| `hash`     | Yes      | string | Pre-hashed password string (e.g., argon2id, bcrypt) |
| `META`     | No       | JSON   | Metadata to attach to the password credential       |

**Response fields:**

| Field           | Type    | Description                                      |
|-----------------|---------|--------------------------------------------------|
| `status`        | string  | `"OK"`                                           |
| `credential_id` | string  | Unique credential identifier                     |
| `user_id`       | string  | The user ID                                      |
| `algorithm`     | string  | Detected hash algorithm (e.g., `"Bcrypt"`)       |
| `created_at`    | integer | Unix timestamp of creation                       |

**Error cases:**

- `BADARG` — missing keyspace, user_id, or hash
- `WRONGTYPE` — keyspace is not a password type
- `STATE_ERROR` — user already has a password
- `VALIDATION_ERROR` — unrecognized hash algorithm or invalid hash format
- `DISABLED` — keyspace is disabled
- `STORAGE` — WAL write failed

**Example:**

```
> PASSWORD IMPORT users user456 $argon2id$v=19$m=65536,t=3,p=4$c29tZXNhbHQ$hash...
< %4
<   +status         +OK
<   +credential_id  +cred_def789
<   +user_id        +user456
<   +algorithm      +Argon2id
<   +created_at     :1711148400
```

---

### AUTH

Authenticate the current connection with a bearer token.

**Syntax:**

```
AUTH <token>
```

**Parameters:**

| Name    | Required | Type   | Description          |
|---------|----------|--------|----------------------|
| `token` | Yes      | string | Authentication token |

**Response:**

Simple string `+OK` on success.

**Error cases:**

- `DENIED` — invalid or expired token

**Example:**

```
> AUTH my-secret-token
< +OK
```

---

### CONFIG GET

Retrieve a runtime configuration value.

**Syntax:**

```
CONFIG GET <key>
```

**Parameters:**

| Name  | Required | Type   | Description              |
|-------|----------|--------|--------------------------|
| `key` | Yes      | string | Configuration key name   |

**Response fields:**

| Field   | Type   | Description           |
|---------|--------|-----------------------|
| `status`| string | `"OK"`                |
| `value` | string | Current config value  |

**Error cases:**

- `BADARG` — missing key
- `NOTFOUND` — unknown configuration key
- `DENIED` — authentication required

**Example:**

```
> CONFIG GET max_connections
< %2
<   +status    +OK
<   +value     +1024
```

---

### CONFIG SET

Set a runtime configuration value.

**Syntax:**

```
CONFIG SET <key> <value>
```

**Parameters:**

| Name    | Required | Type   | Description              |
|---------|----------|--------|--------------------------|
| `key`   | Yes      | string | Configuration key name   |
| `value` | Yes      | string | New value                |

**Response:**

Simple string `+OK` on success.

**Error cases:**

- `BADARG` — missing key or value, invalid value type
- `NOTFOUND` — unknown configuration key
- `DENIED` — authentication required

**Example:**

```
> CONFIG SET log_level debug
< +OK
```

---

### SUBSCRIBE

Subscribe to real-time event notifications on a channel.

**Syntax:**

```
SUBSCRIBE <channel>
```

**Parameters:**

| Name      | Required | Type   | Description                    |
|-----------|----------|--------|--------------------------------|
| `channel` | Yes      | string | Channel name (e.g., `keyspace:tokens`) |

**Response:**

Initial confirmation, then push messages as events occur:

```
+subscribed to <channel>
```

Events are delivered as arrays: `[event_type, keyspace, payload...]`.

**Error cases:**

- `BADARG` — missing channel
- `DENIED` — authentication required

**Example:**

```
> SUBSCRIBE keyspace:tokens
< +subscribed to keyspace:tokens
< *3 +issued +tokens +cred_abc123
< *3 +revoked +tokens +cred_abc123
```

---

### PIPELINE ... END

Send multiple commands as a batch. Responses are returned as an array in order.

**Syntax:**

```
PIPELINE
<command1>
<command2>
...
END
```

**Response:**

Array of responses, one per command in the pipeline.

**Error cases:**

- Individual command errors are returned in their respective array positions.
- `BADARG` — if PIPELINE is empty or END is missing

**Example:**

```
> PIPELINE
> ISSUE tokens TTL 3600
> ISSUE tokens TTL 7200
> END
< *2
<   %3 +status +OK +credential_id +cred_1 +token +eyJ...
<   %3 +status +OK +credential_id +cred_2 +token +eyJ...
```
