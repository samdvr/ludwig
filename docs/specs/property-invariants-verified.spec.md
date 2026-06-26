---
id: property-invariants-verified
title: property invariants are machine-verified by a generated property test
status: active
owners: []
implements:
  - src/verify.rs
  - src/adapters/rust.rs
depends_on: []
version: 1
---

## Intent
Ludwig classifies every invariant as `{deterministic}`, `{property}`, or
`{judgment}`. Deterministic invariants are checked by generated tests and
judgment invariants are routed to a host agent. A `{property}` invariant is now
machine-verified the same way a deterministic one is: the Rust adapter scaffolds
a `test_property_invariant_<n>` test the author fills in — quantified over many
inputs rather than a single case — and the verifier folds that test's real cargo
verdict into a `property` check. An active spec may therefore rely on a property
invariant once its backing test passes, while an unexercised property never
reads as satisfied: a missing property test fails loudly. This supersedes the
earlier policy that deferred property verification and failed every active
property invariant outright.

## Behavior
- {#b1} The adapter scaffolds one `test_property_invariant_<n>` `#[test]` per `{property}` invariant, with a hint to quantify over many generated inputs.
- {#b2} The verifier routes each `test_property_invariant_*` cargo result to a `property` check whose status mirrors the test verdict (pass → pass, fail → fail, ignored → skip).
- {#b3} If a `{property}` invariant has no backing `test_property_invariant_<n>`, the verifier emits a failing `property` check, regardless of spec status.
- {#b4} The verifier no longer fails property invariants merely for being property invariants; the outcome depends on the generated test, not on the spec's status.

## Examples
```example name="passing property test passes"
Given an active spec with a {property} invariant backed by a passing test_property_invariant_1
When the spec is verified
Then the property check reports pass
```

```example name="failing property test fails"
Given an active spec with a {property} invariant backed by a failing test_property_invariant_1
When the spec is verified
Then the property check reports fail
```

```example name="missing property test fails"
Given an active spec with a {property} invariant and no test_property_invariant_1
When the spec is verified
Then a property check reports fail
```

## Invariants
- {deterministic} A `test_property_invariant_1` reported by cargo as ok produces a `property` check with status pass; reported as FAILED produces status fail.
- {deterministic} A spec with a `{property}` invariant and no matching `test_property_invariant_<n>` produces a `property` check with status fail.
- {property} For any spec, the number of generated `test_property_invariant_<n>` stubs equals the number of `{property}` invariants on the spec.
- {judgment} The missing-property failure detail tells the author to add a property test quantified over many inputs, rather than merely stating the check failed.
