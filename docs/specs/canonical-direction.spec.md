---
id: canonical-direction
title: canonical mode sets the drift remedy direction
status: active
owners: []
implements:
  - src/drift.rs
  - src/project.rs
depends_on: []
version: 1
---

## Intent
A Ludwig project declares a `canonical:` mode in `ludwig.yml` — `spec` (the
default) or `code`. The setting decides which side is the source of truth when
a spec and its implementing code diverge, and therefore which direction the
drift remedy points.

In `spec` mode the spec leads: detected drift means the code is stale, so the
guidance is to regenerate the code (or bump the spec version). In `code` mode
the code leads (spec-from-code): the same drift means the spec is the stale
side, so the guidance is to update the spec to match the code and re-verify.
The mode is a closed set; an unknown value must be rejected when the project is
opened rather than silently behaving like neither mode. Deriving a spec from
code automatically is out of scope here — only the direction of the remedy
changes.

## Behavior
- {#b1} `canonical` accepts exactly the values `spec` and `code`; any other value is rejected at project-open time with an error naming the key.
- {#b2} A missing `canonical` key defaults to `spec`.
- {#b3} When the code has drifted from a stamped spec, in `spec` mode the drift remedy directs the user to regenerate the code or bump the spec version.
- {#b4} For the same drift in `code` mode, the remedy directs the user to reconcile or update the spec to match the code, and never tells them to regenerate the code.

## Examples
```example name="unknown canonical rejected"
Given a ludwig.yml with canonical set to an unknown value
When the project is opened
Then opening fails with an error naming the canonical key
```

```example name="spec mode regenerates code"
Given a spec-mode project whose code stamp no longer matches the spec hash
When drift is reported for that file
Then the remedy tells the user to regenerate or bump the spec
```

```example name="code mode updates spec"
Given a code-mode project whose code body changed since the last verify
When drift is reported for that file
Then the remedy tells the user the spec is behind and to update it
```

## Invariants
- {deterministic} Opening a project whose `canonical` value is neither `spec` nor `code` returns an error, while both legal values open successfully.
- {deterministic} For an identical drift on the same file, the remedy text differs between `spec` and `code` mode, and the `code`-mode remedy never instructs the user to regenerate the code.
- {judgment} The drift remedy a user reads in each mode is actionable on its own — it names the side to change and the next command to run.
