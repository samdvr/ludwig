mod common;

use common::TempDir;

fn write_token_bucket(project: &ludwig::project::Project) {
    let fixture = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/specs/valid/token_bucket.spec.md"
    ))
    .unwrap();
    std::fs::write(project.specs_dir().join("token_bucket.spec.md"), fixture).unwrap();
}

fn project_with_token_bucket() -> (TempDir, ludwig::project::Project) {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let p = ludwig::project::Project::open(dir.path()).unwrap();
    write_token_bucket(&p);
    (dir, p)
}

#[test]
fn brief_includes_resolved_spec() {
    let (_dir, p) = project_with_token_bucket();
    let brief = ludwig::plan::brief_for(&p, "token-bucket-rate-limiter").unwrap();
    assert_eq!(brief.spec.id, "token-bucket-rate-limiter");
    assert_eq!(brief.spec.version, 4);
    assert_eq!(brief.spec.behaviors.len(), 4);
    assert_eq!(brief.spec.examples.len(), 2);
    assert_eq!(brief.spec.invariants.len(), 3);
}

#[test]
fn brief_includes_game_glossary() {
    let (_dir, p) = project_with_token_bucket();
    std::fs::write(
        p.specs_dir().join("_game.md"),
        "---\nname: root\n---\n\n## Glossary\n- **Tenant**: a customer organization billed independently.\n",
    )
    .unwrap();
    let brief = ludwig::plan::brief_for(&p, "token-bucket-rate-limiter").unwrap();
    assert_eq!(
        brief.game.glossary.get("Tenant").map(|s| s.as_str()),
        Some("a customer organization billed independently.")
    );
}

#[test]
fn brief_resolves_dependencies_transitively() {
    let (_dir, p) = project_with_token_bucket();
    std::fs::write(
        p.specs_dir().join("clock_source.spec.md"),
        r#"---
id: clock-source
title: Clock source
status: draft
version: 1
---

## Intent
A monotonic clock interface used by anything that measures elapsed
time. Decoupling clock access from the system clock makes time-
dependent logic testable. The clock is the only thing in the project
allowed to call out to the real wall clock.

## Behavior
- {#b1} now_seconds returns a monotonic float.

## Examples
```example name="monotonic"
Given a clock source
When now_seconds is called twice
Then the second value is >= the first
```

## Invariants
- {deterministic} The returned value never decreases.
"#,
    )
    .unwrap();

    let brief = ludwig::plan::brief_for(&p, "token-bucket-rate-limiter").unwrap();
    assert_eq!(brief.depends_on.len(), 1);
    let dep = &brief.depends_on[0];
    assert_eq!(dep.id, "clock-source");
    assert!(dep.found);
}

#[test]
fn brief_marks_fresh_for_first_run() {
    let (_dir, p) = project_with_token_bucket();
    let brief = ludwig::plan::brief_for(&p, "token-bucket-rate-limiter").unwrap();
    match brief.regenerating {
        ludwig::plan::RegenHint::Fresh { fresh } => assert!(fresh),
        _ => panic!("expected Fresh"),
    }
}

#[test]
fn spec_from_description_includes_slug_and_description() {
    let prompt = ludwig::prompts::spec_from_description(
        "url-shortener",
        "Map long URLs to short opaque tokens.",
        None,
        &[],
        &[],
    );
    assert!(prompt.contains("url-shortener"));
    assert!(prompt.contains("Map long URLs"));
    assert!(prompt.contains("## Ludwig spec grammar"));
    assert!(prompt.contains("spec.write"));
}

#[test]
fn spec_from_description_includes_peers_and_glossary() {
    let prompt = ludwig::prompts::spec_from_description(
        "login",
        "Authenticate by email + password",
        Some("auth"),
        &[ludwig::prompts::PeerSpec {
            id: "session-store",
            title: "Session store",
        }],
        &[("User".to_string(), "an authenticated principal".to_string())],
    );
    assert!(prompt.contains("session-store"));
    assert!(prompt.contains("Session store"));
    assert!(prompt.contains("User") && prompt.contains("authenticated principal"));
}

#[test]
fn project_decomposition_lists_existing_state() {
    let prompt = ludwig::prompts::project_decomposition(
        "A URL shortener with analytics.",
        &[ludwig::prompts::ExistingSpec {
            id: "old-spec",
            title: "Old",
            status: "active",
        }],
        &["auth".to_string()],
    );
    assert!(prompt.contains("URL shortener"));
    assert!(prompt.contains("old-spec"));
    assert!(prompt.contains("`auth`"));
    assert!(prompt.contains("\"games\""));
    assert!(prompt.contains("\"specs\""));
}
