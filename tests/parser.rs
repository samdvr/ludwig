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
