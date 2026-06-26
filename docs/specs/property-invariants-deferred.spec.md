---
id: property-invariants-deferred
title: property invariants are parsed but not yet machine-verified
status: active
owners: []
implements:
  - src/verify.rs
depends_on: []
version: 1
---

## Intent
Ludwig classifies every invariant as `{deterministic}`, `{property}`, or
`{judgment}`. Deterministic invariants are checked by generated tests and
judgment invariants are routed to a host agent, but property-based generation
is deferred — no generator runs yet. A `{property}` invariant is therefore
parsed and carried through, yet nothing actually exercises it.

An unchecked invariant must never read as a satisfied one. For an `active`
spec, leaving a property invariant unverified would let the verifier
green-light a claim it never tested, so verification must fail loudly. For a
non-active spec (draft or deprecated) the parser has already declined to
enforce "verified", so reporting the property invariant as skipped is the
honest outcome. This spec pins that policy so it survives until the generator
lands.

## Behavior
- {#b1} The verifier emits exactly one `property` check per `{property}` invariant on the spec.
- {#b2} On an `active` spec, each property check reports `fail` because the invariant is not machine-verified.
- {#b3} On a non-active (draft or deprecated) spec, each property check reports `skip`.
- {#b4} No property-based generator runs; the policy depends only on the spec's status, not on any generated test execution.

## Examples
```example name="active property invariant fails"
Given an active spec carrying a single {property} invariant
When the spec is verified
Then the property check reports fail
```

```example name="draft property invariant skips"
Given a draft spec carrying a single {property} invariant
When the spec is verified
Then the property check reports skip
```

```example name="deprecated property invariant skips"
Given a deprecated spec carrying a single {property} invariant
When the spec is verified
Then the property check reports skip
```

## Invariants
- {deterministic} An active spec with a {property} invariant produces a property check whose status is fail; the same spec marked draft or deprecated produces a property check whose status is skip.
- {deterministic} The number of property checks equals the number of {property} invariants on the spec.
- {judgment} The fail detail tells the author how to proceed (move to draft, rewrite as {deterministic}, or downgrade to {judgment}) rather than just stating that the check failed.
