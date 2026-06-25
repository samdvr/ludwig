use sha2::{Digest, Sha256};

use super::behavior::Behavior;
use super::example::Example;
use super::frontmatter::Frontmatter;
use super::invariant::{Classifier, Invariant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub frontmatter: Frontmatter,
    pub intent: String,
    pub behaviors: Vec<Behavior>,
    pub examples: Vec<Example>,
    pub invariants: Vec<Invariant>,
    pub non_goals: String,
    pub open_questions: Vec<String>,
    pub implementation_notes: String,
    pub canonical_body: String,
}

impl Document {
    pub fn id(&self) -> &str { &self.frontmatter.id }
    pub fn version(&self) -> u32 { self.frontmatter.version }
    pub fn stored_hash(&self) -> Option<&str> { self.frontmatter.hash.as_deref() }

    pub fn canonical_hash(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.canonical_body.as_bytes());
        let digest = hasher.finalize();
        hex(&digest)
    }

    pub fn behavior_tags(&self) -> Vec<&str> {
        self.behaviors.iter().filter_map(|b| b.tag.as_deref()).collect()
    }

    pub fn deterministic_invariants(&self) -> impl Iterator<Item = &Invariant> {
        self.invariants.iter().filter(|i| i.classifier == Classifier::Deterministic)
    }

    pub fn property_invariants(&self) -> impl Iterator<Item = &Invariant> {
        self.invariants.iter().filter(|i| i.classifier == Classifier::Property)
    }

    pub fn judgment_invariants(&self) -> impl Iterator<Item = &Invariant> {
        self.invariants.iter().filter(|i| i.classifier == Classifier::Judgment)
    }

    pub fn active_eligible(&self) -> bool {
        self.open_questions.is_empty()
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
