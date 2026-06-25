---
id: hello-greeter
title: Hello greeter
status: draft
version: 1
---

## Intent
A trivial spec that exists only to exercise the parser's required-section
path. It greets a user by name and returns the greeting string. There is no
deeper purpose here; the fixture is deliberately minimal.

## Behavior
- It takes a name and returns "Hello, <name>!".
- An empty name returns "Hello, friend!".

## Examples
```example name="named greeting"
Given a greeter
When greet("Alice") is called
Then it returns "Hello, Alice!"
```

```example name="empty greeting"
Given a greeter
When greet("") is called
Then it returns "Hello, friend!"
```

## Invariants
- {deterministic} The return value is always a non-empty string.
