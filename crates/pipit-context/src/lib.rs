pub mod budget;
pub mod budget_tracker;
pub mod cache;
pub mod cache_optimizer;
pub mod compaction;
pub mod content_replacement;
pub mod dedup;
pub mod utility;
pub mod cache_microcompact;
pub mod speculative_compact;
pub mod session_memory_compact;
pub mod prompt_ir;
pub mod session;
pub mod knowledge_injection;
pub mod federated_knowledge;
pub mod transcript;

pub use budget::ContextManager;
pub use cache::{CacheBreakpointPlanner, CacheMetrics};
pub use compaction::{CompactionPipeline, CompactionPass, PassResult, PipelineResult, StageMetrics};
pub use content_replacement::{ContentReplacementManager, ReplacementRecord, ToolBudget};
pub use dedup::{dedup_tool_results, DedupResult};
pub use utility::{estimate_utilities, greedy_knapsack, apply_eviction, MessageUtility};
pub use cache_microcompact::{cache_aware_microcompact, find_stale_tool_results, CacheMicrocompactResult};
pub use speculative_compact::{should_speculate, spawn_speculative_compaction, commit_speculation, SpeculativeCompactResult};
pub use session_memory_compact::{MemoryStore, InMemoryStore, MemoryEntry, summary_to_memory};
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
