pub mod discovery;
pub mod tags;
pub mod graph;
pub mod repomap;
pub mod git_archaeology;
pub mod dependency_health;
pub mod semantic_ir;
pub mod projector;
pub mod dataflow;

pub use discovery::discover_files;
pub use tags::{FileTag, TagKind};
pub use graph::ReferenceGraph;
pub use repomap::RepoMap;
pub use git_archaeology::TemporalKnowledgeGraph;
pub use dependency_health::{DependencyHealthReport, analyze_dependencies};
pub use semantic_ir::SemanticIR;
pub use projector::project;
pub use dataflow::DataFlowGraph;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum IntelligenceError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("Intelligence error: {0}")]
    Other(String),
}

/// Configuration for the intelligence system.
#[derive(Debug, Clone)]
pub struct IntelligenceConfig {
    pub max_file_size: u64,
    pub token_budget: usize,
    pub enable_incremental: bool,
}

impl Default for IntelligenceConfig {
    fn default() -> Self {
        Self {
            max_file_size: 1_048_576,
            token_budget: 4096,
            enable_incremental: true,
        }
    }
}
