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

#[test]
fn record_writes_version_cache() {
    // Issue 24: record() should snapshot the canonical body so we can show a
    // meaningful diff between versions later.
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let project = ludwig::project::Project::open(dir.path()).unwrap();
    let doc = write_doubler_spec(&project);

    ludwig::drift::record(&project, &doc, &[]).unwrap();

    let cache = ludwig::drift::cache_path(&project, doc.id(), doc.version());
    assert!(cache.is_file(), "expected cache file at {}", cache.display());
    let body = std::fs::read_to_string(&cache).unwrap();
    assert!(body.contains("## Intent"), "cache must contain canonical body");
    assert!(body.contains(doc.id()));
}

#[test]
fn drift_distinguishes_version_bump_from_body_edit() {
    // Issue 23: when the stamp's version doesn't match the spec's current
    // version, the detail should explicitly call that out (vs. a same-version
    // body edit, which is the surprising case).
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let project = ludwig::project::Project::open(dir.path()).unwrap();
    let doc = write_doubler_spec(&project);
    let src = project.root.join("src").join("stub.rs");
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    // Stamp claims an older version + different hash.
    std::fs::write(
        &src,
        format!(
            "pub fn call_it() -> i32 {{ 42 }}\n// ludwig-spec: {}@99 hash=deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef\n",
            doc.id(),
        ),
    )
    .unwrap();

    let report = ludwig::drift::report(&project, "stub").unwrap();
    let detail = report.files[0].detail.as_deref().unwrap_or("");
    assert!(
        detail.contains("v99") && detail.contains("v1"),
        "expected detail to call out v99 → v1, got: {detail}"
    );
}

/// Reopen `dir` as a project configured with `canonical: code`. The scaffold
/// always writes `spec`, so flip the config on disk and reload.
fn open_in_code_mode(dir: &TempDir) -> ludwig::project::Project {
    std::fs::write(
        dir.path().join("ludwig.yml"),
        "canonical: code\nspecs_dir: specs\nstate_dir: .ludwig\n",
    )
    .unwrap();
    let project = ludwig::project::Project::open(dir.path()).unwrap();
    assert!(project.canonical_mode().is_code(), "fixture must be in code mode");
    project
}

#[test]
fn code_mode_stale_stamp_points_at_the_spec() {
    // In code mode the code is canonical, so a moved spec hash means the SPEC
    // is the stale side — the remedy must point there, not at "regenerate".
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let project = open_in_code_mode(&dir);
    let doc = write_doubler_spec(&project);

    let src = project.root.join("src").join("stub.rs");
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(
        &src,
        format!(
            "pub fn call_it() -> i32 {{ 42 }}\n// ludwig-spec: {}@{} hash=abc1234abc1234abc1234abc1234abc1234abc1234abc1234abc1234abc1234abc\n",
            doc.id(),
            doc.version(),
        ),
    )
    .unwrap();

    let report = ludwig::drift::report(&project, "stub").unwrap();
    assert_eq!(report.files[0].status, ludwig::drift::FileDriftStatus::StaleStamp);
    let detail = report.files[0].detail.as_deref().unwrap_or("");
    assert!(
        detail.contains("code is canonical") && detail.contains("reconcile the spec"),
        "code-mode stale stamp must point at the spec, got: {detail}"
    );
    assert!(
        !detail.contains("regenerate"),
        "code-mode remedy must not tell the user to regenerate code, got: {detail}"
    );
}

#[test]
fn code_mode_body_changed_says_spec_is_behind() {
    // The headline flip: editing the code in code mode means the spec is now
    // behind and should be updated to match.
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let project = open_in_code_mode(&dir);
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
    ludwig::drift::record(&project, &doc, std::slice::from_ref(&src)).unwrap();

    // Edit the code body, keeping the stamp intact.
    let edited = format!(
        "pub fn call_it() -> i32 {{ 43 }}\n// ludwig-spec: {}@{} hash={}\n",
        doc.id(),
        doc.version(),
        doc.canonical_hash()
    );
    std::fs::write(&src, edited).unwrap();

    let report = ludwig::drift::report(&project, "stub").unwrap();
    assert_eq!(report.files[0].status, ludwig::drift::FileDriftStatus::BodyChanged);
    let detail = report.files[0].detail.as_deref().unwrap_or("");
    assert!(
        detail.contains("spec is now behind") && detail.contains("update the spec"),
        "code-mode body change must say the spec is behind, got: {detail}"
    );
}
