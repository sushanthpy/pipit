pub mod budget;
pub mod session;
pub mod knowledge_injection;

pub use budget::ContextManager;
pub use session::SessionTree;
pub use knowledge_injection::{
    InjectedKnowledge, format_knowledge_preamble, select_knowledge_units,
    extract_knowledge_units,
};

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
