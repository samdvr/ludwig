pub mod adapters;
pub mod catalog;
pub mod drift;
pub mod error;
pub mod game;
pub mod mcp;
pub mod parser;
pub mod plan;
pub mod project;
pub mod prompts;
pub mod scaffold;
pub mod skill;
pub mod spec;
pub mod util;
pub mod verify;

pub use error::{Error, ParseError, ProjectError, VerifyError};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
