# Known Issues

## v1.0

### Namespace drop has a narrow TOCTOU window

When dropping a namespace without `FORCE`, the key count check and the WAL write are not atomic. A concurrent `PUT` between the check and the write could succeed but lose data when the drop completes. The window is small and the scenario (simultaneous PUT + non-force DROP on the same namespace) is a misuse pattern.

### LIST cursor pagination uses string comparison

Invalid cursors silently skip results instead of returning an error. The cursor value is opaque to clients — producing an invalid cursor requires deliberate manipulation.

### Replication snapshot frame not processed

Replica nodes that receive snapshot frames from the primary log a warning and ignore the payload. Bootstrap from snapshot is not functional — replicas must have access to the primary's full WAL history to sync.

### Config SET has no schema enforcement

`CONFIG SET` accepts any string value for any key. The `ConfigValueType::validate()` method exists but the config store has no schema registry to enforce types at write time.

## Resolved

See `V1_REMAINING.md` for the full list of resolved security and correctness issues from the v1 audit.
