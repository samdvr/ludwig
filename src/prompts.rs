pub const SPEC_GRAMMAR_GUIDE: &str = "## Ludwig spec grammar (terse reference)

A Ludwig spec is one markdown file with YAML frontmatter and a fixed
ordered set of H2 sections.

Frontmatter (required keys):
  id           kebab-case slug, matches the filename (without .spec.md)
  title        human-readable
  status       draft | active | deprecated
  owners       list of strings
  implements   list of source-file globs the spec governs
  depends_on   list of other spec ids
  version      integer, start at 1

Sections, in this exact order:
  ## Intent           One paragraph (20-250 words). Why this exists.
  ## Behavior         Bulleted prose. Each bullet may carry `{#tagN}`.
  ## Examples         One or more fenced blocks: ```example name=\"...\"
                      with Given / When / Then / And steps in plain English.
  ## Invariants       Bulleted, each prefixed with one classifier:
                      {deterministic} machine-checkable
                      {property}      universally quantified
                      {judgment}      fuzzy, evaluated by an LLM-as-judge
  ## Non-goals        (optional) prose, negative steering
  ## Open questions   (optional) prose; non-empty blocks status: active
  ## Implementation notes (optional) advisory, not requirements

Rules:
- Sections must appear in exactly the order above.
- Every behavior bullet should be exercised by at least one example.
- Examples must contain Given, When, and Then (And is optional).
- Invariant bullets must begin with exactly one {classifier}.
- A spec cannot be marked status: active while it has Open questions.
";

pub const SPEC_EXAMPLE: &str = "---
id: token-bucket-rate-limiter
title: Token-bucket rate limiter
status: draft
owners: []
implements:
  - src/rate_limit/token_bucket.rs
depends_on: []
version: 1
---

## Intent
Protect downstream services from bursty clients by allowing short
bursts up to a configured capacity while enforcing a steady average
rate. A building block for per-tenant API quotas; not, by itself, a
fairness mechanism between tenants.

## Behavior
- {#b1} A limiter is created with `capacity` and `refill_rate` (tokens/sec).
- {#b2} `try_acquire(n)` returns true and consumes n tokens iff at least n are available.
- {#b3} Tokens refill continuously based on elapsed wall-clock time, capped at capacity.

## Examples
```example name=\"burst then throttle\"
Given a limiter with capacity 5 and refill_rate 1/sec
When try_acquire(1) is called 5 times in immediate succession
Then all 5 calls return true
And the 6th call in the same instant returns false
```

## Invariants
- {deterministic} tokens_consumed <= capacity + refill_rate * elapsed_seconds.
- {judgment} Errors surfaced to callers name the limiter in plain English.
";

pub struct PeerSpec<'a> {
    pub id: &'a str,
    pub title: &'a str,
}

pub struct ExistingSpec<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub status: &'a str,
}

pub fn spec_from_description(
    slug: &str,
    description: &str,
    game: Option<&str>,
    peers: &[PeerSpec<'_>],
    glossary: &[(String, String)],
) -> String {
    let peer_block = if peers.is_empty() {
        "(no other specs in this game yet)".to_string()
    } else {
        peers
            .iter()
            .map(|p| format!("- `{}` — {}", p.id, p.title))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let glossary_block = if glossary.is_empty() {
        "(no game-local glossary yet)".to_string()
    } else {
        glossary
            .iter()
            .map(|(term, defn)| format!("- **{term}**: {defn}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let game_label = game.unwrap_or("(root)");
    let call_back_args = match game {
        Some(g) => format!(r#"{{slug: "{slug}", content: <your markdown>, game: "{g}"}}"#),
        None => format!(r#"{{slug: "{slug}", content: <your markdown>}}"#),
    };

    format!(
        "You are drafting a Ludwig specification.

Slug:        {slug}
Game:        {game_label}
Description:
  {description}

Peer specs in this game:
{peer_block}

Glossary (terms local to this game):
{glossary_block}

{SPEC_GRAMMAR_GUIDE}
## Reference example
{SPEC_EXAMPLE}
## Your task
Output a complete Ludwig spec for `{slug}`, as raw markdown. Start
with the YAML frontmatter and end with the last section. Default
`status: draft`. Use `version: 1`. Do not include any preamble,
explanation, or trailing commentary — only the markdown.

After you produce the markdown, call the `spec.write` tool with
`{call_back_args}`.
Ludwig will validate and either persist the spec or return precise
errors for you to correct.
"
    )
}

pub fn project_decomposition(
    description: &str,
    existing_specs: &[ExistingSpec<'_>],
    existing_games: &[String],
) -> String {
    let existing_specs_block = if existing_specs.is_empty() {
        "(no existing specs)".to_string()
    } else {
        existing_specs
            .iter()
            .map(|s| format!("- `{}` ({}) — {}", s.id, s.status, s.title))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let existing_games_block = if existing_games.is_empty() {
        "(no existing games)".to_string()
    } else {
        existing_games
            .iter()
            .map(|g| format!("- `{g}`"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "You are decomposing a software project into Ludwig specifications.

Project description:
  {description}

Existing specs in this project:
{existing_specs_block}

Existing language-games (sub-directories under specs/):
{existing_games_block}

A Ludwig spec describes ONE coherent unit of behavior — a class, an
endpoint, a module, a workflow. Aim for 3–10 specs per project. If
more are needed, group them into language-games (sub-directories
with their own glossary) and propose those too. Reuse existing
specs and games where they fit; don't propose duplicates.

Each game is a Wittgensteinian \"local context\": the same term can
mean different things in different games. Use this when a project
has bounded contexts (e.g. `auth/` and `billing/` may each have
their own `User`).

{SPEC_GRAMMAR_GUIDE}
## Your task
Return ONE JSON object with this exact shape (no preamble):

{{
  \"games\": [
    {{
      \"name\": \"kebab-case-name\",
      \"intent\": \"one sentence: what this game is, what terms are local to it\",
      \"glossary\": {{ \"Term\": \"definition\", ... }}
    }}
  ],
  \"specs\": [
    {{
      \"slug\": \"kebab-case-id\",
      \"title\": \"Human-readable title\",
      \"game\": \"name-of-game-or-null\",
      \"summary\": \"one sentence; what this spec governs\"
    }}
  ],
  \"rationale\": \"2-4 sentences explaining the decomposition\"
}}

After you produce the JSON, for each proposed game, call the
`game.create` tool with its name and glossary. For each proposed
spec, call `spec.propose` to obtain its drafting prompt, then call
`spec.write` with the resulting markdown. When all specs are
written, pause and present the catalog to the human for review
before any implementation begins.
"
    )
}
