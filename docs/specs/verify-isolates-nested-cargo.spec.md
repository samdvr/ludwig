---
id: verify-isolates-nested-cargo
title: verify isolates nested cargo runs
status: active
owners: []
implements:
  - src/adapters/rust.rs
depends_on: []
version: 1
---

## Intent
The Rust adapter runs `cargo test` as a subprocess. When Ludwig itself is
invoked from inside another cargo process — its own integration tests, or any
host that drives `ludwig` under `cargo run` — the nested `cargo test` contends
on the parent build's `target/` lock and can block indefinitely. To stay
deadlock-free the adapter must run the nested build against an isolated target
directory, while a plain top-level invocation keeps sharing the user's normal
target so verification stays fast.

## Behavior
- {#b1} An explicit target-dir override always wins and is used verbatim.
- {#b2} With no override, when Ludwig detects it is running under an outer cargo invocation it directs the nested build to a project-local cache target directory.
- {#b3} With no override and no outer cargo, the nested build inherits the ambient target directory so it shares the user's existing build cache.
- {#b4} The isolated target directory lives under the project's state cache dir, which is git-ignored.

## Examples
```example name="explicit override wins"
Given an explicit nested-target override is set
When the adapter chooses the target dir
Then it uses the override path
```

```example name="nested under cargo isolates"
Given no override and an outer cargo invocation is detected
When the adapter chooses the target dir
Then it returns a path inside the project cache dir
```

```example name="top-level inherits"
Given no override and no outer cargo invocation
When the adapter chooses the target dir
Then it returns no override so the ambient target is used
```

## Invariants
- {deterministic} Given an explicit override, the chosen target dir equals the override regardless of whether an outer cargo is detected.
- {deterministic} Given no override and a detected outer cargo, the chosen target dir is a descendant of the project cache dir; given no override and no outer cargo, no target dir is chosen.
- {judgment} A first-time top-level verify reuses the user's existing build artifacts rather than triggering a full rebuild.
