//! Property-based / fuzzy tests for the spec parser.
//!
//! These complement the example-based tests in `tests/parser.rs`. They fall
//! into two families:
//!
//!   * Robustness — feed arbitrary and structurally-suggestive byte sequences to
//!     `parse` and assert it always returns `Ok`/`Err` and never panics, slices a
//!     char boundary wrong, or integer-overflows.
//!   * Round-trip — generate *valid* specs from constrained strategies and assert
//!     they parse, that their structure is recovered faithfully, and that the
//!     canonical hash is whitespace-insensitive yet content-sensitive.

use std::path::PathBuf;

use proptest::prelude::*;

fn fixture(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/specs/valid");
    p.push(name);
    p
}

// -- robustness: arbitrary input must never panic -----------------------------

/// A character alphabet weighted toward the bytes that actually drive parser
/// control flow (`#`, fence backticks, braces, quotes, colons, newlines), plus a
/// long tail of arbitrary Unicode so multi-byte slicing is exercised too.
fn fuzzy_string() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            6 => proptest::char::range('a', 'z'),
            2 => Just('\n'),
            1 => Just(' '),
            1 => Just('\t'),
            1 => Just('#'),
            1 => Just('-'),
            1 => Just('`'),
            1 => Just('{'),
            1 => Just('}'),
            1 => Just('"'),
            1 => Just(':'),
            1 => Just('\r'),
            1 => proptest::char::range('\u{0}', '\u{10ffff}'),
        ],
        0..400,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

/// Generate inputs that *look* like specs — a frontmatter fence followed by a
/// jumble of section headers, bullets, classifiers, example fences and Gherkin
/// steps — so the fuzzer reaches deep into section tokenizing and step parsing
/// rather than bouncing off the first frontmatter check.
fn structurish_spec() -> impl Strategy<Value = String> {
    let front_line = "[a-z_]{1,12}: [a-z0-9 \\[\\]]{0,16}";
    let body_line = prop_oneof![
        Just("---".to_string()),
        "## [A-Za-z ]{1,20}".prop_map(|s| s),
        "- \\{#[A-Za-z][a-z0-9-]{0,6}\\} [a-z ]{0,20}".prop_map(|s| s),
        "- \\{(deterministic|property|judgment)\\} [a-z ]{0,20}".prop_map(|s| s),
        "- [a-z ]{0,20}".prop_map(|s| s),
        "```example name=\"[a-z 0-9-]{0,15}\"".prop_map(|s| s),
        "(Given|When|Then|And) [a-z ]{0,20}".prop_map(|s| s),
        Just("```".to_string()),
        "[a-z ]{0,30}".prop_map(|s| s),
    ];
    (
        prop::collection::vec(front_line, 0..6),
        prop::collection::vec(body_line, 0..30),
    )
        .prop_map(|(front, body)| {
            let mut s = String::from("---\n");
            for l in &front {
                s.push_str(l);
                s.push('\n');
            }
            s.push_str("---\n");
            for l in &body {
                s.push_str(l);
                s.push('\n');
            }
            s
        })
}

proptest! {
    /// The parser must be total over arbitrary text: any string maps to
    /// `Ok` or `Err`, never a panic.
    #[test]
    fn parse_is_total_over_arbitrary_text(s in fuzzy_string()) {
        let _ = ludwig::parser::parse(&s);
    }

    /// Same, for spec-shaped noise that exercises the section/Gherkin paths.
    #[test]
    fn parse_is_total_over_spec_shaped_noise(s in structurish_spec()) {
        let _ = ludwig::parser::parse(&s);
    }

    /// Raw arbitrary bytes, parsed when they happen to be valid UTF-8. Guards the
    /// `parse_file` UTF-8 boundary indirectly and any byte-index slicing.
    #[test]
    fn parse_is_total_over_arbitrary_utf8(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        if let Ok(s) = std::str::from_utf8(&bytes) {
            let _ = ludwig::parser::parse(s);
        }
    }
}

/// Truncating a known-valid spec at every char boundary must never panic — this
/// catches off-by-one assumptions in the line/fence/section state machines when
/// the document ends mid-construct.
#[test]
fn parsing_every_prefix_of_a_valid_spec_never_panics() {
    for name in ["minimal.spec.md", "token_bucket.spec.md"] {
        let raw = std::fs::read_to_string(fixture(name)).expect("fixture readable");
        for (i, _) in raw.char_indices() {
            let _ = ludwig::parser::parse(&raw[..i]);
        }
        let _ = ludwig::parser::parse(&raw);
    }
}

// -- round-trip: generated valid specs --------------------------------------

#[derive(Debug, Clone)]
struct ValidSpec {
    text: String,
    id: String,
    n_behaviors: usize,
    n_examples: usize,
    n_invariants: usize,
}

prop_compose! {
    /// Generate a structurally valid spec by construction: each piece is drawn
    /// from a strategy that respects the parser's rules (word counts, unique
    /// behavior tags, indexed example names that can't slug-collide, one
    /// classifier per invariant), so a parse failure here is a real parser bug.
    fn arb_valid_spec()(
        id_suffix in "[a-z0-9-]{0,20}",
        title in "[a-zA-Z0-9 ]{1,30}",
        status in prop::sample::select(vec!["draft", "active", "deprecated"]),
        intent_words in prop::collection::vec("[a-z]{1,8}", 20..=60),
        behaviors in prop::collection::vec(
            (any::<bool>(), prop::collection::vec("[a-z]{1,8}", 1..=8)),
            1..=5,
        ),
        examples in prop::collection::vec(
            (
                prop::collection::vec("[a-z]{1,6}", 1..=4),
                prop::collection::vec("[a-z]{1,6}", 1..=4),
                prop::collection::vec("[a-z]{1,6}", 1..=4),
                any::<bool>(),
            ),
            1..=3,
        ),
        invariants in prop::collection::vec(
            (
                prop::sample::select(vec!["deterministic", "property", "judgment"]),
                prop::collection::vec("[a-z]{1,8}", 1..=8),
            ),
            1..=3,
        ),
    ) -> ValidSpec {
        // Prefix with a letter so the id can never be a YAML keyword (no/yes/null);
        // quoting in the emitted YAML keeps it a string regardless.
        let id = format!("s{id_suffix}");

        let mut s = String::new();
        s.push_str("---\n");
        s.push_str(&format!("id: \"{id}\"\n"));
        s.push_str(&format!("title: \"{title}\"\n"));
        s.push_str(&format!("status: {status}\n"));
        s.push_str("version: 1\n");
        s.push_str("---\n\n");

        s.push_str("## Intent\n");
        s.push_str(&intent_words.join(" "));
        s.push_str("\n\n");

        s.push_str("## Behavior\n");
        let mut tag_i = 0usize;
        for (tagged, words) in &behaviors {
            if *tagged {
                s.push_str(&format!("- {{#t{tag_i}}} {}\n", words.join(" ")));
                tag_i += 1;
            } else {
                s.push_str(&format!("- {}\n", words.join(" ")));
            }
        }
        s.push('\n');

        s.push_str("## Examples\n");
        for (i, (given, when, then, extra_and)) in examples.iter().enumerate() {
            // The index in the name guarantees both name- and slug-uniqueness.
            s.push_str(&format!("```example name=\"ex {i} sample\"\n"));
            s.push_str(&format!("Given {}\n", given.join(" ")));
            s.push_str(&format!("When {}\n", when.join(" ")));
            s.push_str(&format!("Then {}\n", then.join(" ")));
            if *extra_and {
                s.push_str(&format!("And {}\n", then.join(" ")));
            }
            s.push_str("```\n\n");
        }

        s.push_str("## Invariants\n");
        for (classifier, words) in &invariants {
            s.push_str(&format!("- {{{classifier}}} {}\n", words.join(" ")));
        }

        ValidSpec {
            text: s,
            id,
            n_behaviors: behaviors.len(),
            n_examples: examples.len(),
            n_invariants: invariants.len(),
        }
    }
}

proptest! {
    /// Every generated valid spec parses, and its structure round-trips.
    #[test]
    fn generated_valid_specs_parse_and_round_trip(spec in arb_valid_spec()) {
        let parsed = ludwig::parser::parse(&spec.text);
        prop_assert!(
            parsed.is_ok(),
            "valid spec failed to parse: {}\n--- spec ---\n{}",
            parsed.as_ref().err().map(|e| e.message.clone()).unwrap_or_default(),
            spec.text,
        );
        let doc = parsed.unwrap();
        prop_assert_eq!(doc.id(), spec.id.as_str());
        prop_assert_eq!(doc.version(), 1);
        prop_assert_eq!(doc.behaviors.len(), spec.n_behaviors);
        prop_assert_eq!(doc.examples.len(), spec.n_examples);
        prop_assert_eq!(doc.invariants.len(), spec.n_invariants);
    }

    /// The canonical hash ignores trailing whitespace and CRLF: perturbing line
    /// endings of a valid spec must not change it.
    #[test]
    fn canonical_hash_is_whitespace_insensitive(spec in arb_valid_spec()) {
        let a = ludwig::parser::parse(&spec.text).unwrap().canonical_hash();
        let perturbed = spec.text.replace('\n', "  \r\n");
        let b = ludwig::parser::parse(&perturbed).unwrap().canonical_hash();
        prop_assert_eq!(a, b);
    }

    /// ...but it is content-sensitive: appending a new section changes it.
    #[test]
    fn canonical_hash_changes_when_a_section_is_added(spec in arb_valid_spec()) {
        let a = ludwig::parser::parse(&spec.text).unwrap().canonical_hash();
        // Implementation notes is the last section in canonical order, so
        // appending it keeps the document well-ordered while changing content.
        let altered = format!("{}\n## Implementation notes\nan extra note here\n", spec.text);
        let b = ludwig::parser::parse(&altered).unwrap().canonical_hash();
        prop_assert_ne!(a, b);
    }

    /// Splicing an arbitrary char into a valid spec at an arbitrary char boundary
    /// must never panic — only ever parse or error.
    #[test]
    fn mutating_a_valid_spec_never_panics(
        spec in arb_valid_spec(),
        c in proptest::char::range('\u{0}', '\u{10ffff}'),
        frac in 0.0f64..1.0,
    ) {
        let boundaries: Vec<usize> = spec.text.char_indices().map(|(i, _)| i).collect();
        let at = boundaries[(frac * boundaries.len() as f64) as usize % boundaries.len().max(1)];
        let mut mutated = spec.text.clone();
        mutated.insert(at, c);
        let _ = ludwig::parser::parse(&mutated);
    }
}
