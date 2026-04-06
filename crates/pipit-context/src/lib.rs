pub mod budget;
pub mod budget_tracker;
pub mod cache;
pub mod cache_optimizer;
pub mod compaction;
pub mod content_replacement;
pub mod prompt_ir;
pub mod session;
pub mod knowledge_injection;
pub mod federated_knowledge;
pub mod transcript;

pub use budget::ContextManager;
pub use cache::{CacheBreakpointPlanner, CacheMetrics};
pub use compaction::{CompactionPipeline, CompactionPass, PassResult, PipelineResult, StageMetrics};
pub use content_replacement::{ContentReplacementManager, ReplacementRecord, ToolBudget};
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
