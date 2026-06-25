---
id: mixed-classifiers
title: Mixed classifiers
status: draft
version: 1
---

## Intent
This spec puts two classifiers in a single invariant bullet, which is
forbidden because the verifier needs to route each invariant to exactly one
check kind. Splitting fixes it.

## Behavior
- It does a thing.

## Examples
```example name="thing"
Given a thing
When it runs
Then it works
```

## Invariants
- {deterministic} {property} both at once, which the parser refuses.
