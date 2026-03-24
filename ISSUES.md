# ShrouDB Issues

## Server response fields don't match protocol.toml spec

**Found:** 2026-03-24 during codegen client sandbox testing
**Severity:** High — generated SDK clients can't parse server responses

The server's RESP3 responses use different field names than what `protocol.toml` defines. Generated clients parse responses based on the spec, so they crash with `KeyError`/missing field errors.

### Known mismatches

| Command | Spec field | Server field | Notes |
|---------|-----------|-------------|-------|
| `ISSUE` | `token` | `api_key` | Server returns keyspace-type-specific name instead of generic `token` |
| `HEALTH` | `state` = `"ready"` | `state` = `"READY"` | Uppercase vs lowercase — spec says `"ready"`, server sends `"READY"` |

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
