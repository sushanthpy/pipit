pub mod budget;
pub mod session;

pub use budget::ContextManager;
pub use session::SessionTree;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ContextError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Branch not found: {0}")]
    BranchNotFound(String),
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("Context error: {0}")]
    Other(String),
}
