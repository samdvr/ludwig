mod common;

use common::TempDir;

fn write_doubler_spec(project: &ludwig::project::Project) -> ludwig::spec::Document {
    let spec_md = r#"---
id: stub
title: Stub
status: active
implements:
  - src/stub.rs
version: 1
---

## Intent
Minimal spec used purely to exercise the drift-detection path in
Ludwig's verify pipeline; the implementation just returns a constant
and the test exists only to make the trailing-comment hash mismatch
observable.

## Behavior
- It returns 42.

## Examples
```example name="returns 42"
Given a stub
When called
Then it returns 42
```

## Invariants
- {deterministic} return value is 42.
"#;
    std::fs::write(project.specs_dir().join("stub.spec.md"), spec_md).unwrap();
    ludwig::parser::parse_file(&project.specs_dir().join("stub.spec.md")).unwrap()
}

#[test]
fn drift_detected_when_spec_changes() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let project = ludwig::project::Project::open(dir.path()).unwrap();
    let doc = write_doubler_spec(&project);

    // Implementation file with a BOGUS hash to simulate drift.
    let src = project.root.join("src").join("stub.rs");
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(
        &src,
        format!(
            "pub fn call_it() -> i32 {{ 42 }}\n// ludwig-spec: {}@{} hash=abc1234abc1234abc1234abc1234abc1234abc1234abc1234abc1234abc1234abc\n",
            doc.id(),
            doc.version()
        ),
    )
    .unwrap();

    let report = ludwig::drift::report(&project, "stub").unwrap();
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].status, ludwig::drift::FileDriftStatus::StaleStamp);
}

#[test]
fn drift_reports_missing_when_implements_file_absent() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let project = ludwig::project::Project::open(dir.path()).unwrap();
    let _ = write_doubler_spec(&project);

    let report = ludwig::drift::report(&project, "stub").unwrap();
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].status, ludwig::drift::FileDriftStatus::Missing);
}

#[test]
fn drift_reports_unstamped_when_trailing_comment_absent() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let project = ludwig::project::Project::open(dir.path()).unwrap();
    let _ = write_doubler_spec(&project);
    let src = project.root.join("src").join("stub.rs");
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, "pub fn call_it() -> i32 { 42 }\n").unwrap();

    let report = ludwig::drift::report(&project, "stub").unwrap();
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].status, ludwig::drift::FileDriftStatus::Unstamped);
}

#[test]
fn drift_reports_ok_when_in_sync() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let project = ludwig::project::Project::open(dir.path()).unwrap();
    let doc = write_doubler_spec(&project);
    let src = project.root.join("src").join("stub.rs");
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(
        &src,
        format!(
            "pub fn call_it() -> i32 {{ 42 }}\n// ludwig-spec: {}@{} hash={}\n",
            doc.id(),
            doc.version(),
            doc.canonical_hash()
        ),
    )
    .unwrap();

    let report = ludwig::drift::report(&project, "stub").unwrap();
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].status, ludwig::drift::FileDriftStatus::Ok);
}

#[test]
fn drift_reports_body_changed_after_record() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let project = ludwig::project::Project::open(dir.path()).unwrap();
    let doc = write_doubler_spec(&project);
    let src = project.root.join("src").join("stub.rs");
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    let initial = format!(
        "pub fn call_it() -> i32 {{ 42 }}\n// ludwig-spec: {}@{} hash={}\n",
        doc.id(),
        doc.version(),
        doc.canonical_hash()
    );
    std::fs::write(&src, &initial).unwrap();

    // Record the file in state.json (simulating a previous verify).
    ludwig::drift::record(&project, &doc, std::slice::from_ref(&src)).unwrap();

    // Edit the body without changing the trailing comment.
    let edited = format!(
        "pub fn call_it() -> i32 {{ 43 }}\n// ludwig-spec: {}@{} hash={}\n",
        doc.id(),
        doc.version(),
        doc.canonical_hash()
    );
    std::fs::write(&src, edited).unwrap();

    let report = ludwig::drift::report(&project, "stub").unwrap();
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].status, ludwig::drift::FileDriftStatus::BodyChanged);
}

#[test]
fn trailing_stamp_parses_for_sub_game_ids_with_slashes() {
    // Regression: the stamp regex used to require `[\w-]+` for the id, which
    // silently failed to match the `/` separator that sub-game ids use.
    let stamp = "// ludwig-spec: auth/login@2 hash=abc1234abc1234abc1234abc1234abc1234abc1234abc1234abc1234abc1234ab";
    let parsed = ludwig::drift::parse_trailing(stamp).expect("stamp must parse");
    assert_eq!(parsed.id, "auth/login");
    assert_eq!(parsed.version, 2);
}
