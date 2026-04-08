pub mod budget;
pub mod budget_tracker;
pub mod cache;
pub mod cache_microcompact;
pub mod cache_optimizer;
pub mod compaction;
pub mod content_replacement;
pub mod dedup;
pub mod federated_knowledge;
pub mod knowledge_injection;
pub mod prompt_ir;
pub mod session;
pub mod session_memory_compact;
pub mod speculative_compact;
pub mod transcript;
pub mod utility;

pub use budget::ContextManager;
pub use cache::{CacheBreakpointPlanner, CacheMetrics};
pub use cache_microcompact::{
    CacheMicrocompactResult, cache_aware_microcompact, find_stale_tool_results,
};
pub use compaction::{
    CompactionPass, CompactionPipeline, PassResult, PipelineResult, StageMetrics,
};
pub use content_replacement::{ContentReplacementManager, ReplacementRecord, ToolBudget};
pub use dedup::{DedupResult, dedup_tool_results};
pub use knowledge_injection::{
    InjectedKnowledge, extract_knowledge_units, format_knowledge_preamble, select_knowledge_units,
};
pub use session::SessionTree;
pub use session_memory_compact::{InMemoryStore, MemoryEntry, MemoryStore, summary_to_memory};
pub use speculative_compact::{
    SpeculativeCompactResult, commit_speculation, should_speculate, spawn_speculative_compaction,
};
pub use utility::{MessageUtility, apply_eviction, estimate_utilities, greedy_knapsack};

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
