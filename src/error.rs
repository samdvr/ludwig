use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error(transparent)]
    Project(#[from] ProjectError),
    #[error(transparent)]
    Verify(#[from] VerifyError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Error)]
pub struct ParseError {
    pub source_path: Option<PathBuf>,
    pub message: String,
}

impl ParseError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { source_path: None, message: message.into() }
    }

    pub fn at(source: Option<&std::path::Path>, message: impl Into<String>) -> Self {
        Self {
            source_path: source.map(|p| p.to_path_buf()),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.source_path {
            Some(p) => write!(f, "{}: {}", p.display(), self.message),
            None => f.write_str(&self.message),
        }
    }
}

#[derive(Debug, Error)]
#[error("{0}")]
pub struct ProjectError(pub String);

impl ProjectError {
    pub fn new(msg: impl Into<String>) -> Self { Self(msg.into()) }
}

#[derive(Debug, Error)]
#[error("{0}")]
pub struct VerifyError(pub String);

impl VerifyError {
    pub fn new(msg: impl Into<String>) -> Self { Self(msg.into()) }
}
