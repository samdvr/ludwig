---
id: out-of-order
title: Out of order
status: draft
version: 1
---

## Intent
This spec places Examples before Behavior, which violates the canonical
ordering enforced by the parser. The error message should name the
mis-ordering clearly so the author can fix it without head-scratching.

## Examples
```example name="oops"
Given nothing
When parsed
Then it fails
```

## Behavior
- It does a thing.

## Invariants
- {deterministic} Parsing fails.
