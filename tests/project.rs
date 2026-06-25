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
    assert!(second.len() < first.len(), "second run should write fewer files");
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
    assert_eq!(path.file_name().unwrap().to_str().unwrap(), "found-me.spec.md");
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

    let spec_path =
        ludwig::scaffold::new_spec(&p, "billing/charge", Some("billing")).unwrap();
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
