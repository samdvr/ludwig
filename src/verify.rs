use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::iso8601;

use crate::adapters::{self, Adapter, TestStatus};
use crate::drift;
use crate::error::VerifyError;
use crate::parser;
use crate::project::Project;
use crate::spec::Document;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub id: String,
    pub spec_version: u32,
    pub spec_hash: String,
    pub spec_path: String,
    pub checks: Vec<Check>,
    pub summary: Summary,
    pub judgment_prompts: Vec<JudgmentPrompt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub kind: String,
    pub name: String,
    pub status: CheckStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The outcome of a single check. Serializes to the same strings the report
/// JSON has always used (`pass`, `fail`, `pending_judgment`, `skip`) via
/// `rename_all`, but as a closed enum every match over it is exhaustive — a new
/// variant can't be silently dropped from [`summarize`] or [`render_text`], and
/// a typo'd status is a compile error rather than a check that vanishes from the
/// summary counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Fail,
    PendingJudgment,
    Skip,
}

impl Check {
    fn new(
        kind: impl Into<String>,
        name: impl Into<String>,
        status: CheckStatus,
        detail: Option<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            name: name.into(),
            status,
            detail,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Summary {
    pub pass: u32,
    pub fail: u32,
    pub pending: u32,
    pub skip: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgmentPrompt {
    pub invariant_key: String,
    pub spec_id: String,
    pub spec_version: u32,
    pub spec_hash: String,
    pub spec_path: String,
    pub invariant_text: String,
    pub evidence_files: Vec<String>,
    pub prompt: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IngestedVerdict {
    pub invariant_key: String,
    pub verdict: crate::project::Verdict,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub spec_id: Option<String>,
    #[serde(default)]
    pub spec_hash: Option<String>,
}

pub struct Verify<'a> {
    project: &'a Project,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RunOptions {
    pub emit_judgment_prompts: bool,
}

impl<'a> Verify<'a> {
    pub fn new(project: &'a Project) -> Self {
        Self { project }
    }

    pub fn run(&self, id: &str, opts: RunOptions) -> Result<Report, crate::Error> {
        let path = self
            .project
            .find_spec_path(id)
            .ok_or_else(|| VerifyError::new(format!("no spec found with id {id:?}")))?;
        self.run_path(&path, opts)
    }

    /// Run the verification pipeline against an already-resolved spec path. The
    /// MCP layer calls this with the path returned by its confinement check so a
    /// single `spec.verify` request does not re-scan and re-parse every spec
    /// twice before doing any real work.
    pub fn run_path(
        &self,
        path: &std::path::Path,
        opts: RunOptions,
    ) -> Result<Report, crate::Error> {
        let doc = parser::parse_file(path)?;
        let spec_path_rel = crate::util::rel_str(&self.project.root, path);

        let judgment_prompts = self.judgment_prompts_for(&doc, &spec_path_rel);

        // Prompts-only mode: the caller just wants the judgment-prompt JSON and
        // does not need a fresh cargo run, a persisted report, or state updates.
        // Short-circuit to avoid the cost (cargo test on a cold target dir can
        // take tens of seconds) and the side effects.
        if opts.emit_judgment_prompts {
            return Ok(Report {
                id: doc.id().to_string(),
                spec_version: doc.version(),
                spec_hash: doc.canonical_hash(),
                spec_path: spec_path_rel,
                checks: Vec::new(),
                summary: Summary::default(),
                judgment_prompts,
            });
        }

        let mut checks: Vec<Check> = Vec::new();
        checks.extend(self.structural_checks(&doc));

        // Render + run the Rust test adapter.
        let adapter = adapters::for_project(self.project);
        let render_info = adapter.render(&doc)?;
        checks.extend(test_file_stamp_check(
            &render_info.spec_file,
            &self.project.root,
            &doc,
        ));
        let run_result = adapter.run(&doc)?;
        checks.extend(deterministic_checks(&run_result));
        checks.extend(missing_test_checks(&doc, &run_result));

        checks.extend(self.judgment_check_stubs(&judgment_prompts)?);

        let summary = summarize(&checks);

        let report = Report {
            id: doc.id().to_string(),
            spec_version: doc.version(),
            spec_hash: doc.canonical_hash(),
            spec_path: spec_path_rel,
            checks,
            summary,
            judgment_prompts,
        };

        self.persist_report(&report)?;
        self.record_state(&doc)?;
        Ok(report)
    }

    pub fn ingest_judgments(&self, json_path: &std::path::Path) -> Result<(), crate::Error> {
        let bytes = fs::read(json_path)
            .map_err(|e| VerifyError::new(format!("read {}: {e}", json_path.display())))?;
        let verdicts: Vec<IngestedVerdict> = serde_json::from_slice(&bytes)
            .map_err(|e| VerifyError::new(format!("parse verdicts: {e}")))?;
        self.apply_judgments(verdicts)
    }

    /// Persist a batch of judgment verdicts to `state.json`. Shared by the
    /// file-based `ingest_judgments` and the MCP `spec.ingest_judgments` tool,
    /// which receives the verdicts inline as a JSON array.
    pub fn apply_judgments(&self, verdicts: Vec<IngestedVerdict>) -> Result<(), crate::Error> {
        // Lock-guarded read-modify-write: load, merge the verdicts, persist —
        // all under the state lock so a concurrent verify/ingest cannot drop
        // either side's writes. See `Project::mutate_state`.
        self.project.mutate_state(|state| {
            for v in verdicts {
                state.judgments.insert(
                    v.invariant_key,
                    crate::project::JudgmentVerdict {
                        verdict: v.verdict,
                        rationale: v.rationale,
                        spec_id: v.spec_id,
                        spec_hash: v.spec_hash,
                    },
                );
            }
            Ok(())
        })?;
        Ok(())
    }

    fn structural_checks(&self, doc: &Document) -> Vec<Check> {
        let mut out: Vec<Check> = Vec::new();
        out.push(Check::new(
            "structural",
            "parseable",
            CheckStatus::Pass,
            Some("parsed cleanly".into()),
        ));
        out.push(Check::new(
            "structural",
            "frontmatter-version",
            CheckStatus::Pass,
            Some(format!("version={}", doc.version())),
        ));

        for pat in &doc.frontmatter.implements {
            let matched = crate::util::matched_files(self.project, pat, false);
            if matched.is_empty() {
                out.push(Check::new(
                    "structural",
                    format!("implements:{pat}"),
                    CheckStatus::Fail,
                    Some(
                        "no files match this glob; either remove from implements: or generate the file"
                            .into(),
                    ),
                ));
                continue;
            }
            for f in matched {
                let rel = crate::util::rel_str(&self.project.root, &f);
                let content = match fs::read_to_string(&f) {
                    Ok(c) => c,
                    Err(e) => {
                        out.push(Check::new(
                            "structural",
                            format!("stamp:{rel}"),
                            CheckStatus::Fail,
                            Some(format!("read failed: {e}")),
                        ));
                        continue;
                    }
                };
                match drift::parse_trailing(&content) {
                    None => out.push(Check::new(
                        "structural",
                        format!("stamp:{rel}"),
                        CheckStatus::Fail,
                        Some("missing trailing `ludwig-spec:` comment".into()),
                    )),
                    Some(stamp) if stamp.id != doc.id() => out.push(Check::new(
                        "structural",
                        format!("stamp:{rel}"),
                        CheckStatus::Fail,
                        Some(format!("stamped for {}, expected {}", stamp.id, doc.id())),
                    )),
                    Some(stamp) if stamp.hash != doc.canonical_hash() => out.push(Check::new(
                        "structural",
                        format!("stamp:{rel}"),
                        CheckStatus::Fail,
                        Some(format!(
                            "spec drifted since stamp ({} → {})",
                            drift::short_hash(stamp.hash),
                            drift::short_hash(&doc.canonical_hash()),
                        )),
                    )),
                    Some(_) => out.push(Check::new(
                        "structural",
                        format!("stamp:{rel}"),
                        CheckStatus::Pass,
                        Some("in sync".into()),
                    )),
                }
            }
        }
        out
    }

    fn judgment_prompts_for(&self, doc: &Document, spec_path_rel: &str) -> Vec<JudgmentPrompt> {
        let mut out: Vec<JudgmentPrompt> = Vec::new();
        let evidence: Vec<String> = doc
            .frontmatter
            .implements
            .iter()
            .flat_map(|g| crate::util::matched_files(self.project, g, false))
            .map(|p| crate::util::rel_str(&self.project.root, &p))
            .collect();

        for (idx, inv) in doc.judgment_invariants().enumerate() {
            let key = format!("{}::judgment::{}", doc.id(), idx + 1);
            let prompt = build_judgment_prompt(doc, inv);
            out.push(JudgmentPrompt {
                invariant_key: key,
                spec_id: doc.id().to_string(),
                spec_version: doc.version(),
                spec_hash: doc.canonical_hash(),
                spec_path: spec_path_rel.to_string(),
                invariant_text: inv.text.clone(),
                evidence_files: evidence.clone(),
                prompt,
            });
        }
        out
    }

    fn judgment_check_stubs(&self, prompts: &[JudgmentPrompt]) -> Result<Vec<Check>, crate::Error> {
        let state = self.project.load_state()?;
        let mut out: Vec<Check> = Vec::with_capacity(prompts.len());
        for p in prompts {
            let stored = state.judgments.get(&p.invariant_key);
            match stored {
                Some(v) if v.spec_hash.as_deref() == Some(p.spec_hash.as_str()) => {
                    let status = match v.verdict {
                        crate::project::Verdict::Pass => CheckStatus::Pass,
                        crate::project::Verdict::Fail => CheckStatus::Fail,
                    };
                    out.push(Check::new(
                        "judgment",
                        truncate(&p.invariant_text, 60),
                        status,
                        v.rationale.clone(),
                    ));
                }
                _ => out.push(Check::new(
                    "judgment",
                    truncate(&p.invariant_text, 60),
                    CheckStatus::PendingJudgment,
                    Some(
                        "awaiting verdict from host agent (run `ludwig verify --ingest-judgments <file>`)"
                            .into(),
                    ),
                )),
            }
        }
        Ok(out)
    }

    fn persist_report(&self, report: &Report) -> Result<(), crate::Error> {
        let dir = self.project.reports_dir();
        fs::create_dir_all(&dir).map_err(|e| VerifyError::new(format!("mkdir reports: {e}")))?;
        let ts = OffsetDateTime::now_utc()
            .format(
                &iso8601::Iso8601::<
                    {
                        iso8601::Config::DEFAULT
                            .set_formatted_components(iso8601::FormattedComponents::DateTime)
                            .encode()
                    },
                >,
            )
            .unwrap_or_else(|_| "ts".to_string())
            .replace(['-', ':'], "")
            .replace('.', "");
        let json_path = dir.join(format!("{}-{ts}.json", report.id));
        let mut bytes = serde_json::to_vec_pretty(report)
            .map_err(|e| VerifyError::new(format!("serialize report: {e}")))?;
        bytes.push(b'\n');
        fs::write(&json_path, &bytes)
            .map_err(|e| VerifyError::new(format!("write {}: {e}", json_path.display())))?;

        let latest = dir.join("latest.md");
        fs::write(&latest, render_text(report))
            .map_err(|e| VerifyError::new(format!("write {}: {e}", latest.display())))?;
        Ok(())
    }

    fn record_state(&self, doc: &Document) -> Result<(), crate::Error> {
        let files: Vec<PathBuf> = doc
            .frontmatter
            .implements
            .iter()
            .flat_map(|g| crate::util::matched_files(self.project, g, false))
            .collect();
        drift::record(self.project, doc, &files)?;
        Ok(())
    }
}

pub fn render_text(report: &Report) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} v{} (hash={})\n",
        report.id,
        report.spec_version,
        drift::short_hash(&report.spec_hash),
    ));
    for c in &report.checks {
        let mark = match c.status {
            CheckStatus::Pass => "ok  ",
            CheckStatus::Fail => "FAIL",
            CheckStatus::PendingJudgment => "pend",
            CheckStatus::Skip => "skip",
        };
        out.push_str(&format!("  [{mark}] {}: {}", c.kind, c.name));
        if let Some(d) = c.detail.as_deref()
            && !d.is_empty()
        {
            out.push_str(&format!(" — {d}"));
        }
        out.push('\n');
    }
    let s = &report.summary;
    out.push_str(&format!(
        "  → pass={} fail={} pending={} skip={}\n",
        s.pass, s.fail, s.pending, s.skip
    ));
    out
}

/// Pull the most useful lines out of raw cargo/rustc output for a failure
/// detail. Prefers rustc error lines (and their `-->` location lines); falls
/// back to the tail of the output. Capped so a report line stays readable.
fn compiler_error_excerpt(raw: &str) -> String {
    let errors: Vec<&str> = raw
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("error") || t.starts_with("-->")
        })
        .take(12)
        .collect();
    let chosen: Vec<&str> = if errors.is_empty() {
        let mut tail: Vec<&str> = raw
            .lines()
            .rev()
            .filter(|l| !l.trim().is_empty())
            .take(8)
            .collect();
        tail.reverse();
        tail
    } else {
        errors
    };
    truncate(chosen.join("\n").trim(), 1200)
}

fn deterministic_checks(run: &crate::adapters::RunResult) -> Vec<Check> {
    let mut out: Vec<Check> = Vec::new();
    for t in &run.tests {
        // Route each generated test to its check kind. Property-invariant tests
        // (`test_property_invariant_*`) are real machine verification now — a
        // generated property test, quantified over many inputs — so they report
        // under the `property` kind rather than being failed as "deferred".
        let (check_kind, item_kind, stripped) =
            if let Some(rest) = t.name.strip_prefix("test_example_") {
                ("deterministic", "example", rest)
            } else if let Some(rest) = t.name.strip_prefix("test_property_invariant_") {
                ("property", "property", rest)
            } else if let Some(rest) = t.name.strip_prefix("test_deterministic_invariant_") {
                ("deterministic", "invariant", rest)
            } else {
                ("deterministic", "invariant", t.name.as_str())
            };
        let name = stripped.replace('_', " ").trim().to_string();
        let status = match t.status {
            TestStatus::Pass => CheckStatus::Pass,
            TestStatus::Fail => CheckStatus::Fail,
            TestStatus::Skip => CheckStatus::Skip,
        };
        let detail = match t.status {
            TestStatus::Skip => Some("test ignored — fill in the `todo!()` body".into()),
            TestStatus::Fail => Some("see report `.ludwig/reports/latest.md` for details".into()),
            TestStatus::Pass => None,
        };
        out.push(Check::new(
            check_kind,
            format!("{item_kind}:{name}"),
            status,
            detail,
        ));
    }
    if run.tests.is_empty() {
        // No parseable `test … ok/FAILED` lines. Distinguish two cases:
        //   - cargo exited non-zero → the test harness almost certainly failed
        //     to compile; surface the actual compiler output instead of a
        //     misleading "is cargo on PATH?" hint.
        //   - cargo exited zero (or unknown) with no tests → genuinely nothing ran.
        let detail = match run.exit_code {
            Some(code) if code != 0 => format!(
                "test harness did not run (cargo exited with code {code}) — likely a compile \
                 error in `tests/ludwig_<slug>.rs`:\n{}",
                compiler_error_excerpt(&run.raw)
            ),
            _ => "no tests reported — check that `cargo` is on PATH and \
                  `tests/ludwig_<slug>.rs` builds"
                .to_string(),
        };
        out.push(Check::new(
            "deterministic",
            "test-runner",
            CheckStatus::Fail,
            Some(detail),
        ));
    }
    // Property invariants are now machine-verified like deterministic ones: the
    // adapter scaffolds a `test_property_invariant_*` test the author fills in,
    // its run result is folded into `checks` above under the `property` kind, and
    // `missing_test_checks` fails loudly if the test is absent. There is nothing
    // status-dependent to add here.
    out
}

/// Compare the test functions cargo actually ran against the set the spec
/// requires. Anything missing from cargo's output is either a brand-new example
/// that the user hasn't backed with a `#[test] fn` yet, or a test the user
/// removed by hand. Either way the spec is unverified along that axis and the
/// report should say so.
fn missing_test_checks(doc: &Document, run: &crate::adapters::RunResult) -> Vec<Check> {
    use std::collections::HashSet;
    let actual: HashSet<&str> = run.tests.iter().map(|t| t.name.as_str()).collect();
    let mut out: Vec<Check> = Vec::new();

    for ex in &doc.examples {
        let expected = crate::adapters::rust::example_test_name(&ex.name);
        if !actual.contains(expected.as_str()) {
            out.push(Check::new(
                "deterministic",
                format!("example:{} (missing)", ex.name),
                CheckStatus::Fail,
                Some(format!(
                    "no `fn {expected}` in tests/ludwig_<slug>.rs; add a #[test] for this example"
                )),
            ));
        }
    }
    for (idx, inv) in doc.deterministic_invariants().enumerate() {
        let expected = crate::adapters::rust::invariant_test_name(idx);
        if !actual.contains(expected.as_str()) {
            out.push(Check::new(
                "deterministic",
                format!("invariant:{} (missing)", truncate(&inv.text, 40)),
                CheckStatus::Fail,
                Some(format!(
                    "no `fn {expected}` in tests/ludwig_<slug>.rs; add a #[test] for this invariant"
                )),
            ));
        }
    }
    for (idx, inv) in doc.property_invariants().enumerate() {
        let expected = crate::adapters::rust::property_test_name(idx);
        if !actual.contains(expected.as_str()) {
            out.push(Check::new(
                "property",
                format!("property:{} (missing)", truncate(&inv.text, 40)),
                CheckStatus::Fail,
                Some(format!(
                    "no `fn {expected}` in tests/ludwig_<slug>.rs; add a property #[test] \
                     (quantified over many inputs) for this invariant"
                )),
            ));
        }
    }
    out
}

/// Verify the test file's trailing `ludwig-spec:` stamp tracks the current spec
/// hash. The adapter's `render` rewrites the stamp in place when one is present,
/// so a mismatch here means the user deleted the stamp entirely.
fn test_file_stamp_check(
    test_file: &std::path::Path,
    root: &std::path::Path,
    doc: &Document,
) -> Vec<Check> {
    let rel = crate::util::rel_str(root, test_file);
    let content = match fs::read_to_string(test_file) {
        Ok(c) => c,
        Err(e) => {
            return vec![Check::new(
                "structural",
                format!("stamp:{rel}"),
                CheckStatus::Fail,
                Some(format!("read failed: {e}")),
            )];
        }
    };
    match drift::parse_trailing(&content) {
        None => vec![Check::new(
            "structural",
            format!("stamp:{rel}"),
            CheckStatus::Fail,
            Some(
                "test file has no trailing `ludwig-spec:` stamp — restore it or delete the file and re-render"
                    .into(),
            ),
        )],
        Some(stamp) if stamp.id != doc.id() => vec![Check::new(
            "structural",
            format!("stamp:{rel}"),
            CheckStatus::Fail,
            Some(format!("stamped for {}, expected {}", stamp.id, doc.id())),
        )],
        Some(stamp) if stamp.hash != doc.canonical_hash() => vec![Check::new(
            "structural",
            format!("stamp:{rel}"),
            CheckStatus::Fail,
            Some(format!(
                "test file stamp drifted ({} → {}); re-run `ludwig verify` to update",
                drift::short_hash(stamp.hash),
                drift::short_hash(&doc.canonical_hash()),
            )),
        )],
        Some(_) => vec![Check::new(
            "structural",
            format!("stamp:{rel}"),
            CheckStatus::Pass,
            Some("in sync".into()),
        )],
    }
}

fn summarize(checks: &[Check]) -> Summary {
    let mut s = Summary::default();
    for c in checks {
        match c.status {
            CheckStatus::Pass => s.pass += 1,
            CheckStatus::Fail => s.fail += 1,
            CheckStatus::PendingJudgment => s.pending += 1,
            CheckStatus::Skip => s.skip += 1,
        }
    }
    s
}

fn build_judgment_prompt(doc: &Document, inv: &crate::spec::Invariant) -> String {
    format!(
        "You are verifying a {{judgment}} invariant on spec `{}` (v{}).\n\n\
Intent of the spec:\n  {}\n\n\
The invariant to judge:\n  {}\n\n\
Read the implementing files (listed in `evidence_files`) and decide\n\
whether the code satisfies this invariant. Respond with a JSON object:\n  \
{{\"verdict\": \"pass\" | \"fail\", \"rationale\": \"one or two sentences\"}}\n\n\
Default to \"fail\" if you are uncertain.",
        doc.id(),
        doc.version(),
        doc.intent,
        inv.text,
    )
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::RunResult;

    /// The on-disk / over-the-wire status strings are part of the report's
    /// contract (consumed by the MCP client and persisted JSON). Pin them so a
    /// future rename of the enum variants can't silently change the format.
    #[test]
    fn check_status_serializes_to_stable_strings() {
        let cases = [
            (CheckStatus::Pass, "\"pass\""),
            (CheckStatus::Fail, "\"fail\""),
            (CheckStatus::PendingJudgment, "\"pending_judgment\""),
            (CheckStatus::Skip, "\"skip\""),
        ];
        for (status, expected) in cases {
            assert_eq!(serde_json::to_string(&status).unwrap(), expected);
            let back: CheckStatus = serde_json::from_str(expected).unwrap();
            assert_eq!(back, status);
        }
    }

    #[test]
    fn verdict_serializes_to_stable_strings() {
        use crate::project::Verdict;
        assert_eq!(serde_json::to_string(&Verdict::Pass).unwrap(), "\"pass\"");
        assert_eq!(serde_json::to_string(&Verdict::Fail).unwrap(), "\"fail\"");
        // A malformed verdict is rejected rather than silently coerced.
        assert!(serde_json::from_str::<Verdict>("\"maybe\"").is_err());
    }

    fn empty_run(exit_code: Option<i32>, raw: &str) -> RunResult {
        RunResult {
            tests: Vec::new(),
            pass: 0,
            fail: 0,
            skip: 0,
            exit_code,
            raw: raw.to_string(),
        }
    }

    ///a non-zero cargo exit with no parsed tests is reported as a
    /// compile failure that surfaces the actual rustc output, not the misleading
    /// "is cargo on PATH?" hint.
    #[test]
    fn empty_tests_with_nonzero_exit_surfaces_compiler_output() {
        let raw = "error[E0425]: cannot find value `foo`\n --> tests/ludwig_hello.rs:3:5\n";
        let checks = deterministic_checks(&empty_run(Some(101), raw));
        let runner = checks
            .iter()
            .find(|c| c.name == "test-runner")
            .expect("runner check");
        let detail = runner.detail.as_deref().unwrap_or("");
        assert!(
            detail.contains("cargo exited with code 101"),
            "got: {detail}"
        );
        assert!(
            detail.contains("E0425"),
            "should include the compiler error: {detail}"
        );
    }

    /// zero exit with no tests keeps the generic "nothing ran" hint.
    #[test]
    fn empty_tests_with_zero_exit_uses_generic_hint() {
        let checks = deterministic_checks(&empty_run(Some(0), ""));
        let runner = checks
            .iter()
            .find(|c| c.name == "test-runner")
            .expect("runner check");
        let detail = runner.detail.as_deref().unwrap_or("");
        assert!(detail.contains("cargo` is on PATH"), "got: {detail}");
    }

    /// A spec body carrying a single `{property}` invariant. Property invariants
    /// are machine-verified by a generated `test_property_invariant_*` test, so
    /// this fixture drives the property-check routing and the missing-test guard.
    fn property_only_spec() -> String {
        "---\n\
         id: prop-policy\n\
         title: Property policy\n\
         status: active\n\
         version: 1\n\
         ---\n\n\
         ## Intent\n\
         A spec whose only invariant is a {property} one, used to pin how the\n\
         verifier routes a generated property test's result to a property check.\n\n\
         ## Behavior\n\
         - {#b1} ident(n) returns n.\n\n\
         ## Examples\n\
         ```example name=\"identity\"\n\
         Given the identity function\n\
         When ident(7) is called\n\
         Then it returns 7\n\
         ```\n\n\
         ## Invariants\n\
         - {property} ident is the identity for all integers.\n"
            .to_string()
    }

    /// A generated property test's cargo verdict is routed to the `property`
    /// check kind and reported as-is — a passing property test passes, a failing
    /// one fails. An active spec can therefore rely on a `{property}` invariant
    /// exactly as it relies on a `{deterministic}` one. See spec
    /// `property-invariants-verified`.
    #[test]
    fn property_invariant_test_result_routes_to_property_check() {
        let run = |status| RunResult {
            tests: vec![crate::adapters::TestResult {
                name: "test_property_invariant_1".into(),
                status,
            }],
            pass: 0,
            fail: 0,
            skip: 0,
            exit_code: Some(0),
            raw: String::new(),
        };
        for (ts, expected) in [
            (TestStatus::Pass, CheckStatus::Pass),
            (TestStatus::Fail, CheckStatus::Fail),
        ] {
            let checks = deterministic_checks(&run(ts));
            let prop = checks
                .iter()
                .find(|c| c.kind == "property")
                .unwrap_or_else(|| panic!("expected a property check, got: {checks:#?}"));
            assert_eq!(
                prop.status, expected,
                "property test verdict {ts:?} must map to the property check status",
            );
        }
    }

    /// An active spec whose `{property}` invariant has no backing test must still
    /// fail: `missing_test_checks` emits a failing `property` check, so the spec
    /// can never silently pass on a property nothing exercised.
    #[test]
    fn missing_property_test_fails_loudly() {
        let doc = crate::parser::parse(&property_only_spec()).unwrap();
        let checks = missing_test_checks(&doc, &empty_run(Some(0), ""));
        let prop = checks
            .iter()
            .find(|c| c.kind == "property")
            .unwrap_or_else(|| panic!("expected a missing-property check, got: {checks:#?}"));
        assert_eq!(prop.status, CheckStatus::Fail);
    }

    #[test]
    fn excerpt_falls_back_to_tail_when_no_error_lines() {
        let raw = "compiling\nlinking\nsomething odd happened\n";
        let excerpt = compiler_error_excerpt(raw);
        assert!(excerpt.contains("something odd happened"));
    }
}
