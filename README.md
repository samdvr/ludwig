# Ludwig

A specification-driven development framework whose specs are written as close
to natural language as possible. The same prose-first markdown spec drives
LLM code generation **and** verifies the resulting code. Named after
Ludwig Wittgenstein.

> **Scope:** Ludwig targets **Rust**. Deterministic checks shell out to
> `cargo test`, so verifying a project requires a Cargo workspace. The spec
> format itself is plain markdown, but verification is Rust-specific by design —
> there is no plan to add adapters for other languages.

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

`{property}` invariants are parsed but not yet machine-verified — no generator
runs. Rather than silently pass, Ludwig reacts to the spec's status: on an
`active` spec each property invariant reports `fail` (you can't rely on an
invariant nothing checked), and on a draft/deprecated spec it reports `skip`.
See `docs/specs/property-invariants-deferred.spec.md`.

### Canonical direction

The `canonical:` setting in `ludwig.yml` decides which side is the source of
truth when a spec and its code diverge:

- `spec` (default) — the spec leads. On drift, the code is stale; `ludwig diff`
  tells you to regenerate or bump the spec version.
- `code` — the code leads (spec-from-code). On drift, the spec is the stale
  side; `ludwig diff` tells you to update the spec to match the code, then
  re-verify. The skill guidance flips accordingly.

An unknown value is rejected at load time. (Ludwig does not yet *derive* a spec
from code — see Deferred.)

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
| `ludwig move SLUG [--to-game G] [--force]` | Relocate an existing spec to a different game |
| `ludwig decompose` | Emit a prompt to break a project description (stdin or `-d`) into specs |
| `ludwig propose SLUG -d DESC [-g GAME]` | Emit a prompt for drafting a single spec |
| `ludwig write-spec SLUG [-g GAME]` | Validate spec markdown on stdin and persist it |
| `ludwig game-new NAME [-i INTENT] [-x Term:Defn ...]` | Create a language-game manifest |
| `ludwig parse [PATH] [--quiet]` | Parse one or all specs; report structural errors |
| `ludwig plan ID` | Emit JSON generation brief for the host agent |
| `ludwig verify [ID] [--all] [--json]` | Run the full pipeline; write report under `.ludwig/reports/` |
| `ludwig verify [ID] --emit-judgment-prompts` | Print judgment prompts as JSON |
| `ludwig verify --ingest-judgments FILE` | Ingest judgment verdicts back into state.json |
| `ludwig diff [ID] [--all] [--json]` | Surface drift between specs and code |
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
| `spec.diff` | Drift report between a spec and its implementing files |
| `spec.propose` | Emit a prompt for drafting a spec from a description |
| `spec.write` | Validate agent-drafted markdown and persist it |
| `spec.move` | Relocate a spec into a different game |
| `spec.ingest_judgments` | Persist judgment verdicts inline (no file path needed) |
| `project.decompose` | Emit a prompt to break a project into specs + games |
| `game.create` | Create a language-game (`_game.md`) |

Resources:
- `ludwig://spec/<id>` — raw spec markdown
- `ludwig://report/latest` — most recent verification report

## Building an app with the MCP

Once `ludwig mcp` is registered, you can drive an entire project from a single
chat session. The agent calls Ludwig's MCP tools instead of you running the
CLI by hand; you stay in the loop for review.

1. **Scaffold the project.** In an empty directory, run `ludwig init` once.
   This creates `ludwig.yml`, `specs/`, `.ludwig/`, and the Claude Code skill
   manifest. The MCP server discovers this root on every tool call.

2. **Describe the app.** Tell the agent what you want to build, e.g.
   *"Build a URL shortener with per-tenant analytics."* The agent calls
   `project.decompose` to get a prompt, then proposes a set of games (`auth`,
   `shorten`, `analytics`, …) and a draft spec per behavior.

3. **Persist the drafts.** For each game the agent calls `game.create`; for
   each spec it calls `spec.propose` to get the drafting prompt, then
   `spec.write` to validate and persist the markdown. Anything malformed is
   rejected with a structural error — the agent retries until the spec parses.

4. **Review and activate.** You read `specs/_index.md` (regenerated by
   `ludwig catalog`), resolve any `Open questions`, and flip drafts to
   `status: active`. Active is the gate: nothing generates code until you
   approve the intent.

5. **Generate code.** The agent calls `spec.plan` per active spec to get the
   generation brief (resolved glossary, dependencies, behavior tags, examples)
   and writes the implementation and test bodies. Each generated source file
   ends with the `ludwig-spec: <id>@<version> hash=…` stamp.

6. **Verify.** The agent calls `spec.verify`, which runs structural and
   deterministic layers and emits judgment prompts. The agent evaluates each
   judgment prompt itself, then calls `spec.verify` again with the verdicts
   ingested. The report lands under `.ludwig/reports/` and is also reachable
   as `ludwig://report/latest`.

7. **Iterate.** When you edit a spec, the agent re-runs `spec.plan` and
   regenerates the affected files; when you edit code, drift detection flags
   the seam. The spec is the durable artifact — chat turns are not.

A minimal driver prompt: *"Use the Ludwig MCP tools. Decompose this
description into specs, write them, wait for me to activate, then generate
and verify."* The agent does the rest.

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

- Property-based **generation** — `{property}` invariants are parsed and their
  verification policy is defined and tested (active → `fail`, non-active →
  `skip`; see "Verification, in three layers"), but no generator produces or
  runs property tests yet.
- **spec-from-code generation** — `canonical: code` mode flips drift semantics
  so the spec is the stale side (see "Canonical direction"), but Ludwig does
  not yet derive or auto-update a spec from existing code; you update the spec
  yourself, then re-verify.

## Development

```bash
cargo build --offline    # the cached crate set is sufficient
cargo test --offline
cargo run --offline -- version
```

## License

MIT.
