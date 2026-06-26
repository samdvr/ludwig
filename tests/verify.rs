mod common;

use common::TempDir;

const TOKEN_BUCKET_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/specs/valid/token_bucket.spec.md"
);

#[test]
fn render_writes_test_file_with_todos_and_stamp() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let project = ludwig::project::Project::open(dir.path()).unwrap();
    let fixture = std::fs::read_to_string(TOKEN_BUCKET_FIXTURE).unwrap();
    std::fs::write(project.specs_dir().join("token_bucket.spec.md"), fixture).unwrap();
    let doc =
        ludwig::parser::parse_file(&project.specs_dir().join("token_bucket.spec.md")).unwrap();

    let adapter = ludwig::adapters::for_project(&project);
    use ludwig::adapters::Adapter;
    let info = adapter.render(&doc).unwrap();
    assert!(info.spec_file.is_file());

    let body = std::fs::read_to_string(&info.spec_file).unwrap();
    assert!(
        body.contains("ludwig-spec: token-bucket-rate-limiter@4 hash="),
        "missing stamp"
    );
    assert!(body.contains("fn test_example_burst_then_throttle()"));
    assert!(body.contains("fn test_example_refill_after_wait()"));
    assert!(body.contains("fn test_deterministic_invariant_1()"));
    assert!(body.contains("todo!"));
    // Doc-comment carries the Gherkin steps from the spec.
    assert!(body.contains("/// Given a limiter with capacity 5"));
}

#[test]
fn render_does_not_overwrite_existing_user_edits() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let project = ludwig::project::Project::open(dir.path()).unwrap();
    let fixture = std::fs::read_to_string(TOKEN_BUCKET_FIXTURE).unwrap();
    std::fs::write(project.specs_dir().join("token_bucket.spec.md"), fixture).unwrap();
    let doc =
        ludwig::parser::parse_file(&project.specs_dir().join("token_bucket.spec.md")).unwrap();

    let adapter = ludwig::adapters::for_project(&project);
    use ludwig::adapters::Adapter;
    let info = adapter.render(&doc).unwrap();
    std::fs::write(&info.spec_file, "// user-owned content\n").unwrap();

    // Re-render must not blow away the user's edits.
    adapter.render(&doc).unwrap();
    let body = std::fs::read_to_string(&info.spec_file).unwrap();
    assert_eq!(body, "// user-owned content\n");
}

#[test]
fn end_to_end_verify_against_real_cargo_project() {
    let dir = TempDir::new("ludwig-test");
    let root = dir.path();

    // Minimal cargo crate skeleton.
    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "verify_target"
version = "0.0.1"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    ludwig::scaffold::init(root).unwrap();
    let project = ludwig::project::Project::open(root).unwrap();

    let spec_md = r#"---
id: doubler
title: Doubler
status: active
implements:
  - src/lib.rs
version: 1
---

## Intent
A function that doubles its integer input. Exists only to exercise
the verify pipeline end-to-end: spec → adapter → user-written test →
cargo test → report. Not a real feature.

## Behavior
- {#b1} double(n) returns 2*n for integer n.

## Examples
```example name="double of three"
Given the doubler
When double(3) is called
Then it returns 6
```

## Invariants
- {deterministic} double(n) is always even.
- {judgment} The implementation reads as ordinary Rust; nothing clever.
"#;
    std::fs::write(project.specs_dir().join("doubler.spec.md"), spec_md).unwrap();
    let doc = ludwig::parser::parse_file(&project.specs_dir().join("doubler.spec.md")).unwrap();

    // Implementing source file with the trailing stamp comment.
    std::fs::write(
        root.join("src").join("lib.rs"),
        format!(
            "pub fn double(n: i64) -> i64 {{ n * 2 }}\n// ludwig-spec: {}@{} hash={}\n",
            doc.id(),
            doc.version(),
            doc.canonical_hash()
        ),
    )
    .unwrap();

    // Scaffold the test file, then replace todo!() with real bodies.
    let adapter = ludwig::adapters::for_project(&project);
    use ludwig::adapters::Adapter;
    let info = adapter.render(&doc).unwrap();
    let scaffold = std::fs::read_to_string(&info.spec_file).unwrap();
    // Sanity check the scaffold included both tests.
    assert!(scaffold.contains("fn test_example_double_of_three"));
    assert!(scaffold.contains("fn test_deterministic_invariant_1"));

    std::fs::write(
        &info.spec_file,
        format!(
            "// ludwig-spec: {}@{} hash={}\n\n\
            use verify_target::double;\n\n\
            #[test]\n\
            fn test_example_double_of_three() {{\n    \
                assert_eq!(double(3), 6);\n\
            }}\n\n\
            #[test]\n\
            fn test_deterministic_invariant_1() {{\n    \
                for i in 0..10 {{ assert_eq!(double(i) % 2, 0); }}\n\
            }}\n",
            doc.id(),
            doc.version(),
            doc.canonical_hash()
        ),
    )
    .unwrap();

    // No env override needed: running under `cargo test` sets CARGO, so the
    // adapter auto-isolates the nested build to this project's own
    // .ludwig/cache/verify-target (see spec `verify-isolates-nested-cargo`).
    // This avoids the previous global-env race between the e2e tests.

    let v = ludwig::verify::Verify::new(&project);
    let report = v.run("doubler", Default::default()).unwrap();
    assert_eq!(
        report.summary.fail, 0,
        "expected no failures, got:\n{:#?}",
        report.checks
    );
    assert!(report.summary.pass >= 2, "expected at least 2 passes");
    assert_eq!(
        report.summary.pending, 1,
        "one judgment invariant should be pending"
    );
}

// -- regression tests for correctness bugs -----------------------------------

#[test]
fn render_updates_stale_stamp_in_existing_test_file() {
    // bug 1: when the spec changes, the trailing stamp on the user-owned test
    // file must track the new hash. Without this, drift detection silently
    // greenlights stale tests.
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let project = ludwig::project::Project::open(dir.path()).unwrap();
    let fixture = std::fs::read_to_string(TOKEN_BUCKET_FIXTURE).unwrap();
    std::fs::write(project.specs_dir().join("token_bucket.spec.md"), fixture).unwrap();
    let doc =
        ludwig::parser::parse_file(&project.specs_dir().join("token_bucket.spec.md")).unwrap();

    let adapter = ludwig::adapters::for_project(&project);
    use ludwig::adapters::Adapter;
    let info = adapter.render(&doc).unwrap();

    // User edits the test file body but keeps the stamp; then injects a stale
    // hash to simulate "spec changed since this file was scaffolded".
    let stale = format!(
        "// ludwig-spec: {}@{} hash=deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef\n\n\
        fn main() {{}}\n",
        doc.id(),
        doc.version(),
    );
    std::fs::write(&info.spec_file, &stale).unwrap();

    adapter.render(&doc).unwrap();
    let body = std::fs::read_to_string(&info.spec_file).unwrap();
    let stamp = ludwig::drift::parse_trailing(&body).expect("stamp must remain after re-render");
    assert_eq!(stamp.hash, doc.canonical_hash(), "stamp should be updated");
    assert!(body.contains("fn main()"), "user body must be preserved");
}

#[test]
fn property_invariant_fails_on_active_spec() {
    // bug 3: an `active` spec with only property invariants used to pass
    // (every property check was `skip`, summary.fail == 0). It must now fail.
    let dir = TempDir::new("ludwig-test");
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "prop_target"
version = "0.0.1"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    ludwig::scaffold::init(root).unwrap();
    let project = ludwig::project::Project::open(root).unwrap();

    let spec_md = r#"---
id: prop-only
title: Property only
status: active
implements:
  - src/lib.rs
version: 1
---

## Intent
A spec that asserts only a {property} invariant. Until property-based
generation lands, an active spec must not be allowed to silently pass
just because its checks were skipped.

## Behavior
- {#b1} ident(n) returns n.

## Examples
```example name="identity"
Given the identity function
When ident(7) is called
Then it returns 7
```

## Invariants
- {property} ident is the identity for all integers.
"#;
    std::fs::write(project.specs_dir().join("prop-only.spec.md"), spec_md).unwrap();
    let doc = ludwig::parser::parse_file(&project.specs_dir().join("prop-only.spec.md")).unwrap();

    std::fs::write(
        root.join("src").join("lib.rs"),
        format!(
            "pub fn ident(n: i64) -> i64 {{ n }}\n// ludwig-spec: {}@{} hash={}\n",
            doc.id(),
            doc.version(),
            doc.canonical_hash()
        ),
    )
    .unwrap();

    let adapter = ludwig::adapters::for_project(&project);
    use ludwig::adapters::Adapter;
    let info = adapter.render(&doc).unwrap();
    std::fs::write(
        &info.spec_file,
        format!(
            "// ludwig-spec: {}@{} hash={}\n\n\
            use prop_target::ident;\n\n\
            #[test]\n\
            fn test_example_identity() {{\n    assert_eq!(ident(7), 7);\n}}\n",
            doc.id(),
            doc.version(),
            doc.canonical_hash()
        ),
    )
    .unwrap();


    // Auto-isolated nested target (see spec `verify-isolates-nested-cargo`).

    let v = ludwig::verify::Verify::new(&project);
    let report = v.run("prop-only", Default::default()).unwrap();
    assert!(
        report.summary.fail >= 1,
        "active spec with only property invariants must fail: {:#?}",
        report.checks
    );
    let property_fail = report
        .checks
        .iter()
        .any(|c| c.kind == "property" && c.status == "fail");
    assert!(property_fail, "property check must report fail on active");
}

#[test]
fn emit_judgment_prompts_skips_adapter_run() {
    // bug 4: `--emit-judgment-prompts` only needs the prompt list, not a full
    // cargo run. Verify the short-circuit: no report file is persisted and the
    // prompts come back even without an implementing file on disk.
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let project = ludwig::project::Project::open(dir.path()).unwrap();

    let spec_md = r#"---
id: emit-only
title: Emit only
status: active
implements:
  - src/missing.rs
version: 1
---

## Intent
A spec used to verify that emit-judgment-prompts skips the adapter run.
The implementing file is intentionally absent — a full verify would
fail on the structural stamp check.

## Behavior
- {#b1} It exists.

## Examples
```example name="exists"
Given a setup
When called
Then it works
```

## Invariants
- {judgment} The implementation reads as ordinary Rust.
"#;
    std::fs::write(project.specs_dir().join("emit-only.spec.md"), spec_md).unwrap();

    let v = ludwig::verify::Verify::new(&project);
    let report = v
        .run(
            "emit-only",
            ludwig::verify::RunOptions { emit_judgment_prompts: true },
        )
        .unwrap();
    assert_eq!(report.judgment_prompts.len(), 1);
    assert!(report.checks.is_empty(), "no checks should run in emit mode");
    assert_eq!(report.summary.fail, 0);
    // No reports/ directory written: the run is side-effect-free.
    assert!(
        !project.reports_dir().join("latest.md").is_file(),
        "emit mode must not persist a report"
    );
}

#[test]
fn missing_example_test_is_flagged() {
    // bug 2: if a spec gains an example after its test file was scaffolded,
    // cargo silently runs only the tests that exist. The verifier must compare
    // expected vs actual test names and fail when one is missing.
    let dir = TempDir::new("ludwig-test");
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "missing_target"
version = "0.0.1"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    ludwig::scaffold::init(root).unwrap();
    let project = ludwig::project::Project::open(root).unwrap();

    let spec_md = r#"---
id: two-examples
title: Two examples
status: active
implements:
  - src/lib.rs
version: 1
---

## Intent
A spec with two examples used to verify the missing-test detection. The
hand-written test file below intentionally implements only the first
example; the second must be flagged as missing by the verifier.

## Behavior
- {#b1} double(n) returns 2*n.

## Examples
```example name="three"
Given the doubler
When double(3) is called
Then it returns 6
```

```example name="four"
Given the doubler
When double(4) is called
Then it returns 8
```

## Invariants
- {deterministic} double(n) is always even.
"#;
    std::fs::write(project.specs_dir().join("two-examples.spec.md"), spec_md).unwrap();
    let doc =
        ludwig::parser::parse_file(&project.specs_dir().join("two-examples.spec.md")).unwrap();

    std::fs::write(
        root.join("src").join("lib.rs"),
        format!(
            "pub fn double(n: i64) -> i64 {{ n * 2 }}\n// ludwig-spec: {}@{} hash={}\n",
            doc.id(),
            doc.version(),
            doc.canonical_hash()
        ),
    )
    .unwrap();

    let adapter = ludwig::adapters::for_project(&project);
    use ludwig::adapters::Adapter;
    let info = adapter.render(&doc).unwrap();
    // Only implement test_example_three and the invariant — `four` is missing.
    std::fs::write(
        &info.spec_file,
        format!(
            "// ludwig-spec: {}@{} hash={}\n\n\
            use missing_target::double;\n\n\
            #[test]\n\
            fn test_example_three() {{\n    assert_eq!(double(3), 6);\n}}\n\n\
            #[test]\n\
            fn test_deterministic_invariant_1() {{\n    for i in 0..10 {{ assert_eq!(double(i) % 2, 0); }}\n}}\n",
            doc.id(),
            doc.version(),
            doc.canonical_hash()
        ),
    )
    .unwrap();


    // Auto-isolated nested target (see spec `verify-isolates-nested-cargo`).

    let v = ludwig::verify::Verify::new(&project);
    let report = v.run("two-examples", Default::default()).unwrap();
    let missing = report
        .checks
        .iter()
        .find(|c| c.name.contains("example:four") && c.status == "fail");
    assert!(
        missing.is_some(),
        "missing example test must be flagged; got:\n{:#?}",
        report.checks
    );
}
