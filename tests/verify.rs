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

    // Isolate the nested cargo build so it doesn't fight the parent target lock.
    let target_dir = TempDir::new("ludwig-cargo-target");
    // SAFETY: tests are single-threaded for this env (cargo test runs each
    // integration test binary in parallel but with separate processes).
    unsafe {
        std::env::set_var(
            "LUDWIG_NESTED_CARGO_TARGET_DIR",
            target_dir.path().display().to_string(),
        );
    }

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
