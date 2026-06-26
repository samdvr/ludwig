use serde::Serialize;

#[derive(Debug, Serialize)]
struct SkillManifest {
    name: &'static str,
    version: String,
    description: &'static str,
    system: &'static str,
    commands: Vec<SlashCommand>,
}

#[derive(Debug, Serialize)]
struct SlashCommand {
    name: &'static str,
    description: &'static str,
    args: &'static str,
    run: &'static str,
    #[serde(skip_serializing_if = "str::is_empty")]
    after: &'static str,
}

const SYSTEM_PROMPT: &str = "\
Ludwig is the project's specification-driven development framework.
Specs live under `specs/` as `.spec.md` files. Each spec has a fixed
ordered shape:

  ## Intent
  ## Behavior
  ## Examples              <-- Given/When/Then blocks
  ## Invariants            <-- {deterministic} | {property} | {judgment}
  ## Non-goals             (optional)
  ## Open questions        (optional; blocks status: active)
  ## Implementation notes  (optional)

Four rules govern your interaction with Ludwig:

1. The `canonical:` setting in `ludwig.yml` decides the source of truth.
   In `spec` mode (the default) the spec is canonical: when the user asks
   for behavior, look at the spec first; if the spec is wrong, edit the
   spec, then regenerate. Never edit generated code without also updating
   the spec. In `code` mode the code is canonical (spec-from-code): when a
   spec and its code diverge, the spec is the stale side — update the spec
   to match the code, then re-verify.

2. The trailing `ludwig-spec: <id>@<version> hash=<sha>` comment in
   every implementing source file is load-bearing — Ludwig uses it to
   detect drift. Never delete or hand-edit it; let `/spec-generate`
   rewrite it.

3. After any code change, run `/spec-verify` (or `ludwig verify`)
   before declaring the task done. The verifier may emit judgment
   prompts that you must evaluate and ingest before the report is
   complete.

4. New work flows from description → spec → human review → code.
   Use `/project-decompose` for whole projects and `/spec-from-description`
   for individual features. Always pause for human review after
   writing specs; do not implement without explicit approval.

Language-games: each directory under `specs/` is a local context with
its own glossary (`_game.md`). Terms mean what the local glossary
says, not what they mean elsewhere in the project.

See `specs/_index.md` for the full catalog.
";

const COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "spec-new",
        description: "Scaffold a new Ludwig spec from a blank template",
        args: "SLUG [--game GAME]",
        run: "ludwig new {{args}}",
        after: "Fill in Intent → Behavior → Examples, in that order. Pause for human review before adding Invariants.",
    },
    SlashCommand {
        name: "spec-from-description",
        description: "Draft a spec from a description; Ludwig validates and persists",
        args: "SLUG -- DESCRIPTION",
        run: "ludwig propose {{slug}} --description {{description|shell-escape}}",
        after: "Read the prompt above. Draft the spec markdown EXACTLY as specified. Then call the `spec.write` MCP tool (or pipe the markdown into `ludwig write-spec {{slug}}`). If Ludwig rejects the draft, read the error, fix the markdown, and try again. Do NOT proceed to implementation — the human reviews specs first.",
    },
    SlashCommand {
        name: "project-decompose",
        description: "Break a project description into specs and language-games",
        args: "-- DESCRIPTION",
        run: "echo {{description|shell-escape}} | ludwig decompose",
        after: "Read the prompt and emit the JSON decomposition. For each game in the result, call `game.create` (MCP tool) or `ludwig game-new <name>`. For each spec, call `spec.propose` to obtain a drafting prompt, draft the markdown, then call `spec.write`. When all specs are written, RUN `ludwig catalog` and present the result to the human. STOP — do not implement any of the specs without explicit human approval.",
    },
    SlashCommand {
        name: "spec-generate",
        description: "Generate or regenerate code from an approved Ludwig spec",
        args: "ID",
        run: "ludwig plan {{args}}",
        after: "Read the JSON brief above. Produce code that satisfies every Behavior bullet and Example. Append a trailing comment `// ludwig-spec: <id>@<version> hash=<hash>` to each implementing file. Then run `/spec-verify {{args}}`.",
    },
    SlashCommand {
        name: "spec-verify",
        description: "Run Ludwig verification, including judgment-LLM round-trip",
        args: "ID",
        run: "ludwig verify {{args}} --emit-judgment-prompts",
        after: "For each judgment prompt in the JSON output, read the listed evidence files and produce a JSON verdict object: {invariant_key, verdict: pass|fail, rationale, spec_id, spec_hash}. Write all verdicts to a JSON array file and run `ludwig verify --ingest-judgments <file>`. Then re-run `ludwig verify {{args}}` to see the final report.",
    },
    SlashCommand {
        name: "spec-diff",
        description: "Show drift between specs and code",
        args: "[ID|--all]",
        run: "ludwig diff {{args}}",
        after: "",
    },
    SlashCommand {
        name: "spec-catalog",
        description: "Regenerate the spec index and read it into context",
        args: "",
        run: "ludwig catalog && cat specs/_index.md",
        after: "",
    },
];

pub fn manifest_yaml() -> String {
    let manifest = SkillManifest {
        name: "ludwig",
        version: crate::VERSION.to_string(),
        description:
            "Specification-driven development. Specs are markdown; the same spec drives generation and verification.",
        system: SYSTEM_PROMPT,
        commands: COMMANDS.to_vec(),
    };
    serde_yaml::to_string(&manifest).unwrap_or_default()
}

impl Clone for SlashCommand {
    fn clone(&self) -> Self {
        Self {
            name: self.name,
            description: self.description,
            args: self.args,
            run: self.run,
            after: self.after,
        }
    }
}
