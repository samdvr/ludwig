---
id: token-bucket-rate-limiter
title: Token-bucket rate limiter
status: active
owners:
  - sam
implements:
  - src/rate_limit/token_bucket.rb
depends_on:
  - clock-source
version: 4
---

## Intent
Protect downstream services from bursty clients by allowing short bursts up
to a configured capacity while enforcing a steady average rate. The limiter
is a building block for per-tenant API quotas; it is not, by itself, a
fairness mechanism between tenants.

## Behavior
- {#b1} A limiter is created with `capacity` (max tokens) and `refill_rate` (tokens per second, may be fractional).
- {#b2} `try_acquire(n)` returns true and consumes `n` tokens if at least `n` are available; otherwise returns false and consumes none.
- {#b3} Tokens refill continuously based on wall-clock time elapsed since the last observation, capped at `capacity`.
- {#b4} The limiter is safe under concurrent callers within one process.

## Examples
```example name="burst then throttle"
Given a limiter with capacity 5 and refill_rate 1/sec
When try_acquire(1) is called 5 times in immediate succession
Then all 5 calls return true
And the 6th call in the same instant returns false
```

```example name="refill after wait"
Given a limiter with capacity 5 and refill_rate 1/sec, fully drained
When 2 seconds of clock time elapse
And try_acquire(2) is called
Then it returns true
And the 3rd token is not yet available
```

## Invariants
- {deterministic} For any sequence of calls, tokens_consumed <= capacity + refill_rate * elapsed_seconds.
- {property} For any capacity C and any drain sequence, after waiting C / refill_rate seconds the limiter is full.
- {judgment} Errors surfaced to callers (if any) name the limiter and the wait time required, in plain English.

## Non-goals
Distributed coordination. Per-tenant accounting. Persistence across restarts.
