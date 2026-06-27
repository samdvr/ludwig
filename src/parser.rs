use std::path::Path;

use indexmap::IndexMap;
use regex::Regex;
use std::sync::LazyLock;

use crate::error::ParseError;
use crate::spec::{
    Behavior, Classifier, Document, Example, Frontmatter, GherkinKeyword, GherkinStep, Invariant,
    REQUIRED_SECTIONS, is_known_section, section_order,
};

const INTENT_MIN_WORDS: usize = 20;
const INTENT_MAX_WORDS: usize = 250;

static BULLET_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[-*+]\s+").unwrap());
static BEHAVIOR_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\{#([A-Za-z][\w-]*)\}\s+").unwrap());
static INVARIANT_CLASSIFIER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\{(deterministic|property|judgment)\}\s+").unwrap());
static EXAMPLE_FENCE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^```example(?:\s+name="([^"]+)")?\s*$"#).unwrap());
static GHERKIN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(Given|When|Then|And)\s+(.+)$").unwrap());

pub fn parse(input: &str) -> Result<Document, ParseError> {
    parse_with_source(input, None)
}

pub fn parse_file(path: &Path) -> Result<Document, ParseError> {
    let content = std::fs::read(path)
        .map_err(|e| ParseError::at(Some(path), format!("read failed: {e}")))?;
    let text = String::from_utf8(content)
        .map_err(|e| ParseError::at(Some(path), format!("invalid UTF-8: {e}")))?;
    parse_with_source(&text, Some(path))
}

pub fn parse_with_source(input: &str, source: Option<&Path>) -> Result<Document, ParseError> {
    let normalized = normalize_line_endings(input);
    let (front_yaml, body) = split_frontmatter(&normalized, source)?;
    let frontmatter = Frontmatter::from_yaml(&front_yaml, source)?;
    let sections = tokenize_sections(&body, source)?;
    validate_section_order(&sections, source)?;

    // `validate_section_order` has already guaranteed every required section is
    // present; `require_section` turns the "can't happen" absence into a regular
    // parse error rather than a panic on user input.
    let intent_text = require_section(&sections, "Intent", source)?.to_owned();
    let intent = parse_intent(&intent_text, source)?;
    let behaviors = parse_behaviors(require_section(&sections, "Behavior", source)?, source)?;
    let examples = parse_examples(require_section(&sections, "Examples", source)?, source)?;
    let invariants = parse_invariants(require_section(&sections, "Invariants", source)?, source)?;

    let non_goals = sections.get("Non-goals").map(|s| s.trim().to_string()).unwrap_or_default();
    let open_questions_text = sections
        .get("Open questions")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let open_questions = parse_open_questions(&open_questions_text, source)?;
    let implementation_notes = sections
        .get("Implementation notes")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    validate_unique_behavior_tags(&behaviors, source)?;
    enforce_active_status_rules(&frontmatter, &open_questions, source)?;

    let canonical_body = build_canonical_body(
        &frontmatter,
        &intent,
        &behaviors,
        &examples,
        &invariants,
        &non_goals,
        &open_questions,
        &implementation_notes,
    );

    Ok(Document {
        frontmatter,
        intent,
        behaviors,
        examples,
        invariants,
        non_goals,
        open_questions,
        implementation_notes,
        canonical_body,
    })
}

fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn split_frontmatter(
    text: &str,
    source: Option<&Path>,
) -> Result<(String, String), ParseError> {
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.first().map(|l| l.trim()) != Some("---") {
        return Err(ParseError::at(
            source,
            "spec must begin with YAML frontmatter delimited by `---`",
        ));
    }

    let mut end_idx = None;
    for (i, line) in lines.iter().enumerate().skip(1) {
        if line.trim() == "---" {
            end_idx = Some(i);
            break;
        }
    }
    let end = end_idx.ok_or_else(|| {
        ParseError::at(source, "frontmatter is not terminated by a closing `---`")
    })?;

    let front = lines[1..end].join("\n");
    let body = if end < lines.len() {
        lines[end + 1..].join("\n")
    } else {
        String::new()
    };
    Ok((front, body))
}

/// Fetch a section that an earlier `validate_section_order` has already proven
/// present. Returns an internal parse error instead of panicking if that
/// invariant is ever violated, so no code path can `expect`-panic on user input.
fn require_section<'a>(
    sections: &'a IndexMap<String, String>,
    name: &str,
    source: Option<&Path>,
) -> Result<&'a String, ParseError> {
    sections.get(name).ok_or_else(|| {
        ParseError::at(source, format!("internal: required section {name:?} missing after validation"))
    })
}

/// Tokenize the body into named sections, tracking code-fence depth so that an H2
/// inside a fenced example block does not split a section.
fn tokenize_sections(
    body: &str,
    source: Option<&Path>,
) -> Result<IndexMap<String, String>, ParseError> {
    let mut sections: IndexMap<String, String> = IndexMap::new();
    let mut current_name: Option<String> = None;
    let mut current_lines: Vec<String> = Vec::new();
    let mut in_fence = false;

    for line in body.split('\n') {
        if line.starts_with("```") {
            in_fence = !in_fence;
            if current_name.is_some() {
                current_lines.push(line.to_string());
            }
            continue;
        }

        if !in_fence && line.starts_with("## ") {
            if let Some(name) = current_name.take() {
                sections.insert(name, current_lines.join("\n"));
                current_lines = Vec::new();
            }
            let name = line.trim_start_matches("## ").trim().to_string();
            if sections.contains_key(&name) {
                return Err(ParseError::at(
                    source,
                    format!("section ## {name} appears more than once"),
                ));
            }
            current_name = Some(name);
        } else if !in_fence && line.starts_with('#') {
            if line.starts_with("# ") {
                return Err(ParseError::at(
                    source,
                    "H1 headings are not allowed inside a spec body; use Intent prose",
                ));
            }
            // A `#`-run with no following space — `##foo`, `#foo`, `###bar` — is a
            // malformed ATX heading (most often a section header typed without the
            // required space). Reject it loudly rather than silently swallowing it
            // as section prose. A `#`-run followed by a space (e.g. `### Notes`) is
            // a legitimate sub-heading inside a section and is kept as content.
            let hashes = line.chars().take_while(|c| *c == '#').count();
            let after = &line[hashes..];
            if !after.is_empty() && !after.starts_with(char::is_whitespace) {
                return Err(ParseError::at(
                    source,
                    format!(
                        "malformed heading {line:?}: ATX headings need a space after the `#` markers (did you mean `## {after}`?)"
                    ),
                ));
            }
            if current_name.is_some() {
                current_lines.push(line.to_string());
            }
        } else if current_name.is_some() {
            current_lines.push(line.to_string());
        }
    }

    if in_fence {
        return Err(ParseError::at(source, "unterminated code fence in spec body"));
    }

    if let Some(name) = current_name {
        sections.insert(name, current_lines.join("\n"));
    }

    if sections.is_empty() {
        return Err(ParseError::at(source, "spec body contains no H2 sections"));
    }

    Ok(sections)
}

fn validate_section_order(
    sections: &IndexMap<String, String>,
    source: Option<&Path>,
) -> Result<(), ParseError> {
    let names: Vec<&str> = sections.keys().map(|s| s.as_str()).collect();

    let missing: Vec<&&str> =
        REQUIRED_SECTIONS.iter().filter(|s| !names.contains(*s)).collect();
    if !missing.is_empty() {
        let list: Vec<String> = missing.iter().map(|s| s.to_string()).collect();
        return Err(ParseError::at(
            source,
            format!("missing required section(s): {}", list.join(", ")),
        ));
    }

    let unknown: Vec<&&str> = names.iter().filter(|n| !is_known_section(n)).collect();
    if !unknown.is_empty() {
        let list: Vec<String> = unknown.iter().map(|s| s.to_string()).collect();
        return Err(ParseError::at(
            source,
            format!("unknown section(s): {}", list.join(", ")),
        ));
    }

    let expected: Vec<&str> = section_order().filter(|s| names.contains(s)).collect();
    if names != expected {
        return Err(ParseError::at(
            source,
            format!(
                "sections out of order. Got {}; expected {}",
                names.join(" → "),
                expected.join(" → ")
            ),
        ));
    }

    Ok(())
}

fn parse_intent(text: &str, source: Option<&Path>) -> Result<String, ParseError> {
    let prose = text.trim().to_string();
    let word_count = count_words(&prose);
    if word_count < INTENT_MIN_WORDS {
        return Err(ParseError::at(
            source,
            format!(
                "Intent must be at least {INTENT_MIN_WORDS} words (got {word_count}); stubs are not specifications"
            ),
        ));
    }
    if word_count > INTENT_MAX_WORDS {
        return Err(ParseError::at(
            source,
            format!(
                "Intent must be at most {INTENT_MAX_WORDS} words (got {word_count}); move detail into Behavior or Implementation notes"
            ),
        ));
    }
    Ok(prose)
}

/// Count "words" in a way that works for both space-separated scripts and
/// scripts that don't use spaces (CJK). Each whitespace-delimited token counts
/// as one word, except that every CJK character within it counts individually —
/// so a 30-character Chinese Intent isn't seen as a single word and rejected as
/// a stub. A token with no CJK characters counts as exactly one word.
fn count_words(s: &str) -> usize {
    let mut count = 0;
    for token in s.split_whitespace() {
        let cjk = token.chars().filter(|c| is_cjk(*c)).count();
        // CJK chars each count as a word; any remaining non-CJK content in the
        // token counts as one more word.
        count += cjk + usize::from(token.chars().any(|c| !is_cjk(c)));
    }
    count
}

/// Whether `c` belongs to a CJK / Japanese / Korean script block that is written
/// without spaces between words.
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF      // Hiragana + Katakana
        | 0x3400..=0x4DBF    // CJK Unified Ext A
        | 0x4E00..=0x9FFF    // CJK Unified
        | 0xAC00..=0xD7AF    // Hangul syllables
        | 0xF900..=0xFAFF    // CJK Compatibility Ideographs
        | 0x20000..=0x2EBEF  // CJK Unified Ext B–F
    )
}

fn parse_behaviors(text: &str, source: Option<&Path>) -> Result<Vec<Behavior>, ParseError> {
    let bullets = extract_bullets(text, "Behavior", source)?;
    let mut out = Vec::with_capacity(bullets.len());
    for line in bullets {
        if let Some(caps) = BEHAVIOR_TAG_RE.captures(&line) {
            let tag = caps.get(1).unwrap().as_str().to_string();
            let rest = line[caps.get(0).unwrap().end()..].trim().to_string();
            out.push(Behavior { tag: Some(tag), text: rest });
        } else {
            out.push(Behavior { tag: None, text: line.trim().to_string() });
        }
    }
    Ok(out)
}

fn parse_examples(text: &str, source: Option<&Path>) -> Result<Vec<Example>, ParseError> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut examples: Vec<Example> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(caps) = EXAMPLE_FENCE_RE.captures(line) {
            let name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            if name.is_empty() {
                return Err(ParseError::at(
                    source,
                    "Example fence must declare name=\"...\"",
                ));
            }

            let mut step_lines: Vec<&str> = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].starts_with("```") {
                step_lines.push(lines[i]);
                i += 1;
            }
            if i >= lines.len() {
                return Err(ParseError::at(
                    source,
                    format!("Example block `{name}` is not closed by ```"),
                ));
            }

            let steps = parse_gherkin_steps(&step_lines, name, source)?;
            examples.push(Example { name: name.to_string(), steps });
        } else if line.starts_with("```") {
            return Err(ParseError::at(
                source,
                "Examples section must only contain ```example``` fences; found a bare code fence",
            ));
        }
        i += 1;
    }

    if examples.is_empty() {
        return Err(ParseError::at(
            source,
            "Examples section must contain at least one ```example name=\"...\"``` block (meaning is use — behavior without examples is vague intent)",
        ));
    }

    let mut names: Vec<&str> = examples.iter().map(|e| e.name.as_str()).collect();
    names.sort();
    for w in names.windows(2) {
        if w[0] == w[1] {
            return Err(ParseError::at(
                source,
                format!("duplicate example name: {:?}", w[0]),
            ));
        }
    }

    // Two distinct names can still slugify to the same Rust test-fn identifier
    // (e.g. "burst then throttle" and "burst-then-throttle" both become
    // `test_example_burst_then_throttle`). Reject these collisions here so the
    // generated test file is guaranteed to compile.
    let mut by_slug: std::collections::BTreeMap<String, &str> =
        std::collections::BTreeMap::new();
    for e in &examples {
        let slug = example_test_slug(&e.name);
        if let Some(prev) = by_slug.insert(slug.clone(), e.name.as_str()) {
            return Err(ParseError::at(
                source,
                format!(
                    "example names {:?} and {:?} both slugify to {slug:?}; rename one so the generated tests don't collide",
                    prev, e.name
                ),
            ));
        }
    }

    Ok(examples)
}

/// Mirror of `adapters::rust::example_test_name` used only for collision checks.
/// Kept in the parser so spec validation does not depend on adapter internals.
fn example_test_slug(name: &str) -> String {
    let mut out = String::from("test_example_");
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    let collapsed: String = out
        .split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    if collapsed.starts_with("test_example_") {
        collapsed
    } else {
        format!("test_example_{collapsed}")
    }
}

fn parse_gherkin_steps(
    lines: &[&str],
    example_name: &str,
    source: Option<&Path>,
) -> Result<Vec<GherkinStep>, ParseError> {
    let mut steps: Vec<GherkinStep> = Vec::new();
    for raw in lines {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }

        let caps = GHERKIN_RE.captures(line).ok_or_else(|| {
            ParseError::at(
                source,
                format!(
                    "in example `{example_name}`: every step must start with Given/When/Then/And (got {line:?})"
                ),
            )
        })?;
        let keyword =
            GherkinKeyword::parse(caps.get(1).unwrap().as_str()).expect("regex matched");
        let text = caps.get(2).unwrap().as_str().trim().to_string();

        if steps.is_empty() && keyword != GherkinKeyword::Given {
            return Err(ParseError::at(
                source,
                format!("in example `{example_name}`: first step must be `Given`"),
            ));
        }
        if steps.is_empty() && keyword == GherkinKeyword::And {
            return Err(ParseError::at(
                source,
                format!("in example `{example_name}`: `And` cannot be the first step"),
            ));
        }

        steps.push(GherkinStep { keyword, text });
    }

    if steps.is_empty() {
        return Err(ParseError::at(
            source,
            format!("example `{example_name}` contains no Given/When/Then steps"),
        ));
    }
    let has_when = steps.iter().any(|s| s.keyword == GherkinKeyword::When);
    let has_then = steps.iter().any(|s| s.keyword == GherkinKeyword::Then);
    if !has_when {
        return Err(ParseError::at(
            source,
            format!("example `{example_name}` is missing a `When` step"),
        ));
    }
    if !has_then {
        return Err(ParseError::at(
            source,
            format!("example `{example_name}` is missing a `Then` step"),
        ));
    }

    Ok(steps)
}

fn parse_invariants(text: &str, source: Option<&Path>) -> Result<Vec<Invariant>, ParseError> {
    let bullets = extract_bullets(text, "Invariants", source)?;
    let mut out = Vec::with_capacity(bullets.len());
    for line in bullets {
        let caps = INVARIANT_CLASSIFIER_RE.captures(&line).ok_or_else(|| {
            ParseError::at(
                source,
                format!(
                    "invariant bullets must begin with a classifier {{deterministic|property|judgment}}: {line:?}"
                ),
            )
        })?;
        let classifier = Classifier::parse(caps.get(1).unwrap().as_str()).expect("regex matched");
        let rest = line[caps.get(0).unwrap().end()..].to_string();
        if INVARIANT_CLASSIFIER_RE.is_match(&rest) {
            return Err(ParseError::at(
                source,
                format!(
                    "invariant bullet declares more than one classifier; split into separate bullets: {line:?}"
                ),
            ));
        }
        out.push(Invariant { classifier, text: rest.trim().to_string() });
    }
    Ok(out)
}

fn parse_open_questions(text: &str, source: Option<&Path>) -> Result<Vec<String>, ParseError> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    // Open questions follows the same shape as Behavior / Invariants / etc.:
    // a bulleted list. Allowing prose here used to be an exception, but the
    // split policy made parser behavior hard to predict — every section now
    // requires bullets.
    extract_bullets(text, "Open questions", source).map(|bullets| {
        bullets
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

fn extract_bullets(
    text: &str,
    section: &str,
    source: Option<&Path>,
) -> Result<Vec<String>, ParseError> {
    let mut bullets: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    for raw in text.split('\n') {
        let line = raw.trim_end();
        if BULLET_RE.is_match(line) {
            if let Some(c) = current.take() {
                bullets.push(c.trim().to_string());
            }
            current = Some(BULLET_RE.replace(line, "").to_string());
        } else if let Some(c) = current.as_mut() {
            if line.starts_with("  ") && !line.trim().is_empty() {
                c.push(' ');
                c.push_str(line.trim());
            } else if line.trim().is_empty() {
                bullets.push(c.trim().to_string());
                current = None;
            } else {
                // Non-bullet, non-continuation prose mid-section.
                return Err(ParseError::at(
                    source,
                    format!("{section} section must be a bulleted list; found prose: {line:?}"),
                ));
            }
        } else if !line.trim().is_empty() {
            return Err(ParseError::at(
                source,
                format!("{section} section must be a bulleted list; found prose: {line:?}"),
            ));
        }
    }
    if let Some(c) = current {
        bullets.push(c.trim().to_string());
    }
    if bullets.is_empty() {
        return Err(ParseError::at(source, format!("{section} section is empty")));
    }
    Ok(bullets)
}

fn validate_unique_behavior_tags(
    behaviors: &[Behavior],
    source: Option<&Path>,
) -> Result<(), ParseError> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for b in behaviors {
        if let Some(tag) = &b.tag
            && !seen.insert(tag.as_str())
        {
            return Err(ParseError::at(
                source,
                format!("duplicate behavior tag: {{#{tag}}}"),
            ));
        }
    }
    Ok(())
}

fn enforce_active_status_rules(
    fm: &Frontmatter,
    open_questions: &[String],
    source: Option<&Path>,
) -> Result<(), ParseError> {
    if !fm.is_active() || open_questions.is_empty() {
        return Ok(());
    }
    Err(ParseError::at(
        source,
        "spec has status: active but `Open questions` is non-empty. Resolve them or move to draft.",
    ))
}

/// Canonical body for hashing, built from the *parsed* document rather than the
/// raw markdown. Hashing the parsed model (not the source text) means the hash
/// tracks meaning: reflowing a multi-line bullet onto one line, collapsing blank
/// lines between bullets, or any other cosmetic edit the parser already
/// normalizes away no longer registers as drift — and, because the body is a
/// pure function of every semantic field, two documents that compare equal
/// always hash equal. Prose sections are right-trimmed per line so trailing
/// whitespace / CRLF churn is ignored (matching the parser's own normalization);
/// optional sections are emitted only when non-empty. Section order is fixed.
#[allow(clippy::too_many_arguments)]
fn build_canonical_body(
    fm: &Frontmatter,
    intent: &str,
    behaviors: &[Behavior],
    examples: &[Example],
    invariants: &[Invariant],
    non_goals: &str,
    open_questions: &[String],
    implementation_notes: &str,
) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(fm.to_canonical_yaml().trim_end_matches('\n'));
    out.push_str("\n---\n");

    out.push_str("\n## Intent\n");
    out.push_str(&normalize_prose(intent));
    out.push('\n');

    out.push_str("\n## Behavior\n");
    for b in behaviors {
        match &b.tag {
            Some(tag) => out.push_str(&format!("- {{#{tag}}} {}\n", b.text)),
            None => out.push_str(&format!("- {}\n", b.text)),
        }
    }

    out.push_str("\n## Examples\n");
    for ex in examples {
        out.push_str(&format!("### {}\n", ex.name));
        for s in &ex.steps {
            out.push_str(&format!("- {} {}\n", gherkin_label(s.keyword), s.text));
        }
    }

    out.push_str("\n## Invariants\n");
    for inv in invariants {
        out.push_str(&format!(
            "- {{{}}} {}\n",
            classifier_label(inv.classifier),
            inv.text
        ));
    }

    let non_goals = normalize_prose(non_goals);
    if !non_goals.is_empty() {
        out.push_str("\n## Non-goals\n");
        out.push_str(&non_goals);
        out.push('\n');
    }
    if !open_questions.is_empty() {
        out.push_str("\n## Open questions\n");
        for q in open_questions {
            out.push_str(&format!("- {}\n", q.trim()));
        }
    }
    let implementation_notes = normalize_prose(implementation_notes);
    if !implementation_notes.is_empty() {
        out.push_str("\n## Implementation notes\n");
        out.push_str(&implementation_notes);
        out.push('\n');
    }
    out
}

/// Right-trim each line and trim the ends so trailing-whitespace / CRLF churn in
/// free-prose sections never changes the hash.
fn normalize_prose(s: &str) -> String {
    s.split('\n')
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn gherkin_label(k: GherkinKeyword) -> &'static str {
    match k {
        GherkinKeyword::Given => "Given",
        GherkinKeyword::When => "When",
        GherkinKeyword::Then => "Then",
        GherkinKeyword::And => "And",
    }
}

fn classifier_label(c: Classifier) -> &'static str {
    match c {
        Classifier::Deterministic => "deterministic",
        Classifier::Property => "property",
        Classifier::Judgment => "judgment",
    }
}
