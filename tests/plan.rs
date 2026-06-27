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

/// `glob_expand` prunes the project's *configured* state dir (not just the
/// default `.ludwig`) so a `**` implements pattern never fingerprints Ludwig's
/// own bookkeeping.
#[test]
fn glob_skips_configured_state_dir() {
    let dir = TempDir::new("ludwig-test");
    std::fs::write(dir.path().join("ludwig.yml"), "state_dir: .bookkeeping\n").unwrap();
    let project = ludwig::project::Project::open(dir.path()).unwrap();
    std::fs::create_dir_all(project.specs_dir()).unwrap();

    // A real implementing file the glob should fingerprint...
    std::fs::create_dir_all(project.root.join("src")).unwrap();
    std::fs::write(project.root.join("src/lib.rs"), "// code\n").unwrap();
    // ...and a decoy inside the configured state dir that must be pruned.
    std::fs::create_dir_all(project.root.join(".bookkeeping")).unwrap();
    std::fs::write(project.root.join(".bookkeeping/leak.rs"), "// state\n").unwrap();

    std::fs::write(
        project.specs_dir().join("globby.spec.md"),
        "---\nid: globby\ntitle: Globby\nstatus: draft\nimplements:\n  - \"**/*.rs\"\nversion: 1\n---\n\n\
         ## Intent\nA spec with a recursive glob in implements, used to confirm the plan \
         fingerprinter walks real source but skips the configured state directory.\n\n\
         ## Behavior\n- {#b1} it exists.\n\n\
         ## Examples\n```example name=\"ok\"\nGiven a thing\nWhen it runs\nThen it works\n```\n\n\
         ## Invariants\n- {deterministic} it stays deterministic.\n",
    )
    .unwrap();

    let brief = ludwig::plan::brief_for(&project, "globby").unwrap();
    let paths: Vec<&str> = brief.implementing_files.iter().map(|f| f.path.as_str()).collect();
    assert!(
        paths.iter().any(|p| p.ends_with("src/lib.rs")),
        "should fingerprint real source: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.contains(".bookkeeping")),
        "must prune the configured state dir: {paths:?}"
    );
}

/// A `_game.md` whose frontmatter block is present but malformed is an authoring
/// error: `Game::load` must surface it, not silently coerce it to an empty game
/// (which would hide a broken manifest behind a directory-named, glossary-less
/// game).
#[test]
fn game_load_rejects_malformed_frontmatter() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let project = ludwig::project::Project::open(dir.path()).unwrap();
    let manifest = project.specs_dir().join("_game.md");
    // Unterminated YAML flow sequence — invalid frontmatter.
    std::fs::write(&manifest, "---\nname: [unterminated\n---\n\n## Glossary\n").unwrap();
    let err = ludwig::game::Game::load(&manifest, &project).unwrap_err();
    assert!(
        err.message.contains("_game.md frontmatter"),
        "got: {}",
        err.message
    );
}
/// that is actually a symlink pointing outside the project must NOT be read or
/// fingerprinted — otherwise the brief would leak the size/sha of an out-of-tree
/// file. `pattern_escapes_root` can't catch this (the string looks in-tree); the
/// runtime `resolved_path_escapes_root` guard must.
#[cfg(unix)]
#[test]
fn brief_does_not_fingerprint_symlink_escaping_root() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let project = ludwig::project::Project::open(dir.path()).unwrap();

    // A secret file living outside the project tree.
    let outside = TempDir::new("ludwig-secret");
    let secret = outside.path().join("secret.txt");
    std::fs::write(&secret, "TOP SECRET").unwrap();

    // An in-tree path that is really a symlink to the out-of-tree secret.
    std::fs::create_dir_all(project.root.join("src")).unwrap();
    symlink(&secret, project.root.join("src/leak.rs")).unwrap();

    std::fs::write(
        project.specs_dir().join("leaky.spec.md"),
        "---\nid: leaky\ntitle: Leaky\nstatus: draft\nimplements:\n  - src/leak.rs\nversion: 1\n---\n\n\
         ## Intent\nThis spec names an in-tree file that is really a symlink to an \
         out-of-tree secret, exercising the runtime symlink confinement guard in the \
         plan fingerprinter so the secret is never read or hashed.\n\n\
         ## Behavior\n- {#b1} it does a thing.\n\n\
         ## Examples\n```example name=\"ok\"\nGiven a thing\nWhen it runs\nThen it works\n```\n\n\
         ## Invariants\n- {deterministic} it stays deterministic.\n",
    )
    .unwrap();

    let brief = ludwig::plan::brief_for(&project, "leaky").unwrap();
    assert!(
        brief.implementing_files.is_empty(),
        "a symlink escaping the project root must not be fingerprinted, got: {:?}",
        brief.implementing_files
    );
}
