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
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Check {
    fn new(
        kind: impl Into<String>,
        name: impl Into<String>,
        status: impl Into<String>,
        detail: Option<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            name: name.into(),
            status: status.into(),
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
    pub verdict: String,
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
    pub fn new(project: &'a Project) -> Self { Self { project } }

    pub fn run(&self, id: &str, opts: RunOptions) -> Result<Report, crate::Error> {
        let path = self
            .project
            .find_spec_path(id)
            .ok_or_else(|| VerifyError::new(format!("no spec found with id {id:?}")))?;
        let doc = parser::parse_file(&path)?;
        let spec_path_rel = path
            .strip_prefix(&self.project.root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();

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
        checks.extend(test_file_stamp_check(&render_info.spec_file, &self.project.root, &doc));
        let run_result = adapter.run(&doc)?;
        checks.extend(deterministic_checks(&doc, &run_result));
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
        let mut state = self.project.load_state()?;
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
        self.project.write_state(&state)?;
        Ok(())
    }

    fn structural_checks(&self, doc: &Document) -> Vec<Check> {
        let mut out: Vec<Check> = Vec::new();
        out.push(Check::new("structural", "parseable", "pass", Some("parsed cleanly".into())));
        out.push(Check::new(
            "structural",
            "frontmatter-version",
            "pass",
            Some(format!("version={}", doc.version())),
        ));

        for pat in &doc.frontmatter.implements {
            let matched = crate::util::matched_files(self.project, pat, false);
            if matched.is_empty() {
                out.push(Check::new(
                    "structural",
                    format!("implements:{pat}"),
                    "fail",
                    Some(
                        "no files match this glob; either remove from implements: or generate the file"
                            .into(),
                    ),
                ));
                continue;
            }
            for f in matched {
                let rel = f
                    .strip_prefix(&self.project.root)
                    .unwrap_or(&f)
                    .to_string_lossy()
                    .into_owned();
                let content = match fs::read_to_string(&f) {
                    Ok(c) => c,
                    Err(e) => {
                        out.push(Check::new(
                            "structural",
                            format!("stamp:{rel}"),
                            "fail",
                            Some(format!("read failed: {e}")),
                        ));
                        continue;
                    }
                };
                match drift::parse_trailing(&content) {
                    None => out.push(Check::new(
                        "structural",
                        format!("stamp:{rel}"),
                        "fail",
                        Some("missing trailing `ludwig-spec:` comment".into()),
                    )),
                    Some(stamp) if stamp.id != doc.id() => out.push(Check::new(
                        "structural",
                        format!("stamp:{rel}"),
                        "fail",
                        Some(format!("stamped for {}, expected {}", stamp.id, doc.id())),
                    )),
                    Some(stamp) if stamp.hash != doc.canonical_hash() => out.push(Check::new(
                        "structural",
                        format!("stamp:{rel}"),
                        "fail",
                        Some(format!(
                            "spec drifted since stamp ({} → {})",
                            drift::short_hash(stamp.hash),
                            drift::short_hash(&doc.canonical_hash()),
                        )),
                    )),
                    Some(_) => out.push(Check::new(
                        "structural",
                        format!("stamp:{rel}"),
                        "pass",
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
            .map(|p| {
                p.strip_prefix(&self.project.root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .into_owned()
            })
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
                    let status = if v.verdict == "pass" { "pass" } else { "fail" };
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
                    "pending_judgment",
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
        fs::create_dir_all(&dir)
            .map_err(|e| VerifyError::new(format!("mkdir reports: {e}")))?;
        let ts = OffsetDateTime::now_utc()
            .format(&iso8601::Iso8601::<{
                iso8601::Config::DEFAULT
                    .set_formatted_components(iso8601::FormattedComponents::DateTime)
                    .encode()
            }>)
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
        let mark = match c.status.as_str() {
            "pass" => "ok  ",
            "fail" => "FAIL",
            "pending_judgment" => "pend",
            "skip" => "skip",
            _ => "??  ",
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

fn deterministic_checks(doc: &Document, run: &crate::adapters::RunResult) -> Vec<Check> {
    let mut out: Vec<Check> = Vec::new();
    for t in &run.tests {
        let kind = if t.name.starts_with("test_example_") {
            "example"
        } else {
            "invariant"
        };
        let name = t
            .name
            .strip_prefix("test_example_")
            .or_else(|| t.name.strip_prefix("test_deterministic_invariant_"))
            .unwrap_or(&t.name)
            .replace('_', " ")
            .trim()
            .to_string();
        let status = match t.status {
            TestStatus::Pass => "pass",
            TestStatus::Fail | TestStatus::Error => "fail",
            TestStatus::Skip => "skip",
        };
        let detail = match t.status {
            TestStatus::Skip => Some("test ignored — fill in the `todo!()` body".into()),
            TestStatus::Fail | TestStatus::Error => {
                Some("see report `.ludwig/reports/latest.md` for details".into())
            }
            TestStatus::Pass => None,
        };
        out.push(Check::new("deterministic", format!("{kind}:{name}"), status, detail));
    }
    if run.tests.is_empty() {
        out.push(Check::new(
            "deterministic",
            "test-runner",
            "fail",
            Some(
                "no tests reported — check that `cargo` is on PATH and `tests/ludwig_<slug>.rs` builds".into(),
            ),
        ));
    }
    // Property invariants are parsed but not yet machine-verified. For an active
    // spec we must fail loudly — otherwise the verifier silently green-lights
    // claims it never checked. For draft / deprecated specs the parser has
    // already declined to enforce "verified", so a `skip` is honest.
    let active = doc.frontmatter.is_active();
    for inv in doc.property_invariants() {
        let (status, detail) = if active {
            (
                "fail",
                "property invariants are not yet machine-verified (deferred to v0.2). \
                 An `active` spec cannot rely on unverified invariants — move to draft, \
                 rewrite the invariant as {deterministic}, or downgrade it to {judgment}.",
            )
        } else {
            (
                "skip",
                "property-based generation deferred to v0.2; skipped on non-active spec",
            )
        };
        out.push(Check::new(
            "property",
            truncate(&inv.text, 60),
            status,
            Some(detail.into()),
        ));
    }
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
                "fail",
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
                "fail",
                Some(format!(
                    "no `fn {expected}` in tests/ludwig_<slug>.rs; add a #[test] for this invariant"
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
    let rel = test_file
        .strip_prefix(root)
        .unwrap_or(test_file)
        .to_string_lossy()
        .into_owned();
    let content = match fs::read_to_string(test_file) {
        Ok(c) => c,
        Err(e) => {
            return vec![Check::new(
                "structural",
                format!("stamp:{rel}"),
                "fail",
                Some(format!("read failed: {e}")),
            )];
        }
    };
    match drift::parse_trailing(&content) {
        None => vec![Check::new(
            "structural",
            format!("stamp:{rel}"),
            "fail",
            Some(
                "test file has no trailing `ludwig-spec:` stamp — restore it or delete the file and re-render"
                    .into(),
            ),
        )],
        Some(stamp) if stamp.id != doc.id() => vec![Check::new(
            "structural",
            format!("stamp:{rel}"),
            "fail",
            Some(format!("stamped for {}, expected {}", stamp.id, doc.id())),
        )],
        Some(stamp) if stamp.hash != doc.canonical_hash() => vec![Check::new(
            "structural",
            format!("stamp:{rel}"),
            "fail",
            Some(format!(
                "test file stamp drifted ({} → {}); re-run `ludwig verify` to update",
                drift::short_hash(stamp.hash),
                drift::short_hash(&doc.canonical_hash()),
            )),
        )],
        Some(_) => vec![Check::new(
            "structural",
            format!("stamp:{rel}"),
            "pass",
            Some("in sync".into()),
        )],
    }
}

fn summarize(checks: &[Check]) -> Summary {
    let mut s = Summary::default();
    for c in checks {
        match c.status.as_str() {
            "pass" => s.pass += 1,
            "fail" => s.fail += 1,
            "pending_judgment" => s.pending += 1,
            "skip" => s.skip += 1,
            _ => {}
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
