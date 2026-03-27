# ShrouDB Issues

## ~~Server response fields don't match protocol.toml spec~~ (FIXED in v0.1.2)

**Found:** 2026-03-24 during codegen client sandbox testing
**Fixed:** shroudb v0.1.2
**Severity:** High — generated SDK clients can't parse server responses

The server's RESP3 responses use different field names than what `protocol.toml` defines. Generated clients parse responses based on the spec, so they crash with `KeyError`/missing field errors.

### Known mismatches

| Command | Spec field | Server field | Notes |
|---------|-----------|-------------|-------|
| `ISSUE` | `token` | `api_key` | Server returns keyspace-type-specific name instead of generic `token` |
| `HEALTH` | `state` = `"ready"` | `state` = `"READY"` | Uppercase vs lowercase — spec says `"ready"`, server sends `"READY"` |
| `JWKS` | `keys` | `jwks` | Spec says `keys` (RFC 7517 §5), server returns `jwks` |
| `INSPECT` | `state` = `"active"` | `state` = `"Active"` | Title-cased vs lowercase — affects all state comparisons |

### How to reproduce

```bash
cd shroudb-codegen/test-sandbox
make test-clients
```

The Python test connects, calls `ISSUE test-apikeys`, and the generated `IssueResponse._from_dict()` crashes:
```
KeyError: 'token'
```

Because the server returns `api_key` instead of `token` in the RESP3 map.

### Resolution options

**Option A (preferred):** Update the server's response serialization to match protocol.toml. The spec is the contract — use `token` consistently regardless of keyspace type. Normalize `state` to lowercase.

**Option B:** Update protocol.toml to match what the server actually returns. This means ISSUE would need per-keyspace-type response schemas (api_key vs jwt vs signature), which complicates codegen significantly.

### Likely affected commands

Any command whose response field names were chosen independently of the spec. The full list should be audited by comparing `protocol.toml` response definitions against the actual `CommandResponse` serialization in `shroudb-protocol/src/handlers/`.

---

## Reset tokens are not single-use (JWT revocation unsupported)

**Found:** 2026-03-24 during shroudb-auth integration testing
**Severity:** Medium — password reset tokens can be reused

The `reset_password` handler issues a JWT with a `jti` claim, then after a successful reset attempts to revoke that `jti` via `REVOKE {ks}_access {jti}`. This silently fails because the `REVOKE` command's `revoke_single` function rejects JWT keyspaces with `WrongType` (`expected "api_key or refresh_token"`).

The JWT `VERIFY` handler already supports revocation checks — it looks up `jti` in the in-memory revocation set (line 87-96 of `verify.rs`). But there's no way to **add** an entry to that set for JWT keyspaces because `REVOKE` blocks it.

### How to reproduce

```rust
// In shroudb-auth/tests/integration.rs (currently #[ignore])
#[tokio::test]
async fn reset_token_is_single_use() { ... }
```

### Fix

In `shroudb-protocol/src/handlers/revoke.rs`, add a `KeyspaceType::Jwt` arm to `revoke_single` that inserts into the revocation set without writing a WAL entry (JWTs are stateless — the revocation set is purely in-memory with TTL-based expiry):

```rust
KeyspaceType::Jwt => {
    // No WAL entry — just add to in-memory revocation set
}
```

Then update `shroudb-auth/src/routes.rs` `reset_password` to extract `jti` from the verified claims (not `credential_id` from the response, which is absent for JWT VERIFY).

### JWKS response field name

The JWKS endpoint returns `{"jwks": [...], "status": "OK"}` rather than the standard `{"keys": [...]}`. The `shroudb-auth` route handler passes through the raw RESP3 response, which uses `jwks` as the field name. Consumers expecting RFC 7517 format (`keys`) will break.

---

## Remote auth mode returns NOTFOUND for valid keyspaces

**Found:** 2026-03-24 during Docker E2E testing
**Severity:** High — remote auth mode is non-functional in Docker

When `shroudb-auth` runs in remote mode (proxying to an external shroudb server via TCP), all keyspace operations return `NOTFOUND keyspace: {name}` even though the keyspaces provably exist on the shroudb server (verified via direct TCP).

### How to reproduce

```bash
docker compose -f e2e/docker-compose.yml up -d
E2E_REMOTE=1 ./e2e/run.sh
```

Or manually:
```bash
# Direct TCP works:
printf '*5\r\n$8\r\nPASSWORD\r\n$3\r\nSET\r\n$18\r\ndefault_passwords\r\n$4\r\ntest\r\n$6\r\npass12\r\n' | nc localhost 6399
# → returns OK

# Remote auth fails:
curl -X POST http://localhost:4002/auth/default/signup \
  -H "Content-Type: application/json" \
  -d '{"user_id":"test","password":"pass12"}'
# → {"error":"NOTFOUND keyspace: default_passwords"}
```

### Investigation notes

- Health check passes (remote server is connected to shroudb)
- Embedded auth with identical keyspace config works perfectly
- The `RemoteDispatcher` serializes via `Command::to_wire_args()` → `ShrouDBClient::raw_command()`
- Likely cause: serialization mismatch in `to_wire_args()` for password commands, or the shroudb server's command parser handles the wire format differently than expected
