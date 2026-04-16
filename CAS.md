# CAS, TTL, and Prefix-Delete — v2 Plan

Three primitives added to the Store trait so embedders can build coordination patterns (locks, leases, claim queues, idempotency, rate limits, expiring state, bulk cleanup) without reinventing them against a non-atomic surface.

Scope is titled **CAS** because compare-and-swap is the only one of the three that closes a correctness gap — without it, concurrent writers silently lose updates. TTL and prefix-delete are ergonomic primitives that belong at the storage layer because every embedder otherwise writes the same code against the same race conditions.

---

## 1. Primitive: Compare-and-Swap

### Semantics

- `put_if_version(ns, key, value, metadata, expected_version) -> Result<u64, StoreError>`
- `delete_if_version(ns, key, expected_version) -> Result<u64, StoreError>`

The write succeeds only if the current version of the key matches `expected_version`. On mismatch, returns `StoreError::VersionConflict { current }`. On success, returns the new version.

### Expected-version values

| `expected_version` | Meaning |
|---|---|
| `0` | Key must not exist (or must be tombstoned at any version). Insert-only semantics. |
| `N > 0` | Key's current active version must equal `N`. Strict update semantics. |

Resurrection: `put_if_version(key, expected=5)` where version 5 is a tombstone succeeds and writes version 6. Same lineage as a normal put against a tombstoned key.

### Error variant

```rust
pub enum StoreError {
    // ... existing variants
    VersionConflict { current: u64 },
}
```

`current` is returned so the caller can decide retry vs abort without a second roundtrip.

### Trait changes

Additive. Existing `put` / `delete` signatures unchanged.

```rust
fn put_if_version(
    &self,
    ns: &str,
    key: &[u8],
    value: &[u8],
    metadata: Option<Metadata>,
    expected_version: u64,
) -> impl Future<Output = Result<u64, StoreError>> + Send;

fn delete_if_version(
    &self,
    ns: &str,
    key: &[u8],
    expected_version: u64,
) -> impl Future<Output = Result<u64, StoreError>> + Send;
```

### WAL

No new entry shape. CAS is a primary-side gate: the version check happens under the namespace write lock *before* the WAL entry is appended. A successful CAS produces a normal `Put` or `Delete` WAL entry. Replicas replay without re-checking — by the time an entry is on the wire, the primary already validated the precondition.

### Protocol (RESP3)

- `PUTIF ns key value metadata? expected_version` → `{version: u64}` or error
- `DELIF ns key expected_version` → `{version: u64}` or error
- New error code: `VERSIONCONFLICT` carrying `current` in the error map

`protocol.toml` updated; SDK clients regenerated.

### CLI

- `shroudb put --if-version N ns key value`
- `shroudb delete --if-version N ns key`

### Pipeline integration

`PipelineCommand::PutIfVersion { ... }` and `PipelineCommand::DeleteIfVersion { ... }` variants. A pipeline aborts on the first conflict and reports which step failed (preserves existing all-or-nothing semantics).

### Replication

Trivial. A successful CAS produces a standard Put/Delete WAL entry. Replicas replay in order, apply unconditionally. A CAS that the primary rejected never reaches the WAL and never ships.

### Tests

- Unit: version match path, mismatch path, resurrection from tombstone, `expected=0` against existing key rejects, `expected=0` against non-existent succeeds.
- Integration: two concurrent clients racing on the same key — one wins, one gets `VersionConflict { current }`; retry loop converges.
- Pipeline: CAS inside batch aborts correctly on mid-batch conflict.
- Replay: WAL replay after crash produces identical state regardless of which CAS call "won."

### Security posture

Fail-closed. On conflict, no write occurs and the caller receives an error. Versions are monotonic — no downgrade path. No new surface for timing attacks (version comparison is O(1) integer equality).

---

## 2. Primitive: Server-side TTL

### Semantics

- `put(..., ttl: Option<Duration>)` — entry is automatically deleted at or after `now + ttl`.
- `get` on an expired-but-not-yet-swept entry returns `NotFound`. The storage layer never returns expired data to callers.
- `list` filters out expired entries.
- `versions` preserves history up through tombstone compaction as usual.

### API shape

Introduce a `PutOptions` struct to absorb TTL + CAS without a combinatorial explosion of method variants:

```rust
#[derive(Debug, Clone, Default)]
pub struct PutOptions {
    pub metadata: Option<Metadata>,
    pub ttl: Option<Duration>,
    pub expected_version: Option<u64>,
}

fn put_with_options(
    &self,
    ns: &str,
    key: &[u8],
    value: &[u8],
    options: PutOptions,
) -> impl Future<Output = Result<u64, StoreError>> + Send;
```

Existing `put`, `put_if_version` delegate to `put_with_options` with the appropriate defaults.

### Storage

`expires_at_ms: Option<u64>` becomes a field on the encrypted entry record, alongside value and metadata. Same encryption envelope — expiry time is never in plaintext at rest.

### Read-path expiry check

`get`, `list`, `versions` (for the active entry) check `expires_at_ms <= now_ms()` on the server, using server wall-clock. Client-supplied timestamps are never trusted for expiry decisions.

If expired and the entry has not yet been swept: behave as if it were already deleted (return `NotFound` / omit from listings). Do not synchronously issue a tombstone write — the sweeper handles physical deletion.

### Sweeper

One sweeper per namespace. In-memory structure:

- Min-heap keyed by `expires_at_ms`, values are `(ns, key, version)` tuples.
- On `put_with_options(..., ttl)`: insert into both main index and TTL heap.
- On sweep tick (configurable interval, default 1s): pop all entries with `expires_at_ms <= now`, issue a normal `Delete` WAL entry for each. Under the namespace write lock so concurrent writers see a consistent tombstone.

Heap memory budget: `~64 bytes per TTL'd key`. 1M TTL'd keys ≈ 64MB. If TTL'd-key count exceeds a configurable ceiling, sweep falls back to periodic scan (slower, bounded memory).

Bounded-index interaction (V2_PLANS Phase 1): the TTL heap holds ALL TTL'd keys in memory even for entries evicted from the LRU. This is fine — the heap entry is a tiny tuple, not the value. The sweeper's `Delete` lookup goes through the normal get-or-load path.

### Replication

Sweeper runs on primary only. It emits standard `Delete` WAL entries that ship to replicas like any other delete. Replicas never run their own sweeper (would produce duplicate tombstones and clock-drift divergence).

Replica read-path: also honors `expires_at_ms` on read, so a replica returns `NotFound` for an expired entry even before the primary's sweeper has issued the tombstone. No new replication lag window — correctness is tied to the entry's own expiry field, not to the sweep event.

### Version history

An expired+swept entry's version history is retained per normal tombstone compaction rules. No new GC semantics.

### Config

```toml
[store.ttl]
sweep_interval_ms = 1000
max_heap_entries = 10_000_000   # fallback to scan above this
```

### Protocol

- `PUT` command gains optional `ttl` field (milliseconds). `protocol.toml` updated.
- No new command; TTL is a modifier on existing put.

### CLI

- `shroudb put --ttl 60s ns key value`

### Tests

- Unit: expiry boundary (at `expires_at_ms`, 1ms before, 1ms after), sweep emits correct tombstone, heap ordering under random insert order.
- Integration: put with TTL → immediate get succeeds → wait past expiry → get returns NotFound without sweep → sweep tick → WAL contains tombstone.
- Replication: primary sweeps, replica converges; replica read during lag window also returns NotFound.
- Recovery: crash mid-sweep, restart, TTL heap rebuilt from WAL replay, remaining expired entries swept.
- Clock skew: system clock jumps backward — sweeper doesn't re-resurrect already-deleted entries (tombstones are version-monotonic).

### Security posture

- Expiry time is encrypted at rest (part of the entry record).
- Read-path expiry check runs on server with server clock. Client cannot forge a "not expired yet" response.
- Expiry does not leak via listings, versions output, or error messages — an expired key is indistinguishable from a never-existed key at the API surface until the sweep emits a tombstone (at which point it's indistinguishable from an explicit delete).
- TTL cannot be used to bypass ACL — the sweeper's implicit delete goes through the same ACL as an explicit delete (the tenant-of-record is the entry's original writer). This needs a dedicated test.

---

## 3. Primitive: Prefix-Delete

### Semantics

- `delete_prefix(ns, prefix) -> Result<u64, StoreError>` — deletes all active keys in `ns` whose byte representation starts with `prefix`. Returns the count of keys deleted.
- Atomic from the outside: under the namespace write lock for the duration of the operation. Callers see either "before" state or "after" state, never partial.

### Trait change

```rust
fn delete_prefix(
    &self,
    ns: &str,
    prefix: &[u8],
) -> impl Future<Output = Result<u64, StoreError>> + Send;
```

### WAL

Option: primary scans matching keys under the namespace write lock and writes N individual `Delete` entries as a single atomic batch append. Replicas replay N deletes in order. Simpler than a new `DeletePrefix` entry type; replication stays compositional.

Rationale: prefix-deletes are rare events (tenant teardown, bulk cleanup). WAL amplification is acceptable. Revisit if volume ever justifies a compact `DeletePrefix` entry.

### Bounds

Configurable upper limit on keys affected per call — default 100,000. Exceeding the cap returns `StoreError::PrefixTooLarge { matched: u64, limit: u64 }`. Caller paginates by refining the prefix.

```toml
[store]
delete_prefix_max_keys = 100_000
```

### Protocol

- `DELPREFIX ns prefix` → `{deleted: u64}`
- `protocol.toml` updated; SDK clients regenerated.

### CLI

- `shroudb delete-prefix ns prefix`

### ACL

The caller must hold delete permission on the namespace. Check applied once, before materialization. No per-key ACL check — prefix-delete is a namespace-scoped op.

### Replication

Primary writes N tombstones in a single batched WAL append. Replicas apply the batch atomically (same batch framing as `pipeline` today). No new replication semantics.

### Tests

- Unit: exact prefix match, no match, prefix matching all keys, empty prefix rejected (safety).
- Integration: concurrent writer inserting matching keys during delete — either included in the delete or written after (no partial state visible).
- Replay: WAL contains batched deletes; replay produces identical tombstoned state.
- Limit: prefix matching more than `delete_prefix_max_keys` returns the error, no partial deletion.
- ACL: caller without delete permission rejected before any keys are touched.

### Security posture

- Fail-closed on ACL check.
- Cap prevents a single malicious or buggy call from issuing unbounded WAL writes.
- Empty prefix explicitly rejected (would match all keys — force caller to type it out with `delete-prefix ns ""` if they really mean it; separate flag `--all` required).

---

## 4. Cross-cutting changes

### Files touched

| File | Change |
|---|---|
| `commons/shroudb-store/src/store.rs` | Add `put_if_version`, `delete_if_version`, `put_with_options`, `delete_prefix`, `PutOptions` |
| `commons/shroudb-store/src/error.rs` | Add `VersionConflict`, `PrefixTooLarge` |
| `commons/shroudb-store/src/entry.rs` | Add `expires_at_ms: Option<u64>` field |
| `shroudb/shroudb-protocol/src/command.rs` | Add `PutIf`, `DelIf`, `DelPrefix` command variants; TTL field on `Put` |
| `shroudb/shroudb-protocol/src/handlers/` | New `put_if.rs`, `del_if.rs`, `del_prefix.rs`; extend `put.rs` for TTL |
| `shroudb/shroudb-protocol/src/dispatch.rs` | Route new commands; classify behavior (all three are `WriteOnly`) |
| `shroudb/shroudb-server/src/storage/` | Wire CAS gate under namespace write lock; TTL min-heap + sweep timer; prefix scan + batch WAL append |
| `shroudb/shroudb-cli/src/main.rs` | `--if-version`, `--ttl`, `delete-prefix` subcommand |
| `shroudb/shroudb-client/src/remote_store.rs` | Client-side bindings for new methods |
| `shroudb/protocol.toml` | New commands + TTL field |
| `shroudb/DOCS.md`, `README.md`, `ABOUT.md`, `PROTOCOL.md` | User-facing documentation |
| `shroudb/CHANGELOG.md` | v2 entry |

### Downstream repos

Per CLAUDE.md rule 9: any downstream repo consuming the Store trait must be updated in the same effort.

- **shroudb-codegen** — regenerate SDK clients from `protocol.toml`.
- **Engines on the v0.2+ Store trait** — no breaking changes (additive only), but bump their `shroudb-store` dep to pick up the new methods. Engines that want CAS/TTL/prefix-delete adopt opportunistically.
- **shroudb-moat** — same as engines.

### Fuzz targets

- WAL round-trip with TTL field populated.
- Pipeline with interleaved `PutIfVersion` / `DeleteIfVersion` operations.
- Prefix-delete with adversarial prefixes (empty, binary, UTF-8 edge cases, prefix longer than any key).

---

## 5. Delivery order

All three ship together in v2 to amortize the Store-trait / protocol.toml / SDK-regen cycle. Within the work:

1. **CAS first** — smallest surface, closes the correctness gap, unblocks everything else. `put_if_version`, `delete_if_version`, `VersionConflict`, protocol + CLI + tests.
2. **`PutOptions` refactor** — introduce the struct, migrate `put` / `put_if_version` through it. Sets up TTL.
3. **TTL** — `expires_at_ms` on entry, read-path check, min-heap sweeper, replication test, security test.
4. **Prefix-delete** — trait method, server-side scan under write lock, batched WAL append, cap + error variant.
5. **Docs + changelog + regen SDKs** — simultaneous with the final PR; CLAUDE.md rules 7–9 require docs land in the same changeset as code.

Pre-push checklist (CLAUDE.md) applies as normal: `cargo fmt`, `cargo clippy -D warnings`, `cargo test --workspace`, `cargo deny check` before any push.

---

## 6. Open questions

Called out explicitly so they're not silently decided during implementation:

1. **`PutOptions` vs. separate methods.** Proposed: `PutOptions`. Alternative: keep `put_if_version` and add `put_with_ttl` and `put_with_ttl_if_version` as separate methods. Trait stays flatter but combinatorial.
2. **TTL sweep interval default.** Proposed: 1s. Tradeoff between sweep lag (space bloat) and sweep cost (CPU + WAL writes under load).
3. **Prefix-delete cap.** Proposed: 100,000 keys, configurable. Alternative: unbounded with a cursor-paginated `delete_prefix_paged` variant.
4. **`expected_version = 0` semantics on tombstoned key.** Proposed: succeeds (treats tombstoned as "not existing"). Alternative: require explicit `expected = <tombstone_version>`. Affects insert-or-update patterns.
5. **TTL on `put_if_version`.** Proposed: allowed via `PutOptions { ttl, expected_version }`. No known concern, noted for review.

### Resolutions (2026-04-16)

| # | Resolution |
|---|---|
| 1 | `PutOptions`. Refactor lands in step 2 of delivery. |
| 2 | `sweep_interval_ms = 1000` default, configurable. |
| 3 | `delete_prefix_max_keys = 100_000` default, configurable. Scan fallback ships in step 3 (not deferred). |
| 4 | `expected_version = 0` on a tombstoned key **succeeds**. Pinned in a named unit test in step 1 (`cas_expected_zero_on_tombstone_succeeds`) so behavior is not reversible without a visible test change. |
| 5 | Allowed. A CAS that does not specify `ttl` writes a **TTL-less entry** — it does **not** inherit the overwritten version's TTL. Pinned by `cas_does_not_inherit_ttl` in step 3. |

---

## 7. Deferred / follow-up

Anything skipped during v2 implementation lands here so it stays tracked. Do not silently adopt any of these — each needs its own effort.

### 7.1 WAL `EntryPut` v1 variant removal

In step 3, TTL is added via a new `WalPayload::EntryPutV2` variant carrying `expires_at_ms`. The legacy `EntryPut` variant remains decodable for replay compatibility — old WAL segments and snapshots keep working without on-disk migration.

**Follow-up:** a future major release removes `WalPayload::EntryPut` entirely. Precondition: a `migrate-ttl` subcommand in `commons/shroudb-wal-tool` that rewrites v1 `EntryPut` entries to `EntryPutV2` with `expires_at_ms = None`. Operators run the tool; then the variant is removed. Do not remove the variant before the tool ships and is documented as a required upgrade step.

### 7.2 Structured RESP3 Map-frame errors

Step 0 encodes `VersionConflict { current }` and `PrefixTooLarge { matched, limit }` as SimpleError strings: `VERSIONCONFLICT current=5`, `PREFIXTOOLARGE matched=N limit=M`. This matches the existing error surface (all handlers return `SimpleError`).

**Follow-up:** a separate effort can move RESP3 errors to Map frames for typed consumer access. It touches every handler and every SDK's error parser — scope it on its own, not bundled into v2.

### 7.3 Opportunistic engine adoption

Engine repos get mechanical `shroudb-store 0.2.0` dep bumps in step 5. Actual use of the new primitives by engines lands as follow-up PRs post-merge. Candidates with strong fit:
- **`shroudb-keep`** — CAS on credential rotation writes (closes a correctness gap in concurrent rotation).
- **`shroudb-sentry`** — TTL on audit-log retention (replaces ad-hoc time-windowed compaction).
- **`shroudb-stash`** — TTL on ephemeral blob references.

Each is its own PR after the v2 server change merges. Do not bundle.

### 7.4 `PipelineResult::VersionConflict` non-fatal variant

Step 1 ships CAS inside pipelines using the existing `StoreError::PipelineAborted` path: the first conflict aborts the whole batch and reports the failing step. This preserves current all-or-nothing semantics.

**Follow-up:** if callers need per-step conflict tolerance (e.g., batch with "try CAS on 10 keys, apply the ones that matched"), add a `PipelineResult::VersionConflict { current }` variant that lets the batch proceed. Deferred until a real caller needs it — speculative design otherwise.

### 7.5 Pipeline same-key staging bug (pre-existing)

Surfaced while writing the ns-lock tests in step 0. A pipeline that issues multiple puts to the *same key* stages every put against the same pre-pipeline `current_version` — all four stage `version = pre + 1`, then the apply phase overwrites them in order and only the last wins (from the index's POV), but each returned `PipelineResult::Put(v)` carries the same value.

This is a **pre-existing bug**, not introduced by v2. The ns-lock added in step 0 prevents the cross-pipeline version collision; it does not fix the intra-pipeline same-key staging.

**Follow-up:** fix the staging loop to simulate a running version counter per `(ns, key)` within the pipeline, so `N` puts to the same key stage as `pre+1 .. pre+N`. Separate effort — not in v2 scope.

### 7.6 Prefix-delete cursor pagination

Step 4 caps `delete_prefix` at 100k keys (configurable) and returns `PrefixTooLarge` on breach. The caller refines the prefix and retries.

**Follow-up:** a cursor-paginated `delete_prefix_paged(ns, prefix, cursor) -> (deleted_count, next_cursor)` variant for tenant-teardown use cases where the caller cannot refine the prefix further. Not in v2 — the cap + error pattern is sufficient for the current use cases (bulk cleanup, tenant teardown at bounded cardinality).
