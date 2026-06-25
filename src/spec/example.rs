use serde::Serialize;

use super::gherkin_step::{GherkinKeyword, GherkinStep};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Example {
    pub name: String,
    pub steps: Vec<GherkinStep>,
}

impl Example {
    pub fn given_steps(&self) -> impl Iterator<Item = &GherkinStep> {
        self.steps.iter().filter(|s| s.keyword == GherkinKeyword::Given)
    }

    pub fn when_steps(&self) -> impl Iterator<Item = &GherkinStep> {
        self.steps.iter().filter(|s| s.keyword == GherkinKeyword::When)
    }

    pub fn then_steps(&self) -> impl Iterator<Item = &GherkinStep> {
        self.steps
            .iter()
            .filter(|s| matches!(s.keyword, GherkinKeyword::Then | GherkinKeyword::And))
    }
}
