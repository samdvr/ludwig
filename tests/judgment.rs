mod common;

use common::TempDir;

const GREETER_SPEC: &str = r#"---
id: greeter
title: Greeter
status: active
implements:
  - src/lib.rs
version: 1
---

## Intent
A function returning a friendly greeting. Exists only to exercise the
judgment-prompt round-trip: the host agent must read the implementation
and decide whether the prose-only invariant is satisfied.

## Behavior
- {#b1} greet(name) returns "Hello, <name>!".

## Examples
```example name="named"
Given a greeter
When greet("Alice") is called
Then it returns "Hello, Alice!"
```

## Invariants
- {deterministic} The return value is a non-empty string.
- {judgment} The greeting is in plain English without leetspeak.
"#;

fn make_project_with_greeter() -> (TempDir, ludwig::project::Project, ludwig::spec::Document) {
    let dir = TempDir::new("ludwig-test");
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "greeter_target"
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

    std::fs::write(project.specs_dir().join("greeter.spec.md"), GREETER_SPEC).unwrap();
    let doc = ludwig::parser::parse_file(&project.specs_dir().join("greeter.spec.md")).unwrap();

    // Implementation file with the trailing stamp.
    std::fs::write(
        root.join("src").join("lib.rs"),
        format!(
            "pub fn greet(name: &str) -> String {{ format!(\"Hello, {{name}}!\") }}\n// ludwig-spec: {}@{} hash={}\n",
            doc.id(),
            doc.version(),
            doc.canonical_hash()
        ),
    )
    .unwrap();

    // Scaffold the test file, then replace todo!() with real bodies so the
    // deterministic checks pass — leaving the judgment to be evaluated.
    use ludwig::adapters::Adapter;
    let adapter = ludwig::adapters::for_project(&project);
    let info = adapter.render(&doc).unwrap();
    std::fs::write(
        &info.spec_file,
        format!(
            "// ludwig-spec: {}@{} hash={}\n\n\
            use greeter_target::greet;\n\n\
            #[test]\n\
            fn test_example_named() {{\n    \
                assert_eq!(greet(\"Alice\"), \"Hello, Alice!\");\n\
            }}\n\n\
            #[test]\n\
            fn test_deterministic_invariant_1() {{\n    \
                assert!(!greet(\"anyone\").is_empty());\n\
            }}\n",
            doc.id(),
            doc.version(),
            doc.canonical_hash()
        ),
    )
    .unwrap();

    let target_dir = TempDir::new("ludwig-cargo-target");
    let leaked: &'static str = Box::leak(target_dir.path().display().to_string().into_boxed_str());
    unsafe { std::env::set_var("LUDWIG_NESTED_CARGO_TARGET_DIR", leaked); }
    // Note: we leak the TempDir path (and keep the dir alive for the test) by
    // holding it in the calling test via the returned TempDir wrapper.
    std::mem::forget(target_dir);

    (dir, project, doc)
}

#[test]
fn first_run_marks_judgment_pending() {
    let (_dir, project, doc) = make_project_with_greeter();
    let v = ludwig::verify::Verify::new(&project);
    let report = v.run(doc.id(), Default::default()).unwrap();
    let pending: Vec<_> = report
        .checks
        .iter()
        .filter(|c| c.status == "pending_judgment")
        .collect();
    assert_eq!(pending.len(), 1, "expected exactly one pending judgment");
}

#[test]
fn ingesting_verdict_resolves_pending() {
    let (_dir, project, doc) = make_project_with_greeter();
    let v = ludwig::verify::Verify::new(&project);
    let first = v.run(doc.id(), Default::default()).unwrap();
    let prompt = first.judgment_prompts.first().expect("one judgment prompt");

    let verdicts_path = project.root.join("verdicts.json");
    let body = serde_json::json!([{
        "invariant_key": prompt.invariant_key,
        "verdict": "pass",
        "rationale": "Plain English, no leet.",
        "spec_id": doc.id(),
        "spec_hash": doc.canonical_hash()
    }]);
    std::fs::write(&verdicts_path, serde_json::to_string(&body).unwrap()).unwrap();

    v.ingest_judgments(&verdicts_path).unwrap();

    let second = v.run(doc.id(), Default::default()).unwrap();
    let pending: Vec<_> = second
        .checks
        .iter()
        .filter(|c| c.status == "pending_judgment")
        .collect();
    assert!(pending.is_empty(), "judgment should be resolved");
    let judgments_pass: Vec<_> = second
        .checks
        .iter()
        .filter(|c| c.kind == "judgment" && c.status == "pass")
        .collect();
    assert_eq!(judgments_pass.len(), 1);
}

#[test]
fn ingested_verdict_invalidated_when_spec_changes() {
    let (_dir, project, doc) = make_project_with_greeter();
    let v = ludwig::verify::Verify::new(&project);
    let first = v.run(doc.id(), Default::default()).unwrap();
    let prompt = first.judgment_prompts.first().expect("one judgment prompt");

    let verdicts_path = project.root.join("verdicts.json");
    let body = serde_json::json!([{
        "invariant_key": prompt.invariant_key,
        "verdict": "pass",
        "rationale": "ok",
        "spec_id": doc.id(),
        "spec_hash": "stale-hash"
    }]);
    std::fs::write(&verdicts_path, serde_json::to_string(&body).unwrap()).unwrap();

    v.ingest_judgments(&verdicts_path).unwrap();

    let second = v.run(doc.id(), Default::default()).unwrap();
    let pending: Vec<_> = second
        .checks
        .iter()
        .filter(|c| c.status == "pending_judgment")
        .collect();
    assert_eq!(pending.len(), 1, "stale-hash verdict must not satisfy the check");
}
