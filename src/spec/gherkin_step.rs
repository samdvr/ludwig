use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GherkinKeyword {
    Given,
    When,
    Then,
    And,
}

impl GherkinKeyword {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "Given" => Some(Self::Given),
            "When" => Some(Self::When),
            "Then" => Some(Self::Then),
            "And" => Some(Self::And),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GherkinStep {
    pub keyword: GherkinKeyword,
    pub text: String,
}
