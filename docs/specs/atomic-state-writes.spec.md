---
id: atomic-state-writes
title: state.json is written atomically
status: active
owners: []
implements:
  - src/project.rs
depends_on: []
version: 1
---

## Intent
`state.json` is the single source of truth for spec hashes, file fingerprints,
and judgment verdicts. It is rewritten on every `verify` and judgment ingest.
A naive in-place write that is interrupted (crash, full disk, SIGKILL) can
leave the file truncated, which would silently lose every recorded verdict on
the next load. Writes must therefore be atomic: a reader either sees the old
complete file or the new complete file, never a partial one.

## Behavior
- {#b1} A state write serializes to a temporary file in the same directory, then renames it over the destination.
- {#b2} The rename replaces any existing `state.json` in a single filesystem operation.
- {#b3} On success no temporary or partial file is left behind in the state directory.
- {#b4} The state directory is created first if it does not yet exist.

## Examples
```example name="write then read round-trips"
Given a project with one recorded spec state
When the state is written and then loaded again
Then the loaded state equals what was written
```

```example name="no temp residue after write"
Given a project state directory
When the state is written
Then the state directory contains state.json and no leftover temporary file
```

```example name="overwrite preserves a valid file"
Given an existing valid state.json
When a new state is written over it
Then reading state.json afterwards yields valid, complete JSON
```

## Invariants
- {deterministic} After a successful write, the state directory contains exactly one regular file whose name is the configured state file.
- {deterministic} A load immediately following a write returns a state byte-equal in meaning to the one written (round-trips through serde).
- {judgment} The temporary file name is derived so two writers in the same directory are unlikely to collide on it.
