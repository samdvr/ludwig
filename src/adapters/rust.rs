use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::LazyLock;

use regex::Regex;

use crate::adapters::{Adapter, RenderInfo, RunResult, TestResult, TestStatus};
use crate::error::VerifyError;
use crate::project::Project;
use crate::spec::{Document, GherkinKeyword};

pub struct RustAdapter {
    pub project: Project,
}

impl RustAdapter {
    pub fn new(project: Project) -> Self {
        Self { project }
    }

    pub fn tests_dir(&self) -> PathBuf {
        self.project.root.join("tests")
    }

    pub fn test_file_for(&self, id: &str) -> PathBuf {
        self.tests_dir()
            .join(format!("ludwig_{}.rs", file_slug(id)))
    }
}

impl Adapter for RustAdapter {
    fn render(&self, doc: &Document) -> Result<RenderInfo, crate::Error> {
        fs::create_dir_all(self.tests_dir())
            .map_err(|e| VerifyError::new(format!("mkdir tests: {e}")))?;
        let target = self.test_file_for(doc.id());
        // Create the scaffold with `create_new` (via `write_guarded`) so a file
        // that appears between a would-be `is_file()` check and the write can't be
        // silently clobbered (TOCTOU). If the file already exists we fall through
        // to the existing-file path below.
        match crate::util::write_guarded(&target, render_scaffold(doc).as_bytes(), false) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Body is user-owned, but the trailing `ludwig-spec:` stamp must track
                // the current spec hash so drift detection stays meaningful. Update in
                // place when present; if the user has stripped the stamp entirely we
                // leave the file alone — the structural check will surface that as a
                // missing-stamp failure.
                let content = fs::read_to_string(&target)
                    .map_err(|e| VerifyError::new(format!("read {}: {e}", target.display())))?;
                // Guard against slug collisions: two distinct spec ids can map to the
                // same `file_slug` (e.g. `auth-login` and `auth/login` both become
                // `auth_login`, so both want `tests/ludwig_auth_login.rs`). If this
                // file already carries a stamp for a *different* spec, refuse to touch
                // it — otherwise `update_stamp_in_place` would silently rewrite that
                // spec's stamp to ours and `run` would verify the wrong tests. Surface
                // it so the author renames one id.
                if let Some(stamp) = crate::drift::parse_trailing(&content)
                    && stamp.id != doc.id()
                {
                    return Err(VerifyError::new(format!(
                        "generated test file {} already belongs to spec {:?}, but spec {:?} maps to the \
                         same file (their ids differ only by `-` vs `/`); rename one id so they don't collide",
                        crate::util::rel_str(&self.project.root, &target),
                        stamp.id,
                        doc.id(),
                    ))
                    .into());
                }
                let updated = crate::drift::update_stamp_in_place(&content, doc);
                if updated != content {
                    crate::util::write_guarded(&target, updated.as_bytes(), true)
                        .map_err(|e| VerifyError::new(format!("write {}: {e}", target.display())))?;
                }
            }
            Err(e) => {
                return Err(VerifyError::new(format!("write {}: {e}", target.display())).into());
            }
        }
        // Return both paths; for the Rust adapter the "spec file" and "steps file" are
        // the same single file. Keep the API uniform for future adapters.
        Ok(RenderInfo {
            spec_file: target.clone(),
            steps_file: target,
        })
    }

    fn run(&self, doc: &Document) -> Result<RunResult, crate::Error> {
        let test_name = format!("ludwig_{}", file_slug(doc.id()));
        let mut cmd = Command::new("cargo");
        cmd.arg("test")
            .arg("--test")
            .arg(&test_name)
            .arg("--")
            .arg("--format=pretty")
            .arg("--test-threads=1")
            .current_dir(&self.project.root);

        // Run the nested build against an isolated target dir when we're nested
        // under an outer cargo invocation, so we don't deadlock on its `target/`
        // lock. A plain top-level run inherits the ambient target and keeps its
        // build cache. See spec `verify-isolates-nested-cargo`.
        if let Some(td) = choose_verify_target_dir(
            std::env::var("LUDWIG_NESTED_CARGO_TARGET_DIR").ok(),
            std::env::var_os("CARGO").is_some(),
            &self.project.cache_dir(),
        ) {
            let _ = fs::create_dir_all(&td);
            cmd.env("CARGO_TARGET_DIR", td);
        }

        let output = cmd
            .output()
            .map_err(|e| VerifyError::new(format!("spawn cargo: {e}")))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let combined = format!("{stdout}\n{stderr}");
        Ok(parse_output(&combined, output.status.code()))
    }
}

// -- nested-cargo target isolation -------------------------------------------

/// Decide the `CARGO_TARGET_DIR` for the nested `cargo test` run, or `None` to
/// inherit the ambient one. See spec `verify-isolates-nested-cargo`:
/// an explicit override always wins; otherwise we isolate only when we detect
/// we're running under an outer cargo invocation (cargo sets `CARGO` for the
/// processes it spawns), so a plain top-level `ludwig verify` keeps sharing the
/// user's build cache.
pub(crate) fn choose_verify_target_dir(
    explicit_override: Option<String>,
    outer_cargo: bool,
    cache_dir: &std::path::Path,
) -> Option<PathBuf> {
    if let Some(o) = explicit_override {
        return Some(PathBuf::from(o));
    }
    if outer_cargo {
        return Some(cache_dir.join("verify-target"));
    }
    None
}

// -- output parsing -----------------------------------------------------------

static CARGO_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^test\s+(?P<name>[A-Za-z_][\w:]*)\s+\.\.\.\s+(?P<verdict>ok|FAILED|ignored)\b")
        .unwrap()
});

pub(crate) fn parse_output(output: &str, exit_code: Option<i32>) -> RunResult {
    let mut results: Vec<TestResult> = Vec::new();
    for line in output.lines() {
        if let Some(caps) = CARGO_LINE.captures(line.trim()) {
            let name = caps.name("name").unwrap().as_str().to_string();
            // `cargo test` may report `<crate_name>::test_foo` style for integration tests;
            // strip the module prefix so callers see just `test_foo`.
            let name = name.rsplit("::").next().unwrap_or(&name).to_string();
            let status = match caps.name("verdict").unwrap().as_str() {
                "ok" => TestStatus::Pass,
                "FAILED" => TestStatus::Fail,
                "ignored" => TestStatus::Skip,
                _ => TestStatus::Fail,
            };
            results.push(TestResult { name, status });
        }
    }
    let pass = results
        .iter()
        .filter(|r| r.status == TestStatus::Pass)
        .count() as u32;
    let fail = results
        .iter()
        .filter(|r| r.status == TestStatus::Fail)
        .count() as u32;
    let skip = results
        .iter()
        .filter(|r| r.status == TestStatus::Skip)
        .count() as u32;
    RunResult {
        tests: results,
        pass,
        fail,
        skip,
        exit_code,
        raw: output.to_string(),
    }
}

// -- rendering ----------------------------------------------------------------

pub(crate) fn file_slug(id: &str) -> String {
    id.replace(['-', '/'], "_")
}

pub(crate) fn example_test_name(name: &str) -> String {
    let mut out = String::from("test_example_");
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    // Collapse repeats and trim trailing underscores.
    let collapsed: String = out
        .split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    if collapsed.starts_with("test_example_") {
        collapsed
    } else {
        format!("test_example_{collapsed}")
    }
}

pub(crate) fn invariant_test_name(idx: usize) -> String {
    format!("test_deterministic_invariant_{}", idx + 1)
}

pub(crate) fn property_test_name(idx: usize) -> String {
    format!("test_property_invariant_{}", idx + 1)
}

fn keyword_label(k: GherkinKeyword) -> &'static str {
    match k {
        GherkinKeyword::Given => "Given",
        GherkinKeyword::When => "When",
        GherkinKeyword::Then => "Then",
        GherkinKeyword::And => "And",
    }
}

fn render_scaffold(doc: &Document) -> String {
    let mut out = String::new();
    let stamp = format!(
        "// ludwig-spec: {id}@{version} hash={hash}",
        id = doc.id(),
        version = doc.version(),
        hash = doc.canonical_hash(),
    );
    out.push_str(&stamp);
    out.push('\n');
    out.push_str("//\n");
    out.push_str("// Auto-generated by Ludwig from the spec above. This file is YOURS\n");
    out.push_str("// after the initial scaffold — fill in each `todo!()` with real logic.\n");
    out.push_str("// Ludwig will not overwrite your edits. Re-run `ludwig verify` to update\n");
    out.push_str("// the trailing stamp when the spec changes.\n\n");

    for ex in &doc.examples {
        let test_fn = example_test_name(&ex.name);
        out.push_str(&format!("/// Example: {}\n", ex.name));
        for s in &ex.steps {
            out.push_str(&format!("/// {} {}\n", keyword_label(s.keyword), s.text));
        }
        out.push_str("#[test]\n");
        out.push_str(&format!("fn {test_fn}() {{\n"));
        out.push_str(&format!(
            "    todo!(\"implement example: {}\");\n",
            escape_rust_string(&ex.name)
        ));
        out.push_str("}\n\n");
    }

    for (idx, inv) in doc.deterministic_invariants().enumerate() {
        let test_fn = invariant_test_name(idx);
        out.push_str(&format!("/// {{deterministic}} {}\n", inv.text));
        out.push_str("#[test]\n");
        out.push_str(&format!("fn {test_fn}() {{\n"));
        out.push_str(&format!(
            "    todo!(\"implement invariant: {}\");\n",
            escape_rust_string(&inv.text)
        ));
        out.push_str("}\n\n");
    }

    for (idx, inv) in doc.property_invariants().enumerate() {
        let test_fn = property_test_name(idx);
        out.push_str(&format!("/// {{property}} {}\n", inv.text));
        out.push_str(
            "/// Universally quantified: this must hold for *all* inputs, not one case.\n\
             /// Drive it with many generated inputs — a `proptest!` / `quickcheck` block,\n\
             /// or a loop over a wide range — so the property is actually exercised.\n",
        );
        out.push_str("#[test]\n");
        out.push_str(&format!("fn {test_fn}() {{\n"));
        out.push_str(&format!(
            "    todo!(\"implement property invariant: {}\");\n",
            escape_rust_string(&inv.text)
        ));
        out.push_str("}\n\n");
    }

    out
}

/// Escape a string so it is safe to drop inside a Rust `"…"` literal. Backslashes
/// MUST be escaped before quotes, otherwise `\` → `\\` would re-escape the `"`
/// emitted by the quote replacement.
fn escape_rust_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- spec: verify-isolates-nested-cargo ----------------------------------

    /// {#b1} An explicit override wins regardless of outer-cargo detection.
    #[test]
    fn target_dir_explicit_override_wins() {
        let cache = std::path::Path::new("/tmp/cache");
        assert_eq!(
            choose_verify_target_dir(Some("/x/override".to_string()), true, cache),
            Some(PathBuf::from("/x/override"))
        );
        assert_eq!(
            choose_verify_target_dir(Some("/x/override".to_string()), false, cache),
            Some(PathBuf::from("/x/override"))
        );
    }

    /// {#b2} No override + detected outer cargo → isolate under the cache dir.
    #[test]
    fn target_dir_nested_under_cargo_isolates() {
        let cache = std::path::Path::new("/tmp/proj/.ludwig/cache");
        let got = choose_verify_target_dir(None, true, cache).expect("should isolate");
        assert!(
            got.starts_with(cache),
            "expected isolation under cache dir, got {got:?}"
        );
    }

    /// {#b3} No override + no outer cargo → inherit the ambient target.
    #[test]
    fn target_dir_top_level_inherits() {
        let cache = std::path::Path::new("/tmp/proj/.ludwig/cache");
        assert_eq!(choose_verify_target_dir(None, false, cache), None);
    }

    #[test]
    fn parses_cargo_test_output() {
        let raw = "\
running 3 tests
test test_example_burst_then_throttle ... ok
test test_example_refill_after_wait ... FAILED
test test_deterministic_invariant_1 ... ignored
";
        let result = parse_output(raw, Some(0));
        assert_eq!(result.tests.len(), 3);
        assert_eq!(result.pass, 1);
        assert_eq!(result.fail, 1);
        assert_eq!(result.skip, 1);
        assert_eq!(result.tests[0].name, "test_example_burst_then_throttle");
    }

    #[test]
    fn parse_output_preserves_nonzero_exit_with_no_tests() {
        // A compile failure: cargo prints rustc errors and no `test … ok` lines,
        // and exits non-zero. parse_output must surface zero tests *and* retain
        // the exit code so the verifier can distinguish "didn't build" from
        // "cargo missing".
        let raw =
            "error[E0425]: cannot find value `foo` in this scope\n --> tests/ludwig_x.rs:3:5\n";
        let result = parse_output(raw, Some(101));
        assert!(result.tests.is_empty());
        assert_eq!(result.exit_code, Some(101));
        assert!(result.raw.contains("E0425"));
    }

    #[test]
    fn example_test_name_slugs_correctly() {
        assert_eq!(
            example_test_name("burst then throttle"),
            "test_example_burst_then_throttle"
        );
        assert_eq!(
            example_test_name("Quoted \"name\" here"),
            "test_example_quoted_name_here"
        );
    }

    #[test]
    fn render_scaffold_escapes_backslashes_in_string_literals() {
        // Regression: a backslash in an example/invariant name produced source
        // like `todo!("…: foo\bar")`, which Rust rejects as an invalid escape.
        let spec = r#"---
id: backslash
title: Backslash
status: draft
version: 1
---

## Intent
A spec whose example name contains a backslash. The generated test
file must escape that backslash so cargo can still compile the file
without manual intervention.

## Behavior
- thing

## Examples
```example name="path\to\thing"
Given a thing
When it runs
Then it works
```

## Invariants
- {deterministic} backslash-laden \w pattern stays a literal.
"#;
        let doc = crate::parser::parse(spec).expect("spec parses");
        let body = super::render_scaffold(&doc);
        // The example name and invariant text round-trip with their backslashes
        // doubled, so the emitted `todo!("…")` is a well-formed Rust literal.
        assert!(
            body.contains(r#"todo!("implement example: path\\to\\thing")"#),
            "example backslashes not escaped:\n{body}"
        );
        assert!(
            body.contains(r#"\\w pattern"#),
            "invariant backslashes not escaped:\n{body}"
        );
    }
}
