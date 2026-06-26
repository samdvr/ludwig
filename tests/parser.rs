use std::path::PathBuf;

fn fixture(kind: &str, name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/specs");
    p.push(kind);
    p.push(name);
    p
}

fn parse_valid(name: &str) -> ludwig::spec::Document {
    ludwig::parser::parse_file(&fixture("valid", name)).expect("fixture parses")
}

fn parse_invalid(name: &str) -> ludwig::error::ParseError {
    ludwig::parser::parse_file(&fixture("invalid", name)).expect_err("fixture must fail")
}

// -- happy path ---------------------------------------------------------------

#[test]
fn parses_token_bucket_fixture() {
    let doc = parse_valid("token_bucket.spec.md");
    assert_eq!(doc.id(), "token-bucket-rate-limiter");
    assert_eq!(doc.version(), 4);
    assert!(doc.frontmatter.is_active());
    assert_eq!(doc.frontmatter.owners, vec!["sam".to_string()]);
    assert_eq!(
        doc.frontmatter.implements,
        vec!["src/rate_limit/token_bucket.rs".to_string()]
    );
    assert_eq!(doc.frontmatter.depends_on, vec!["clock-source".to_string()]);
}

#[test]
fn intent_word_count_recorded() {
    let doc = parse_valid("token_bucket.spec.md");
    assert!(!doc.intent.is_empty());
    assert!(doc.intent.split_whitespace().count() >= 20);
}

#[test]
fn behaviors_parsed_with_tags() {
    let doc = parse_valid("token_bucket.spec.md");
    assert_eq!(doc.behaviors.len(), 4);
    assert_eq!(doc.behavior_tags(), vec!["b1", "b2", "b3", "b4"]);
    assert!(doc.behaviors[0].text.contains("capacity"));
}

#[test]
fn examples_have_gherkin_steps() {
    let doc = parse_valid("token_bucket.spec.md");
    assert_eq!(doc.examples.len(), 2);
    let first = &doc.examples[0];
    assert_eq!(first.name, "burst then throttle");
    assert_eq!(first.given_steps().count(), 1);
    assert_eq!(first.when_steps().count(), 1);
    assert!(first.then_steps().count() >= 2); // Then + And
}

#[test]
fn invariants_classified() {
    let doc = parse_valid("token_bucket.spec.md");
    assert_eq!(doc.deterministic_invariants().count(), 1);
    assert_eq!(doc.property_invariants().count(), 1);
    assert_eq!(doc.judgment_invariants().count(), 1);
}

#[test]
fn minimal_spec_parses() {
    let doc = parse_valid("minimal.spec.md");
    assert_eq!(doc.id(), "hello-greeter");
    assert_eq!(doc.behaviors.len(), 2);
    assert_eq!(doc.examples.len(), 2);
}

#[test]
fn canonical_hash_stable_across_whitespace() {
    let raw = std::fs::read_to_string(fixture("valid", "minimal.spec.md")).unwrap();
    let a = ludwig::parser::parse(&raw).unwrap().canonical_hash();
    let perturbed = raw.replace('\n', "  \r\n");
    let b = ludwig::parser::parse(&perturbed).unwrap().canonical_hash();
    assert_eq!(a, b);
}

#[test]
fn canonical_hash_changes_when_content_changes() {
    let raw = std::fs::read_to_string(fixture("valid", "minimal.spec.md")).unwrap();
    let a = ludwig::parser::parse(&raw).unwrap().canonical_hash();
    let altered = raw.replace("returns \"Hello, Alice!\"", "returns \"Hi, Alice!\"");
    let b = ludwig::parser::parse(&altered).unwrap().canonical_hash();
    assert_ne!(a, b);
}

// -- error paths --------------------------------------------------------------

#[test]
fn missing_required_section() {
    let err = parse_invalid("missing_section.spec.md");
    assert!(err.message.contains("missing required section"), "got: {}", err.message);
    assert!(err.message.contains("Behavior"), "got: {}", err.message);
}

#[test]
fn out_of_order_sections() {
    let err = parse_invalid("out_of_order.spec.md");
    assert!(err.message.to_lowercase().contains("out of order"), "got: {}", err.message);
}

#[test]
fn mixed_classifiers_in_invariant() {
    let err = parse_invalid("mixed_classifiers.spec.md");
    assert!(
        err.message.contains("more than one classifier"),
        "got: {}",
        err.message
    );
}

#[test]
fn active_status_blocked_by_open_questions() {
    let err = parse_invalid("active_with_open_questions.spec.md");
    assert!(
        err.message.contains("status: active") && err.message.contains("Open questions"),
        "got: {}",
        err.message
    );
}

#[test]
fn missing_frontmatter() {
    let err = ludwig::parser::parse("## Intent\nhi\n").expect_err("should fail");
    assert!(
        err.message.contains("must begin with YAML frontmatter"),
        "got: {}",
        err.message
    );
}

#[test]
fn unterminated_frontmatter() {
    let err = ludwig::parser::parse("---\nid: x\n\n## Intent\n").expect_err("should fail");
    assert!(err.message.contains("not terminated"), "got: {}", err.message);
}

#[test]
fn intent_below_word_minimum() {
    let spec = r#"---
id: short
title: Short
status: draft
version: 1
---

## Intent
Too short.

## Behavior
- thing

## Examples
```example name="x"
Given a thing
When it runs
Then it works
```

## Invariants
- {deterministic} ok
"#;
    let err = ludwig::parser::parse(spec).expect_err("should fail");
    assert!(
        err.message.contains("Intent must be at least"),
        "got: {}",
        err.message
    );
}

#[test]
fn example_must_declare_name() {
    let spec = r#"---
id: noname
title: No name
status: draft
version: 1
---

## Intent
This spec leaves the example name attribute off the fence, which the
parser must reject because tests are keyed by example name and an
anonymous block has no stable identity.

## Behavior
- thing

## Examples
```example
Given a thing
When it runs
Then it works
```

## Invariants
- {deterministic} ok
"#;
    let err = ludwig::parser::parse(spec).expect_err("should fail");
    assert!(
        err.message.contains("Example fence must declare name"),
        "got: {}",
        err.message
    );
}

#[test]
fn gherkin_steps_must_start_with_given() {
    let spec = r#"---
id: badgherkin
title: Bad Gherkin
status: draft
version: 1
---

## Intent
A spec whose example block starts with When instead of Given. The
parser must reject this because state must be established before a
call, even if the call is trivial.

## Behavior
- thing

## Examples
```example name="x"
When it runs
Then it works
```

## Invariants
- {deterministic} ok
"#;
    let err = ludwig::parser::parse(spec).expect_err("should fail");
    assert!(
        err.message.contains("first step must be `Given`"),
        "got: {}",
        err.message
    );
}

#[test]
fn duplicate_behavior_tag() {
    let spec = r#"---
id: duptag
title: Duplicate tag
status: draft
version: 1
---

## Intent
A spec that uses the same behavior tag twice. Tags are how Examples
and Invariants reference behavior bullets, so they must be unique
within a spec.

## Behavior
- {#b1} first
- {#b1} second

## Examples
```example name="x"
Given a thing
When it runs
Then it works
```

## Invariants
- {deterministic} ok
"#;
    let err = ludwig::parser::parse(spec).expect_err("should fail");
    assert!(
        err.message.contains("duplicate behavior tag"),
        "got: {}",
        err.message
    );
}

#[test]
fn example_names_that_slugify_to_the_same_test_fn_are_rejected() {
    // "burst then throttle" and "burst-then-throttle" both become the test fn
    // `test_example_burst_then_throttle`, which would fail to compile.
    let spec = r#"---
id: collide
title: Slug Collision
status: draft
version: 1
---

## Intent
A spec whose two example names look distinct to humans but slugify to
the same Rust test function. The parser must reject this so the
generated test file is guaranteed to compile cleanly.

## Behavior
- thing

## Examples
```example name="burst then throttle"
Given a thing
When it runs
Then it works
```

```example name="burst-then-throttle"
Given a thing
When it runs
Then it works
```

## Invariants
- {deterministic} ok
"#;
    let err = ludwig::parser::parse(spec).expect_err("should fail");
    assert!(
        err.message.contains("slugify"),
        "expected slug-collision error, got: {}",
        err.message
    );
}

#[test]
fn frontmatter_version_above_u32_max_is_rejected() {
    let spec = r#"---
id: bigver
title: Big Version
status: draft
version: 5000000000
---

## Intent
A spec whose version overflows u32. The parser must reject this so we
don't silently wrap the value when casting to the canonical `u32`
representation used everywhere downstream.

## Behavior
- thing

## Examples
```example name="x"
Given a thing
When it runs
Then it works
```

## Invariants
- {deterministic} ok
"#;
    let err = ludwig::parser::parse(spec).expect_err("should fail");
    assert!(
        err.message.contains("version") && err.message.contains("<="),
        "expected version-overflow error, got: {}",
        err.message
    );
}

#[test]
fn open_questions_must_be_bulleted() {
    // Open questions used to accept prose as a fallback; every other section
    // requires bullets. Make policy uniform.
    let spec = r#"---
id: prose-oq
title: Prose open questions
status: draft
version: 1
---

## Intent
A spec whose Open questions section contains a paragraph rather than a
bulleted list. The parser must refuse this so the formatting of every
section follows the same rule.

## Behavior
- thing

## Examples
```example name="x"
Given a thing
When it runs
Then it works
```

## Invariants
- {deterministic} ok

## Open questions
This is prose, not a bullet, so the parser must reject it.
"#;
    let err = ludwig::parser::parse(spec).expect_err("should fail");
    assert!(
        err.message.contains("Open questions")
            && err.message.to_lowercase().contains("bullet"),
        "expected bullets-required error, got: {}",
        err.message
    );
}

/// `implements:` patterns are spec-controlled and can arrive via the untrusted
/// spec.write tool. A pattern that escapes the project tree (absolute, drive
/// prefix, or `..`) must be rejected at validation time so verify/drift can
/// never expand it into an out-of-project read. See spec `mcp-path-confinement`.
#[test]
fn rejects_implements_that_escape_project_root() {
    let template = |impl_line: &str| {
        format!(
            r#"---
id: escaper
title: Escaper
status: draft
owners: []
implements:
  - {impl_line}
depends_on: []
version: 1
---

## Intent
A spec whose implements entry tries to point outside the project root, which
the validator must reject before any reader expands the pattern against disk.

## Behavior
- {{#b1}} does a thing

## Examples
```example name="x"
Given a thing
When it runs
Then it works
```

## Invariants
- {{deterministic}} ok
"#
        )
    };
    for bad in ["../../etc/passwd", "/etc/passwd", "src/../../secret"] {
        let err = ludwig::parser::parse(&template(bad)).expect_err("must reject escaping implements");
        assert!(
            err.message.contains("implements"),
            "expected an implements-confinement error for {bad:?}, got: {}",
            err.message
        );
    }
    // A legitimate in-tree pattern still parses.
    ludwig::parser::parse(&template("src/lib.rs")).expect("in-tree implements must parse");
}

// -- golden canonical hash (D2: detect serializer/format drift) --------------

/// A frozen spec literal whose canonical hash is pinned below. The canonical
/// hash is persisted in file stamps and `state.json`, so if the serialization
/// of the canonical body ever changes — e.g. a `serde_yaml` upgrade alters
/// frontmatter quoting/spacing — every stamp in the wild would silently
/// invalidate and report mass drift. This golden value makes that a loud test
/// failure instead. If you intentionally change the canonical form, bump the
/// pinned hash here in the same commit.
const GOLDEN_SPEC: &str = "---\n\
id: golden-fixture\n\
title: Golden fixture\n\
status: active\n\
owners: []\n\
implements: []\n\
depends_on: []\n\
version: 1\n\
---\n\n\
## Intent\n\
A frozen specification used purely to pin the canonical hashing format so a\n\
dependency upgrade cannot silently change every stamp in existing projects.\n\
There is no behavior here beyond being stable.\n\n\
## Behavior\n\
- {#b1} It exists and never changes.\n\n\
## Examples\n\
```example name=\"trivial\"\n\
Given the golden fixture\n\
When it is hashed\n\
Then the hash equals the pinned value\n\
```\n\n\
## Invariants\n\
- {deterministic} The canonical hash equals the pinned constant.\n";

#[test]
fn golden_canonical_hash_is_pinned() {
    let doc = ludwig::parser::parse(GOLDEN_SPEC).expect("golden spec parses");
    assert_eq!(
        doc.canonical_hash(),
        "91358482e1abffa17ab113cfbe84fd1a7e1c72f6f97abefa7f88ca15d5369243"
    );
}

#[test]
fn rejects_invalid_frontmatter_id() {
    // S2: a spec `id` flows into filesystem paths, so the parser must reject a
    // non-slug id (here one containing `..` and a backslash) rather than carry
    // it through to `tests/ludwig_<id>.rs` / the cache path.
    let spec = "---\n\
id: \"../../evil\"\n\
title: Bad id\n\
status: draft\n\
version: 1\n\
---\n\n\
## Intent\n\
A spec whose frontmatter id is not a valid kebab-case slug; the parser must\n\
reject it at validation time before the id can reach any filesystem path.\n\n\
## Behavior\n\
- {#b1} It should never parse.\n\n\
## Examples\n\
```example name=\"x\"\n\
Given a bad id\n\
When parsed\n\
Then it is rejected\n\
```\n\n\
## Invariants\n\
- {deterministic} Parsing fails.\n";
    let err = ludwig::parser::parse(spec).expect_err("invalid id must be rejected");
    assert!(
        err.message.contains("id") && err.message.contains("kebab"),
        "error should name the id rule, got: {}",
        err.message
    );
}
