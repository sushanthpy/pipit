pub mod apply;
pub mod history;
pub mod search_replace;
pub mod udiff;
pub mod whole_file;

pub use history::EditHistory;
pub use search_replace::SearchReplaceFormat;
pub use udiff::UnifiedDiffFormat;
pub use whole_file::WholeFileFormat;

use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EditError {
    #[error("Parse error: {0}")]
    ParseError(String),
    #[error("Search text not found in {path}: {search}")]
    SearchNotFound { path: PathBuf, search: String },
    #[error("File not found: {0}")]
    FileNotFound(PathBuf),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Nothing to undo")]
    NothingToUndo,
    #[error("Edit error: {0}")]
    Other(String),
}

/// An edit format defines how the LLM expresses changes and how we apply them.
pub trait EditFormat: Send + Sync {
    fn name(&self) -> &str;

    /// Instructions for the system prompt.
    fn prompt_instructions(&self) -> &str;

    /// Parse edit operations from LLM response text.
    fn parse(&self, response: &str, known_files: &[PathBuf]) -> Result<Vec<EditOp>, EditError>;

    /// Apply a single edit operation.
    fn apply(&self, op: &EditOp, root: &Path) -> Result<AppliedEdit, EditError>;
}

/// A single edit operation.
#[derive(Debug, Clone)]
pub enum EditOp {
    SearchReplace {
        path: PathBuf,
        search: String,
        replace: String,
    },
    UnifiedDiff {
        path: PathBuf,
        hunks: Vec<DiffHunk>,
    },
    WholeFile {
        path: PathBuf,
        content: String,
    },
    CreateFile {
        path: PathBuf,
        content: String,
    },
    DeleteFile {
        path: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone)]
pub enum DiffLine {
    Context(String),
    Added(String),
    Removed(String),
}

/// Result of applying an edit.
#[derive(Debug, Clone)]
pub struct AppliedEdit {
    pub path: PathBuf,
    pub diff: String,
    pub lines_added: u32,
    pub lines_removed: u32,
    pub before_content: String,
    pub after_content: String,
}

/// Select the best edit format based on model capabilities.
pub fn select_edit_format(
    preferred: Option<pipit_provider::PreferredFormat>,
) -> Box<dyn EditFormat> {
    match preferred {
        Some(pipit_provider::PreferredFormat::SearchReplace) => Box::new(SearchReplaceFormat),
        Some(pipit_provider::PreferredFormat::UnifiedDiff) => Box::new(UnifiedDiffFormat),
        Some(pipit_provider::PreferredFormat::WholeFile) => Box::new(WholeFileFormat),
        None => Box::new(SearchReplaceFormat),
    }
}
