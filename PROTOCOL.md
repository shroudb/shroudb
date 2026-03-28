# ShrouDB Wire Protocol Specification

Version 1.0.0 -- Encrypted key-value database.

This document is the human-readable wire protocol reference for ShrouDB v1. SDK authors should implement against this specification. The machine-readable source of truth is `protocol.toml`.

---

## 1. Transport

ShrouDB listens on **TCP port 6399** (default) for both plain and TLS connections.

| Mode      | URI scheme        | Description                        |
|-----------|-------------------|------------------------------------|
| Plain TCP | `shroudb://`      | Unencrypted transport              |
| TLS       | `shroudb+tls://`  | TLS-encrypted transport            |

**URI format:**

```
shroudb://[token@]host[:port]
shroudb+tls://[token@]host[:port]
```

TLS is configured on the server via `tls_cert` and `tls_key`. Mutual TLS (mTLS) is supported via `tls_client_ca`.

---

## 2. Wire Format: RESP3

ShrouDB uses a subset of the RESP3 binary protocol for framing. RESP3 was chosen for its simplicity and binary safety, not because ShrouDB is related to Redis. ShrouDB has its own command set and is not compatible with Redis clients.

### 2.1 Supported Types

| Prefix | Type          | Description                          | Example                               |
|--------|---------------|--------------------------------------|---------------------------------------|
| `+`    | Simple String | Short, non-binary string             | `+OK\r\n`                             |
| `-`    | Simple Error  | Error code followed by message       | `-NOT_FOUND key not found\r\n`        |
| `:`    | Integer       | Signed 64-bit integer                | `:42\r\n`                             |
| `$`    | Bulk String   | Length-prefixed binary-safe string   | `$5\r\nhello\r\n`                     |
| `*`    | Array         | Ordered sequence of values           | `*2\r\n$3\r\nfoo\r\n$3\r\nbar\r\n`   |
| `%`    | Map           | Ordered sequence of key-value pairs  | `%1\r\n+key\r\n+val\r\n`             |
| `>`    | Push          | Server-initiated out-of-band message | `>3\r\n+message\r\n+ns\r\n+k\r\n`    |
| `_`    | Null          | Absence of a value                   | `_\r\n`                               |

### 2.2 Request Encoding

Every client request is a RESP3 **Array of Bulk Strings**:

```
*3\r\n
$3\r\nGET\r\n
$7\r\ndefault\r\n
$5\r\nmykey\r\n
```

### 2.3 Response Envelope

**Success:** a RESP3 Map containing at minimum a `status` key with value `"OK"`, plus command-specific fields.

```
%2\r\n
+status\r\n+OK\r\n
+version\r\n:3\r\n
```

**Error:** a RESP3 Simple Error string with the format `CODE message`:

```
-NOT_FOUND key does not exist\r\n
```

---

## 3. Authentication

When authentication is configured, the **first command** on any new connection must be `AUTH`. All other commands return `-NOT_AUTHENTICATED no auth token provided on this connection` until authentication succeeds.

### AUTH

Authenticate the connection with a bearer token.

**Request:**

```
*2\r\n
$4\r\nAUTH\r\n
$14\r\nmy-secret-token\r\n
```

**Success response:**

```
%2\r\n
+status\r\n+OK\r\n
+actor\r\n$7\r\nadmin-1\r\n
```

The `actor` field identifies the authenticated principal.

**Errors:**

| Code     | Condition                |
|----------|--------------------------|
| `DENIED` | Invalid or expired token |

---

## 4. Data Commands

### 4.1 PUT

Store a value at the given key. Auto-increments the version number. Creates the key if it does not exist.

**Request:**

```
PUT <namespace> <key> [<value>] [VALUE <value>] [META <json>]
```

The value can be provided as a positional third argument or via the explicit `VALUE` keyword. `META` attaches a JSON metadata object validated against the namespace schema (if one is configured).

```
*4\r\n
$3\r\nPUT\r\n
$7\r\ndefault\r\n
$5\r\nmykey\r\n
$11\r\nhello world\r\n
```

With metadata:

```
*6\r\n
$3\r\nPUT\r\n
$7\r\ndefault\r\n
$5\r\nmykey\r\n
$11\r\nhello world\r\n
$4\r\nMETA\r\n
$16\r\n{"env":"staging"}\r\n
```

**Success response:**

```
%2\r\n
+status\r\n+OK\r\n
+version\r\n:1\r\n
```

| Field     | Type    | Description                         |
|-----------|---------|-------------------------------------|
| `status`  | string  | `"OK"`                              |
| `version` | integer | The version number that was written |

**Errors:**

| Code                | Condition                                           |
|---------------------|-----------------------------------------------------|
| `BAD_ARG`           | Missing namespace or key                            |
| `NAMESPACE_NOT_FOUND` | Namespace does not exist                          |
| `VALIDATION_FAILED` | Metadata failed namespace schema validation         |
| `DENIED`            | Insufficient permissions                            |

### 4.2 GET

Retrieve the value stored at a key.

**Request:**

```
GET <namespace> <key> [VERSION <n>] [META]
```

`VERSION` retrieves a specific historical version instead of the latest. The `META` flag includes metadata in the response.

```
*3\r\n
$3\r\nGET\r\n
$7\r\ndefault\r\n
$5\r\nmykey\r\n
```

With version and metadata:

```
*6\r\n
$3\r\nGET\r\n
$7\r\ndefault\r\n
$5\r\nmykey\r\n
$7\r\nVERSION\r\n
$1\r\n2\r\n
$4\r\nMETA\r\n
```

**Success response:**

```
%4\r\n
+status\r\n+OK\r\n
+key\r\n$5\r\nmykey\r\n
+value\r\n$11\r\nhello world\r\n
+version\r\n:3\r\n
```

When `META` is requested and metadata exists:

```
%5\r\n
+status\r\n+OK\r\n
+key\r\n$5\r\nmykey\r\n
+value\r\n$11\r\nhello world\r\n
+version\r\n:3\r\n
+metadata\r\n$16\r\n{"env":"staging"}\r\n
```

| Field      | Type    | Description                              |
|------------|---------|------------------------------------------|
| `status`   | string  | `"OK"`                                   |
| `key`      | bytes   | The key                                  |
| `value`    | bytes   | The stored value                         |
| `version`  | integer | Version number                           |
| `metadata` | json    | Metadata object (only if `META` flag set)|

**Errors:**

| Code                 | Condition                                |
|----------------------|------------------------------------------|
| `BAD_ARG`            | Missing namespace or key                 |
| `NOT_FOUND`          | Key does not exist                       |
| `VERSION_NOT_FOUND`  | Requested version does not exist         |
| `NAMESPACE_NOT_FOUND`| Namespace does not exist                 |
| `DENIED`             | Insufficient permissions                 |

### 4.3 DELETE

Delete a key by writing a tombstone version. The key is no longer returned by GET or LIST, but its version history is retained.

**Request:**

```
DELETE <namespace> <key>
```

```
*3\r\n
$6\r\nDELETE\r\n
$7\r\ndefault\r\n
$5\r\nmykey\r\n
```

**Success response:**

```
%2\r\n
+status\r\n+OK\r\n
+version\r\n:4\r\n
```

| Field     | Type    | Description                            |
|-----------|---------|----------------------------------------|
| `status`  | string  | `"OK"`                                 |
| `version` | integer | The tombstone version number           |

**Errors:**

| Code                 | Condition                        |
|----------------------|----------------------------------|
| `BAD_ARG`            | Missing namespace or key         |
| `NOT_FOUND`          | Key does not exist               |
| `NAMESPACE_NOT_FOUND`| Namespace does not exist         |
| `DENIED`             | Insufficient permissions         |

### 4.4 LIST

List active keys in a namespace with optional prefix filtering and cursor-based pagination.

**Request:**

```
LIST <namespace> [PREFIX <prefix>] [CURSOR <cursor>] [LIMIT <n>]
```

Default `LIMIT` is 100.

```
*2\r\n
$4\r\nLIST\r\n
$7\r\ndefault\r\n
```

With prefix and limit:

```
*6\r\n
$4\r\nLIST\r\n
$7\r\ndefault\r\n
$6\r\nPREFIX\r\n
$5\r\nuser:\r\n
$5\r\nLIMIT\r\n
$2\r\n50\r\n
```

**Success response:**

```
%3\r\n
+status\r\n+OK\r\n
+keys\r\n*3\r\n$6\r\nuser:1\r\n$6\r\nuser:2\r\n$6\r\nuser:3\r\n
+cursor\r\n$8\r\nabc12345\r\n
```

When the scan is complete, `cursor` is null:

```
+cursor\r\n_\r\n
```

| Field    | Type          | Description                                   |
|----------|---------------|-----------------------------------------------|
| `status` | string        | `"OK"`                                        |
| `keys`   | array<bytes>  | Array of key names                            |
| `cursor` | string / null | Opaque cursor for next page, null when done   |

**Errors:**

| Code                 | Condition                |
|----------------------|--------------------------|
| `BAD_ARG`            | Missing namespace        |
| `NAMESPACE_NOT_FOUND`| Namespace does not exist |
| `DENIED`             | Insufficient permissions |

### 4.5 VERSIONS

Retrieve version history for a key, most recent first.

**Request:**

```
VERSIONS <namespace> <key> [LIMIT <n>] [FROM <version>]
```

Default `LIMIT` is 100. `FROM` starts the listing from a specific version number (descending).

```
*3\r\n
$8\r\nVERSIONS\r\n
$7\r\ndefault\r\n
$5\r\nmykey\r\n
```

**Success response:**

```
%2\r\n
+status\r\n+OK\r\n
+versions\r\n*2\r\n
  %4\r\n+version\r\n:3\r\n+state\r\n+active\r\n+updated_at\r\n:1711152000\r\n+actor\r\n+admin-1\r\n
  %4\r\n+version\r\n:2\r\n+state\r\n+active\r\n+updated_at\r\n:1711148400\r\n+actor\r\n+admin-1\r\n
```

Each entry in the `versions` array is a map:

| Field        | Type    | Description                                  |
|--------------|---------|----------------------------------------------|
| `version`    | integer | Version number                               |
| `state`      | string  | `"active"` or `"deleted"`                    |
| `updated_at` | integer | Unix timestamp when this version was written |
| `actor`      | string  | Authenticated principal who wrote the version|

**Errors:**

| Code                 | Condition                        |
|----------------------|----------------------------------|
| `BAD_ARG`            | Missing namespace or key         |
| `NOT_FOUND`          | Key does not exist               |
| `NAMESPACE_NOT_FOUND`| Namespace does not exist         |
| `DENIED`             | Insufficient permissions         |

---

## 5. Namespace Commands

All namespace commands are subcommands of the `NAMESPACE` verb.

### 5.1 NAMESPACE CREATE

Create a new namespace.

**Request:**

```
NAMESPACE CREATE <name> [SCHEMA <json>] [MAX_VERSIONS <n>] [TOMBSTONE_RETENTION <seconds>]
```

`SCHEMA` is a JSON object defining the metadata schema enforced on PUT. `MAX_VERSIONS` caps version history per key. `TOMBSTONE_RETENTION` controls how long tombstones are retained before compaction (in seconds).

```
*4\r\n
$9\r\nNAMESPACE\r\n
$6\r\nCREATE\r\n
$8\r\nsessions\r\n
$12\r\nMAX_VERSIONS\r\n
$2\r\n10\r\n
```

**Success response:**

```
%1\r\n
+status\r\n+OK\r\n
```

**Errors:**

| Code               | Condition                          |
|--------------------|------------------------------------|
| `BAD_ARG`          | Missing name or invalid parameters |
| `NAMESPACE_EXISTS` | Namespace already exists           |
| `DENIED`           | Insufficient permissions (admin)   |

### 5.2 NAMESPACE DROP

Drop a namespace.

**Request:**

```
NAMESPACE DROP <name> [FORCE]
```

Without `FORCE`, the namespace must be empty (no keys). With `FORCE`, all keys are deleted along with the namespace.

```
*3\r\n
$9\r\nNAMESPACE\r\n
$4\r\nDROP\r\n
$8\r\nsessions\r\n
```

**Success response:**

```
%1\r\n
+status\r\n+OK\r\n
```

**Errors:**

| Code                  | Condition                                        |
|-----------------------|--------------------------------------------------|
| `BAD_ARG`             | Missing name                                     |
| `NAMESPACE_NOT_FOUND` | Namespace does not exist                         |
| `NAMESPACE_NOT_EMPTY` | Namespace contains keys (use FORCE to override)  |
| `DENIED`              | Insufficient permissions (admin)                 |

### 5.3 NAMESPACE LIST

List namespaces visible to the authenticated token.

**Request:**

```
NAMESPACE LIST [CURSOR <cursor>] [LIMIT <n>]
```

Default `LIMIT` is 100.

```
*2\r\n
$9\r\nNAMESPACE\r\n
$4\r\nLIST\r\n
```

**Success response:**

```
%3\r\n
+status\r\n+OK\r\n
+namespaces\r\n*2\r\n+default\r\n+sessions\r\n
+cursor\r\n_\r\n
```

| Field        | Type           | Description                                 |
|--------------|----------------|---------------------------------------------|
| `status`     | string         | `"OK"`                                      |
| `namespaces` | array<string>  | Namespace names                             |
| `cursor`     | string / null  | Opaque cursor for next page, null when done |

**Errors:**

| Code     | Condition                |
|----------|--------------------------|
| `DENIED` | Insufficient permissions |

### 5.4 NAMESPACE INFO

Get metadata about a namespace.

**Request:**

```
NAMESPACE INFO <name>
```

```
*3\r\n
$9\r\nNAMESPACE\r\n
$4\r\nINFO\r\n
$7\r\ndefault\r\n
```

**Success response:**

```
%4\r\n
+status\r\n+OK\r\n
+name\r\n+default\r\n
+key_count\r\n:1234\r\n
+created_at\r\n:1711065600\r\n
```

| Field        | Type    | Description                       |
|--------------|---------|-----------------------------------|
| `status`     | string  | `"OK"`                            |
| `name`       | string  | Namespace name                    |
| `key_count`  | integer | Number of active keys             |
| `created_at` | integer | Unix timestamp of creation        |

**Errors:**

| Code                 | Condition                |
|----------------------|--------------------------|
| `BAD_ARG`            | Missing name             |
| `NAMESPACE_NOT_FOUND`| Namespace does not exist |
| `DENIED`             | Insufficient permissions |

### 5.5 NAMESPACE ALTER

Update namespace configuration. Changes are enforced on new writes only; existing data is not retroactively validated.

**Request:**

```
NAMESPACE ALTER <name> [SCHEMA <json>] [MAX_VERSIONS <n>] [TOMBSTONE_RETENTION <seconds>]
```

```
*5\r\n
$9\r\nNAMESPACE\r\n
$5\r\nALTER\r\n
$7\r\ndefault\r\n
$12\r\nMAX_VERSIONS\r\n
$1\r\n5\r\n
```

**Success response:**

```
%1\r\n
+status\r\n+OK\r\n
```

**Errors:**

| Code                 | Condition                          |
|----------------------|------------------------------------|
| `BAD_ARG`            | Missing name or invalid parameters |
| `NAMESPACE_NOT_FOUND`| Namespace does not exist           |
| `DENIED`             | Insufficient permissions (admin)   |

### 5.6 NAMESPACE VALIDATE

Check all existing entries in a namespace against its current metadata schema.

**Request:**

```
NAMESPACE VALIDATE <name>
```

```
*3\r\n
$9\r\nNAMESPACE\r\n
$8\r\nVALIDATE\r\n
$7\r\ndefault\r\n
```

**Success response:**

```
%3\r\n
+status\r\n+OK\r\n
+count\r\n:2\r\n
+reports\r\n*2\r\n
  %3\r\n+key\r\n$6\r\nuser:1\r\n+version\r\n:1\r\n+errors\r\n*1\r\n+missing required field: env\r\n
  %3\r\n+key\r\n$6\r\nuser:3\r\n+version\r\n:2\r\n+errors\r\n*1\r\n+invalid type for field: count\r\n
```

| Field     | Type  | Description                                         |
|-----------|-------|-----------------------------------------------------|
| `status`  | string| `"OK"`                                              |
| `count`   | integer| Number of entries with validation errors            |
| `reports` | array | Array of maps, each with `key`, `version`, `errors` |

Each report entry:

| Field    | Type          | Description                  |
|----------|---------------|------------------------------|
| `key`    | bytes         | The key that failed          |
| `version`| integer       | Version that was checked     |
| `errors` | array<string> | List of validation failures  |

**Errors:**

| Code                 | Condition                |
|----------------------|--------------------------|
| `BAD_ARG`            | Missing name             |
| `NAMESPACE_NOT_FOUND`| Namespace does not exist |
| `DENIED`             | Insufficient permissions |

---

## 6. Pipeline Semantics

Pipelines batch multiple commands into a single atomic unit. All commands succeed or all are rolled back.

### PIPELINE

**Request:**

```
PIPELINE <count>
<command 1>
<command 2>
...
```

The `count` parameter declares how many commands follow. Each subsequent command is a standard RESP3 array (as if sent independently). Clients should also include a `REQUEST_ID` in each pipelined command for correlation.

```
*2\r\n
$8\r\nPIPELINE\r\n
$1\r\n3\r\n
*4\r\n$3\r\nPUT\r\n$7\r\ndefault\r\n$2\r\nk1\r\n$2\r\nv1\r\n
*4\r\n$3\r\nPUT\r\n$7\r\ndefault\r\n$2\r\nk2\r\n$2\r\nv2\r\n
*3\r\n$6\r\nDELETE\r\n$7\r\ndefault\r\n$2\r\nk3\r\n
```

**Success response:**

An Array of individual command responses, one per command, in order:

```
*3\r\n
  %2\r\n+status\r\n+OK\r\n+version\r\n:1\r\n
  %2\r\n+status\r\n+OK\r\n+version\r\n:1\r\n
  %2\r\n+status\r\n+OK\r\n+version\r\n:5\r\n
```

**Error behavior:**

If any command in the pipeline fails, the entire pipeline is rolled back and the server returns:

```
-PIPELINE_ABORTED pipeline command failed, all commands rolled back\r\n
```

**Errors:**

| Code               | Condition                                          |
|--------------------|----------------------------------------------------|
| `BAD_ARG`          | Missing count or count is zero                     |
| `PIPELINE_ABORTED` | A command in the pipeline failed; all rolled back  |

---

## 7. Pub/Sub: Push Frames

### 7.1 SUBSCRIBE

Subscribe to change events on a namespace. The connection enters subscription mode and receives push frames as events occur.

**Request:**

```
SUBSCRIBE <namespace> [KEY <key>] [EVENTS <type> [<type> ...]]
```

`KEY` filters events to a single key. `EVENTS` filters by event type: `PUT`, `DELETE`, or `*` for all (default).

```
*2\r\n
$9\r\nSUBSCRIBE\r\n
$7\r\ndefault\r\n
```

With filters:

```
*6\r\n
$9\r\nSUBSCRIBE\r\n
$7\r\ndefault\r\n
$3\r\nKEY\r\n
$5\r\nmykey\r\n
$6\r\nEVENTS\r\n
$3\r\nPUT\r\n
```

**Confirmation response:**

```
%1\r\n
+status\r\n+OK\r\n
```

**Push frames:**

Events are delivered as RESP3 Push frames (`>` prefix):

```
>4\r\n
+message\r\n
+default\r\n
+mykey\r\n
+PUT\r\n
```

| Position | Value     | Description            |
|----------|-----------|------------------------|
| 0        | `message` | Frame type identifier  |
| 1        | namespace | Source namespace        |
| 2        | key       | Affected key           |
| 3        | event     | Event type: PUT, DELETE |

Push frames are out-of-band: they can arrive interleaved with responses to other commands on the same connection.

**Errors:**

| Code                 | Condition                |
|----------------------|--------------------------|
| `BAD_ARG`            | Missing namespace        |
| `NAMESPACE_NOT_FOUND`| Namespace does not exist |
| `DENIED`             | Insufficient permissions |

### 7.2 UNSUBSCRIBE

End the current subscription and exit subscription mode.

**Request:**

```
*1\r\n
$11\r\nUNSUBSCRIBE\r\n
```

**Success response:**

```
%1\r\n
+status\r\n+OK\r\n
```

---

## 8. Operational Commands

### 8.1 PING

Test connectivity.

**Request:**

```
*1\r\n
$4\r\nPING\r\n
```

**Success response:**

```
%2\r\n
+status\r\n+OK\r\n
+message\r\n+PONG\r\n
```

### 8.2 HEALTH

Check server health. Does not require authentication.

**Request:**

```
*1\r\n
$6\r\nHEALTH\r\n
```

**Success response:**

```
%2\r\n
+status\r\n+OK\r\n
+message\r\n+healthy\r\n
```

**Errors:**

| Code        | Condition                    |
|-------------|------------------------------|
| `NOT_READY` | Server is not in READY state |

### 8.3 CONFIG GET

Read a runtime configuration value.

**Request:**

```
CONFIG GET <key>
```

```
*3\r\n
$6\r\nCONFIG\r\n
$3\r\nGET\r\n
$15\r\nmax_connections\r\n
```

**Success response:**

```
%3\r\n
+status\r\n+OK\r\n
+key\r\n+max_connections\r\n
+value\r\n+1024\r\n
```

| Field    | Type          | Description          |
|----------|---------------|----------------------|
| `status` | string        | `"OK"`               |
| `key`    | string        | Configuration key    |
| `value`  | string / null | Current value        |

**Errors:**

| Code       | Condition                |
|------------|--------------------------|
| `BAD_ARG`  | Missing key              |
| `DENIED`   | Insufficient permissions |

### 8.4 CONFIG SET

Set a runtime configuration value.

**Request:**

```
CONFIG SET <key> <value>
```

```
*4\r\n
$6\r\nCONFIG\r\n
$3\r\nSET\r\n
$9\r\nlog_level\r\n
$5\r\ndebug\r\n
```

**Success response:**

```
%1\r\n
+status\r\n+OK\r\n
```

**Errors:**

| Code       | Condition                          |
|------------|------------------------------------|
| `BAD_ARG`  | Missing key or value, invalid type |
| `DENIED`   | Insufficient permissions (admin)   |

### 8.5 COMMAND LIST

List all commands supported by the server.

**Request:**

```
*2\r\n
$7\r\nCOMMAND\r\n
$4\r\nLIST\r\n
```

**Success response:**

```
%3\r\n
+status\r\n+OK\r\n
+count\r\n:16\r\n
+commands\r\n*16\r\n+PUT\r\n+GET\r\n+DELETE\r\n...
```

| Field      | Type          | Description              |
|------------|---------------|--------------------------|
| `status`   | string        | `"OK"`                   |
| `count`    | integer       | Number of commands       |
| `commands` | array<string> | List of command names    |

---

## 9. Error Code Reference

| Code                  | HTTP Equiv | Description                                              |
|-----------------------|------------|----------------------------------------------------------|
| `BAD_ARG`             | 400        | Missing or malformed command argument                    |
| `DENIED`              | 401        | Authentication required or insufficient permissions      |
| `NOT_AUTHENTICATED`   | 401        | No auth token provided on this connection                |
| `NOT_FOUND`           | 404        | Key or resource does not exist                           |
| `NAMESPACE_NOT_FOUND` | 404        | Namespace does not exist                                 |
| `VERSION_NOT_FOUND`   | 404        | Requested version does not exist                         |
| `NAMESPACE_EXISTS`    | 409        | Namespace already exists                                 |
| `NAMESPACE_NOT_EMPTY` | 409        | Namespace is not empty (use FORCE to override)           |
| `PIPELINE_ABORTED`    | 409        | Pipeline command failed, all commands rolled back        |
| `VALIDATION_FAILED`   | 422        | Metadata validation failed against namespace schema      |
| `NOT_READY`           | 503        | Server is not in READY state                             |

All errors are returned as RESP3 Simple Error strings:

```
-CODE human-readable message\r\n
```

---

## 10. Command Summary

| Command              | ACL   | Description                                       |
|----------------------|-------|---------------------------------------------------|
| `AUTH`               | none  | Authenticate the connection                       |
| `PING`               | none  | Test connectivity                                 |
| `HEALTH`             | none  | Check server health                               |
| `PUT`                | write | Store a value at a key                            |
| `GET`                | read  | Retrieve a value by key                           |
| `DELETE`             | write | Delete a key (tombstone)                          |
| `LIST`               | read  | List keys in a namespace                          |
| `VERSIONS`           | read  | Retrieve version history for a key                |
| `NAMESPACE CREATE`   | admin | Create a namespace                                |
| `NAMESPACE DROP`     | admin | Drop a namespace                                  |
| `NAMESPACE LIST`     | none  | List namespaces (filtered by token grants)        |
| `NAMESPACE INFO`     | read  | Get namespace metadata                            |
| `NAMESPACE ALTER`    | admin | Update namespace configuration                    |
| `NAMESPACE VALIDATE` | read  | Validate existing entries against metadata schema |
| `PIPELINE`           | none  | Execute commands atomically                       |
| `SUBSCRIBE`          | read  | Subscribe to change events                        |
| `UNSUBSCRIBE`        | none  | End a subscription                                |
| `CONFIG GET`         | none  | Read a runtime config value                       |
| `CONFIG SET`         | admin | Set a runtime config value                        |
| `COMMAND LIST`       | none  | List supported commands                           |
