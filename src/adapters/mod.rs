pub mod rust;

use crate::project::Project;
use crate::spec::Document;

#[derive(Debug, Clone)]
pub struct RunResult {
    pub tests: Vec<TestResult>,
    pub pass: u32,
    pub fail: u32,
    pub skip: u32,
    pub exit_code: Option<i32>,
    pub raw: String,
}

#[derive(Debug, Clone)]
pub struct TestResult {
    pub name: String,
    pub status: TestStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestStatus {
    Pass,
    Fail,
    Error,
    Skip,
}

impl TestStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Error => "error",
            Self::Skip => "skip",
        }
    }
}

pub trait Adapter {
    fn render(&self, doc: &Document) -> Result<RenderInfo, crate::Error>;
    fn run(&self, doc: &Document) -> Result<RunResult, crate::Error>;
}

#[derive(Debug, Clone)]
pub struct RenderInfo {
    pub spec_file: std::path::PathBuf,
    pub steps_file: std::path::PathBuf,
}

pub fn for_project(project: &Project) -> rust::RustAdapter {
    // v0.1: always Rust.
    rust::RustAdapter::new(project.clone())
}
