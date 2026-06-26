mod common;
use common::TempDir;

#[test]
fn init_creates_skeleton() {
    let dir = TempDir::new("ludwig-test");
    let written = ludwig::scaffold::init(dir.path()).expect("init");
    assert!(!written.is_empty());
    assert!(dir.path().join("ludwig.yml").is_file());
    assert!(dir.path().join("specs").is_dir());
    assert!(dir.path().join(".ludwig").is_dir());
    assert!(dir.path().join(".ludwig").join("state.json").is_file());
}

#[test]
fn init_is_idempotent() {
    let dir = TempDir::new("ludwig-test");
    let first = ludwig::scaffold::init(dir.path()).unwrap();
    let second = ludwig::scaffold::init(dir.path()).unwrap();
    assert!(
        second.len() < first.len(),
        "second run should write fewer files"
    );
}

#[test]
fn discover_walks_upward() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let nested = dir.path().join("deeply").join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let p = ludwig::project::Project::discover(&nested).expect("discover");
    assert_eq!(
        p.root.canonicalize().unwrap(),
        dir.path().canonicalize().unwrap()
    );
}

#[test]
fn discover_fails_outside_project() {
    let dir = TempDir::new("ludwig-test");
    let err = ludwig::project::Project::discover(dir.path()).expect_err("must fail");
    assert!(err.0.to_lowercase().contains("no ludwig.yml"));
}

#[test]
fn new_spec_scaffolds_a_parseable_template() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let p = ludwig::project::Project::open(dir.path()).unwrap();
    let target = ludwig::scaffold::new_spec(&p, "my-thing", None).expect("new_spec");
    assert!(target.is_file());
    let doc = ludwig::parser::parse_file(&target).expect("parses");
    assert_eq!(doc.id(), "my-thing");
    assert!(doc.frontmatter.is_draft());
}

#[test]
fn new_spec_rejects_invalid_slug() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let p = ludwig::project::Project::open(dir.path()).unwrap();
    let err = ludwig::scaffold::new_spec(&p, "Bad_Slug!", None).expect_err("must fail");
    assert!(err.0.contains("slug must be"), "got: {}", err.0);
}

#[test]
fn state_round_trip() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let p = ludwig::project::Project::open(dir.path()).unwrap();
    let mut state = p.load_state().unwrap();
    state.specs.insert(
        "foo".to_string(),
        ludwig::project::SpecState {
            version: 1,
            hash: "abc".to_string(),
            implementing_files: Default::default(),
        },
    );
    p.write_state(&state).unwrap();
    let reloaded = p.load_state().unwrap();
    assert_eq!(reloaded.specs.get("foo").unwrap().hash, "abc");
}

#[test]
fn find_spec_path_by_id() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let p = ludwig::project::Project::open(dir.path()).unwrap();
    ludwig::scaffold::new_spec(&p, "found-me", None).unwrap();
    let path = p.find_spec_path("found-me").expect("found");
    assert_eq!(
        path.file_name().unwrap().to_str().unwrap(),
        "found-me.spec.md"
    );
}

#[test]
fn catalog_renders_grouped_by_game() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let p = ludwig::project::Project::open(dir.path()).unwrap();

    let auth_dir = p.specs_dir().join("auth");
    std::fs::create_dir_all(&auth_dir).unwrap();
    std::fs::write(
        auth_dir.join("_game.md"),
        "---\nname: auth\n---\n\n## Glossary\n- **User**: an authenticated principal.\n",
    )
    .unwrap();

    ludwig::scaffold::new_spec(&p, "auth/login", Some("auth")).unwrap();
    ludwig::scaffold::new_spec(&p, "stand-alone", None).unwrap();

    let output = ludwig::catalog::render(&p);
    assert!(output.contains("auth"));
    assert!(output.contains("auth/login"));
    assert!(output.contains("stand-alone"));
}

#[test]
fn glossary_inherits_from_parent_directory() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let p = ludwig::project::Project::open(dir.path()).unwrap();

    std::fs::write(
        p.specs_dir().join("_game.md"),
        "---\nname: root\n---\n\n## Glossary\n- **Tenant**: a customer organization.\n",
    )
    .unwrap();

    let billing = p.specs_dir().join("billing");
    std::fs::create_dir_all(&billing).unwrap();
    std::fs::write(
        billing.join("_game.md"),
        "---\nname: billing\n---\n\n## Glossary\n- **Invoice**: a monthly statement.\n",
    )
    .unwrap();

    let spec_path = ludwig::scaffold::new_spec(&p, "billing/charge", Some("billing")).unwrap();
    let game = ludwig::game::Game::for_spec(&p, &spec_path);

    assert_eq!(game.name, "billing");
    assert_eq!(
        game.glossary.get("Tenant").map(|s| s.as_str()),
        Some("a customer organization.")
    );
    assert_eq!(
        game.glossary.get("Invoice").map(|s| s.as_str()),
        Some("a monthly statement.")
    );
}

#[test]
fn move_spec_relocates_and_cleans_source_dir() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let p = ludwig::project::Project::open(dir.path()).unwrap();
    std::fs::create_dir_all(p.specs_dir().join("billing")).unwrap();
    ludwig::scaffold::new_spec(&p, "billing/charge", Some("billing")).unwrap();
    assert!(
        p.specs_dir()
            .join("billing")
            .join("charge.spec.md")
            .is_file()
    );

    let target = ludwig::scaffold::move_spec(&p, "billing/charge", Some("auth"), false).unwrap();
    assert!(target.is_file());
    assert!(p.specs_dir().join("auth").join("charge.spec.md").is_file());
    assert!(
        !p.specs_dir()
            .join("billing")
            .join("charge.spec.md")
            .is_file()
    );
    // Source dir is now empty and was cleaned up.
    assert!(!p.specs_dir().join("billing").is_dir());
}

#[test]
fn move_spec_refuses_overwrite_without_force() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let p = ludwig::project::Project::open(dir.path()).unwrap();
    ludwig::scaffold::new_spec(&p, "thing", None).unwrap();
    // Hand-write a conflicting destination with the same id under auth/.
    let auth_dir = p.specs_dir().join("auth");
    std::fs::create_dir_all(&auth_dir).unwrap();
    std::fs::write(auth_dir.join("thing.spec.md"), "placeholder").unwrap();

    let err =
        ludwig::scaffold::move_spec(&p, "thing", Some("auth"), false).expect_err("must refuse");
    assert!(err.0.contains("already exists"));
}

// -- spec: atomic-state-writes -----------------------------------------------

fn sample_state() -> ludwig::project::State {
    let mut s = ludwig::project::State::default();
    s.specs.insert(
        "demo".to_string(),
        ludwig::project::SpecState {
            version: 1,
            hash: "abc".to_string(),
            implementing_files: Default::default(),
        },
    );
    s.last_run = Some("now".to_string());
    s
}

/// {deterministic} A load immediately following a write round-trips.
#[test]
fn state_write_round_trips() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let project = ludwig::project::Project::open(dir.path()).unwrap();
    project.write_state(&sample_state()).unwrap();
    let loaded = project.load_state().unwrap();
    assert_eq!(loaded.specs.get("demo").unwrap().hash, "abc");
    assert_eq!(loaded.last_run.as_deref(), Some("now"));
}

/// {deterministic} After a successful write the state dir holds exactly one
/// regular file (state.json) — a temp-then-rename impl must not leave residue.
#[test]
fn state_write_leaves_no_temp_residue() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let project = ludwig::project::Project::open(dir.path()).unwrap();
    project.write_state(&sample_state()).unwrap();
    let mut files: Vec<String> = std::fs::read_dir(project.state_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(
        files,
        vec!["state.json".to_string()],
        "unexpected files: {files:?}"
    );
}

/// {#b4} The state directory is created first if it does not yet exist.
#[test]
fn state_write_creates_state_dir_if_absent() {
    let dir = TempDir::new("ludwig-test");
    std::fs::write(dir.path().join("ludwig.yml"), "canonical: spec\n").unwrap();
    let project = ludwig::project::Project::open(dir.path()).unwrap();
    assert!(!project.state_dir().is_dir());
    project.write_state(&sample_state()).unwrap();
    assert!(project.state_path().is_file());
}

// -- atomic / guarded writes ---------------------------------------

/// write_guarded with overwrite=false refuses to clobber an existing file
/// (create_new closes the TOCTOU window an is_file() pre-check leaves open).
#[test]
fn write_guarded_refuses_to_clobber_when_not_overwriting() {
    let dir = TempDir::new("ludwig-test");
    let target = dir.path().join("f.txt");
    std::fs::write(&target, b"original").unwrap();

    let err = ludwig::util::write_guarded(&target, b"new", false).expect_err("must refuse");
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    // Original content is untouched.
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
}

/// write_guarded with overwrite=true replaces the file atomically and leaves no
/// temp residue behind.
#[test]
fn write_guarded_overwrites_atomically_without_residue() {
    let dir = TempDir::new("ludwig-test");
    let target = dir.path().join("f.txt");
    std::fs::write(&target, b"original").unwrap();

    ludwig::util::write_guarded(&target, b"replaced", true).unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "replaced");

    let stray: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "f.txt")
        .collect();
    assert!(stray.is_empty(), "unexpected leftover files: {stray:?}");
}

/// new_spec twice for the same id errors the second time rather than silently
/// overwriting the first.
#[test]
fn new_spec_refuses_duplicate() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let p = ludwig::project::Project::open(dir.path()).unwrap();
    ludwig::scaffold::new_spec(&p, "thing", None).unwrap();
    let err = ludwig::scaffold::new_spec(&p, "thing", None).expect_err("must refuse");
    assert!(err.0.contains("already exists"));
}

const MINIMAL_SPEC: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/specs/valid/minimal.spec.md"
));

/// Two spec files declaring the same frontmatter id are flagged by the index,
/// and `by_id` keeps exactly one entry (first-seen path) so downstream lookups
/// stay deterministic.
#[test]
fn index_specs_flags_duplicate_ids() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let p = ludwig::project::Project::open(dir.path()).unwrap();
    std::fs::write(p.specs_dir().join("copy-a.spec.md"), MINIMAL_SPEC).unwrap();
    std::fs::write(p.specs_dir().join("copy-b.spec.md"), MINIMAL_SPEC).unwrap();

    let index = p.index_specs();
    assert!(index.parse_errors.is_empty());
    assert_eq!(index.by_id.len(), 1, "the shared id collapses to one entry");
    assert_eq!(index.duplicates.len(), 1, "the duplicate id is reported");
    let (_, paths) = index.duplicates.iter().next().unwrap();
    assert_eq!(paths.len(), 2);
}

/// A spec that fails to parse is surfaced in `parse_errors` rather than silently
/// dropped — the gap that let `verify --all` green-light a broken spec.
#[test]
fn index_specs_collects_parse_errors() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let p = ludwig::project::Project::open(dir.path()).unwrap();
    std::fs::write(p.specs_dir().join("good.spec.md"), MINIMAL_SPEC).unwrap();
    std::fs::write(p.specs_dir().join("broken.spec.md"), "not a real spec\n").unwrap();

    let index = p.index_specs();
    assert_eq!(index.by_id.len(), 1);
    assert_eq!(index.parse_errors.len(), 1);
    assert!(index.parse_errors[0].0.ends_with("broken.spec.md"));
}

/// A `ludwig.yml` that points `specs_dir` outside the project root (here via a
/// `..` segment) is rejected at open time so no derived path can escape.
#[test]
fn config_with_escaping_specs_dir_is_rejected() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    std::fs::write(dir.path().join("ludwig.yml"), "specs_dir: ../escape\n").unwrap();
    let err = ludwig::project::Project::open(dir.path()).expect_err("must reject");
    assert!(err.0.contains("specs_dir"), "got: {}", err.0);
}

/// `canonical:` is a closed enum, so an unknown value is a hard error at load
/// time rather than a string that silently behaves like neither mode.
#[test]
fn config_with_unknown_canonical_is_rejected() {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    std::fs::write(dir.path().join("ludwig.yml"), "canonical: coed\n").unwrap();
    let err = ludwig::project::Project::open(dir.path()).expect_err("must reject");
    assert!(err.0.contains("canonical"), "got: {}", err.0);
}

/// Both valid canonical values load and round-trip to the typed enum.
#[test]
fn config_accepts_both_canonical_modes() {
    use ludwig::project::Canonical;
    for (yaml, expected) in [("canonical: spec\n", Canonical::Spec), ("canonical: code\n", Canonical::Code)] {
        let dir = TempDir::new("ludwig-test");
        ludwig::scaffold::init(dir.path()).unwrap();
        std::fs::write(dir.path().join("ludwig.yml"), yaml).unwrap();
        let p = ludwig::project::Project::open(dir.path()).unwrap();
        assert_eq!(p.canonical_mode(), expected, "for {yaml:?}");
    }
}

/// R1 regression: `mutate_state` must serialize concurrent read-modify-writes
/// so no update is lost. Many threads each insert a distinct judgment key under
/// the lock; afterward every key must be present. With the previous unlocked
/// load→mutate→write, interleaving would drop most of them (last-writer-wins).
#[test]
fn mutate_state_serializes_concurrent_writers() {
    use ludwig::project::{JudgmentVerdict, Project, Verdict};
    use std::sync::Arc;

    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let project = Arc::new(Project::open(dir.path()).unwrap());

    let n = 16;
    let handles: Vec<_> = (0..n)
        .map(|i| {
            let project = Arc::clone(&project);
            std::thread::spawn(move || {
                project
                    .mutate_state(|state| {
                        state.judgments.insert(
                            format!("spec::judgment::{i}"),
                            JudgmentVerdict {
                                verdict: Verdict::Pass,
                                rationale: None,
                                spec_id: None,
                                spec_hash: None,
                            },
                        );
                        Ok(())
                    })
                    .expect("mutate_state");
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let state = project.load_state().unwrap();
    assert_eq!(
        state.judgments.len(),
        n,
        "every concurrent writer's verdict must survive — no lost updates"
    );
}
