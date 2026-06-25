---
id: missing-section
title: Missing section
status: draft
version: 1
---

## Intent
This spec is missing the required Behavior section, which should cause the
parser to refuse it and explain what is missing in a precise way. The
fixture exists to lock that error path down.

## Examples
```example name="oops"
Given nothing
When parsed
Then it fails
```

## Invariants
- {deterministic} Parsing fails.
