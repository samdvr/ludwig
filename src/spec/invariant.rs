use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Classifier {
    Deterministic,
    Property,
    Judgment,
}

impl Classifier {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "deterministic" => Some(Self::Deterministic),
            "property" => Some(Self::Property),
            "judgment" => Some(Self::Judgment),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Invariant {
    pub classifier: Classifier,
    pub text: String,
}
