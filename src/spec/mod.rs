pub mod behavior;
pub mod document;
pub mod example;
pub mod frontmatter;
pub mod gherkin_step;
pub mod invariant;

pub use behavior::Behavior;
pub use document::Document;
pub use example::Example;
pub use frontmatter::{Frontmatter, Status};
pub use gherkin_step::{GherkinKeyword, GherkinStep};
pub use invariant::{Classifier, Invariant};

/// Required sections, in the order they must appear.
pub const REQUIRED_SECTIONS: &[&str] = &["Intent", "Behavior", "Examples", "Invariants"];

/// Optional sections, in the order they must appear if present (all after the required ones).
pub const OPTIONAL_SECTIONS: &[&str] = &["Non-goals", "Open questions", "Implementation notes"];

/// Canonical section order = required ++ optional.
pub fn section_order() -> impl Iterator<Item = &'static str> {
    REQUIRED_SECTIONS.iter().chain(OPTIONAL_SECTIONS.iter()).copied()
}

pub fn is_known_section(name: &str) -> bool {
    REQUIRED_SECTIONS.contains(&name) || OPTIONAL_SECTIONS.contains(&name)
}
