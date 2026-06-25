use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Behavior {
    pub tag: Option<String>,
    pub text: String,
}

impl Behavior {
    pub fn tagged(&self) -> bool {
        self.tag.is_some()
    }
}
