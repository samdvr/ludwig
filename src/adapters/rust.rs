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
    pub fn new(project: Project) -> Self { Self { project } }

    pub fn tests_dir(&self) -> PathBuf {
        self.project.root.join("tests")
    }

    pub fn test_file_for(&self, id: &str) -> PathBuf {
        self.tests_dir().join(format!("ludwig_{}.rs", file_slug(id)))
    }
}

impl Adapter for RustAdapter {
    fn render(&self, doc: &Document) -> Result<RenderInfo, crate::Error> {
        fs::create_dir_all(self.tests_dir())
            .map_err(|e| VerifyError::new(format!("mkdir tests: {e}")))?;
        let target = self.test_file_for(doc.id());
        // The test file is USER-OWNED after first scaffold. Only write if absent.
        // Drift detection (via the trailing `ludwig-spec:` comment) handles updates.
        if !target.is_file() {
            fs::write(&target, render_scaffold(doc))
                .map_err(|e| VerifyError::new(format!("write {}: {e}", target.display())))?;
        }
        // Return both paths; for the Rust adapter the "spec file" and "steps file" are
        // the same single file. Keep the API uniform for future adapters.
        Ok(RenderInfo { spec_file: target.clone(), steps_file: target })
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

        // Honour an override for the target directory so nested invocations don't
        // contend on the parent build's target lock.
        if let Ok(td) = std::env::var("LUDWIG_NESTED_CARGO_TARGET_DIR") {
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
    let pass = results.iter().filter(|r| r.status == TestStatus::Pass).count() as u32;
    let fail = results
        .iter()
        .filter(|r| matches!(r.status, TestStatus::Fail | TestStatus::Error))
        .count() as u32;
    let skip = results.iter().filter(|r| r.status == TestStatus::Skip).count() as u32;
    RunResult { tests: results, pass, fail, skip, exit_code, raw: output.to_string() }
}

// -- rendering ----------------------------------------------------------------

pub(crate) fn file_slug(id: &str) -> String {
    id.replace(['-', '/'], "_")
}

fn example_test_name(name: &str) -> String {
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

fn invariant_test_name(idx: usize) -> String {
    format!("test_deterministic_invariant_{}", idx + 1)
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
