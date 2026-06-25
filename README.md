# Ludwig

A specification-driven development framework whose specs are written as close
to natural language as possible. The same prose-first markdown spec drives
LLM code generation **and** verifies the resulting code. Named after
Wittgenstein: meaning is use, language-games, family resemblance.

> The trailing `ludwig-spec:` comment in every generated source file makes the
> relationship between language and world visible at the seam. That's the
> point.

## Why

Today, when a human asks an LLM to write code, the request is a chat message
and the contract is whatever the model inferred. The result drifts the moment
either side is edited. Ludwig replaces the chat message with a
**specification** — a markdown document with a small fixed shape — that is the
durable, versioned source of intent. The same document drives generation and
verifies the resulting code, so spec and implementation stay coupled.

## A spec, in full

```markdown
---
id: token-bucket-rate-limiter
title: Token-bucket rate limiter
status: active
implements:
  - src/rate_limit/token_bucket.rs
version: 4
---

## Intent
Protect downstream services from bursty clients by allowing short bursts up
to a configured capacity while enforcing a steady average rate. The limiter
is a building block for per-tenant API quotas; it is not, by itself, a
fairness mechanism between tenants.

## Behavior
- {#b1} A limiter is created with `capacity` and `refill_rate` (tokens/sec).
- {#b2} `try_acquire(n)` returns true and consumes n tokens iff at least n are available.
- {#b3} Tokens refill continuously based on wall-clock time, capped at capacity.

## Examples
​```example name="burst then throttle"
Given a limiter with capacity 5 and refill_rate 1/sec
When try_acquire(1) is called 5 times in immediate succession
Then all 5 calls return true
And the 6th call in the same instant returns false
​```

## Invariants
- {deterministic} tokens_consumed <= capacity + refill_rate * elapsed_seconds.
- {property} after waiting C / refill_rate seconds the limiter is full.
- {judgment} Errors surfaced to callers name the limiter and required wait in plain English.
```

Sections are fixed and ordered: **Intent → Behavior → Examples → Invariants**
(plus optional **Non-goals**, **Open questions**, **Implementation notes**).
The only embedded DSL is `Given/When/Then` inside Examples — Gherkin-shaped so
LLMs already know it.

## Workflow

Ludwig supports two complementary directions:

**1. Description → specs.** Start from a project or feature description; the
host agent decomposes it into specs and drafts each one; Ludwig validates and
persists; the human reviews before any implementation.

```bash
echo "A URL shortener with per-tenant analytics" | ludwig decompose
# inside Claude Code:
/project-decompose A URL shortener with per-tenant analytics
# agent emits JSON, then for each game/spec calls game.create + spec.propose + spec.write
ludwig catalog && cat specs/_index.md   # review, then move drafts to status: active
```

**2. Spec → code.** Once a spec is active, generate the implementation, then
verify.

```bash
ludwig init                              # one-time scaffolding
ludwig new auth/login --game auth        # OR write a spec by hand
/spec-generate login                     # inside Claude Code: LLM writes src/ + tests/ludwig_login.rs
ludwig verify login                      # structural + deterministic + judgment-pending
/spec-verify login                       # inside Claude Code: also evaluates {judgment} invariants
git add specs/ src/ tests/ .ludwig/state.json && git commit
```

When you change the spec, `ludwig diff` flags the implementing files. When you
change the code directly, the same diff flags drift on the other side. The
trailing `ludwig-spec: <id>@<version> hash=<sha>` comment in each implementing
file is load-bearing — don't hand-edit it.

## Verification, in three layers

1. **Structural** — frontmatter is well-typed, sections are in order, behavior
   tags are unique, every file in `implements:` exists and carries a matching
   `ludwig-spec:` stamp. In-process Rust. Sub-second.
2. **Deterministic** — Ludwig scaffolds `tests/ludwig_<slug>.rs` with one
   `#[test]` per Example and per `{deterministic}` invariant, each containing
   a `todo!()` body and a doc-comment with the Gherkin steps. You replace the
   bodies. `ludwig verify` shells out to `cargo test --test ludwig_<slug>` and
   parses the results.
3. **Judgment** — each `{judgment}` invariant is packaged as a prompt and
   emitted as JSON. Ludwig itself holds no API keys; the host agent (Claude
   Code) evaluates each prompt and writes verdicts back via
   `ludwig verify --ingest-judgments <file>`. Verdicts are keyed by the spec
   hash, so changing the spec invalidates old verdicts automatically.

## The Wittgenstein angle, concretely

- **Meaning is use** — the Examples section is verification, not illustration.
  A behavior bullet without examples that exercise it is treated as vague
  intent, and the parser flags it.
- **Language-games** — each `specs/<dir>/` is a local context. `_game.md`
  declares its glossary. The word "user" in `specs/auth/` need not mean the
  same thing as in `specs/billing/`. The generation brief always includes the
  resolved glossary for the spec's enclosing game.
- **Family resemblance** — beyond the fixed section list, Ludwig refuses to
  grow a domain ontology. Specs for an HTTP endpoint and a CLI command
  resemble each other; they don't share a schema.
- **Whereof one cannot speak** — `Open questions` in a spec blocks
  `status: active`. Unresolved meaning cannot drive generation.

## Installation

```bash
git clone <this repo> && cd ludwig
cargo install --path .
```

Or, from a release build, symlink the binary onto your PATH:

```bash
cargo build --release
sudo ln -s "$PWD/target/release/ludwig" /usr/local/bin/ludwig
```

Requires a Rust toolchain (the Ludwig binary), and — in any project Ludwig
verifies — whatever runs `cargo test`.

## Commands

| Command | Purpose |
|---|---|
| `ludwig init` | Scaffold `ludwig.yml`, `specs/`, `.ludwig/`, register Claude Code skill |
| `ludwig new SLUG [--game G]` | Scaffold a new spec from a blank template |
| `ludwig decompose` | Emit a prompt to break a project description (stdin or `-d`) into specs |
| `ludwig propose SLUG -d DESC [-g GAME]` | Emit a prompt for drafting a single spec |
| `ludwig write-spec SLUG [-g GAME]` | Validate spec markdown on stdin and persist it |
| `ludwig game-new NAME [-i INTENT] [-x Term:Defn ...]` | Create a language-game manifest |
| `ludwig parse [PATH]` | Parse one or all specs; report structural errors |
| `ludwig plan ID` | Emit JSON generation brief for the host agent |
| `ludwig verify [ID] [--all]` | Run the full pipeline; write report under `.ludwig/reports/` |
| `ludwig verify [ID] --emit-judgment-prompts` | Print judgment prompts as JSON |
| `ludwig verify --ingest-judgments FILE` | Ingest judgment verdicts back into state.json |
| `ludwig diff [ID] [--all]` | Surface drift between specs and code |
| `ludwig catalog` | Regenerate `specs/_index.md` |
| `ludwig mcp [--root PATH]` | Start the MCP server over stdio |

## MCP server

Ludwig speaks MCP (Model Context Protocol) over stdio. Register it with Claude
Code:

```bash
claude mcp add ludwig -- ludwig mcp
# or, scoped to one project:
claude mcp add --scope project ludwig -- ludwig mcp
```

The server discovers the project at each tool call (`$PWD`, then
`$LUDWIG_PROJECT`, then `--root`).

| Tool | Purpose |
|---|---|
| `spec.list` | List all specs in the project |
| `spec.read` | Return the parsed structure of a spec |
| `spec.plan` | Generation brief (drives code generation) |
| `spec.verify` | Run the verification pipeline |
| `spec.propose` | Emit a prompt for drafting a spec from a description |
| `spec.write` | Validate agent-drafted markdown and persist it |
| `project.decompose` | Emit a prompt to break a project into specs + games |
| `game.create` | Create a language-game (`_game.md`) |

Resources:
- `ludwig://spec/<id>` — raw spec markdown
- `ludwig://report/latest` — most recent verification report

## On-disk layout

```
<project root>/
  ludwig.yml                       # project config
  specs/
    _index.md                      # generated: spec catalog
    auth/
      _game.md                     # language-game manifest
      login.spec.md
  src/
    auth/login.rs                  # ends with `// ludwig-spec: login@1 hash=…`
  tests/
    ludwig_login.rs                # scaffolded once, then user-owned
  .ludwig/
    state.json                     # spec hashes, file fingerprints, judgment verdicts
    reports/                       # verification reports (JSON + latest.md)
  .claude/
    skills/
      ludwig.yaml                  # registered by `ludwig init`
```

`<slug>.spec.md` matches the frontmatter `id`. IDs are globally unique within
a project.

## Status

v0.1 ships:

- Strict, hand-rolled markdown spec parser
- Project scaffolding, catalog, language-game inheritance
- Generation brief (`ludwig plan`) with transitively-resolved `depends_on`
- Rust adapter: scaffolds `#[test]` stubs from Examples + invariants, runs via `cargo test`
- Structural + deterministic verification, judgment-prompt round-trip
- Three-way drift detection (`ludwig diff`) — stale stamp, body changed, missing, unstamped
- Claude Code skill manifest + JSON-RPC 2.0 MCP server over stdio
- 60-test integration suite

Deferred:

- `{property}` invariants (parsed and reported as `skip`)
- `canonical: code` mode (spec-from-code)
- Adapters for other languages

See `plan.md` for the design and the milestone-by-milestone history of the
Ruby → Rust migration.

## Development

```bash
cargo build --offline    # the cached crate set is sufficient
cargo test --offline
cargo run --offline -- version
```

## License

MIT.
