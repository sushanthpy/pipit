pub mod builtins;
pub mod file_history;
pub mod lazy_index;
pub mod mcp;
pub mod registry;
pub mod tool_semantics_bridge;
pub mod typed_output;
pub mod typed_tool;
pub mod wrapper;

pub use mcp::{McpClient, McpConfig, McpManager, load_mcp_config};
pub use registry::*;
pub use typed_tool::{
    ArtifactKind, CapabilitySet as TypedCapabilitySet, OutputStream, Purity as TypedPurity,
    RealizedEdit as TypedRealizedEdit, ToolCard, ToolEvent, ToolExample, ToolSearchIndex,
    TypedTool, TypedToolAdapter, TypedToolResult, register_typed,
};

use async_trait::async_trait;
use pipit_config::ApprovalMode;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("Tool not found: {0}")]
    NotFound(String),
    #[error("Invalid arguments: {0}")]
    InvalidArgs(String),
    #[error("Execution failed: {0}")]
    ExecutionFailed(String),
    #[error("Permission denied: {0}")]
    PermissionDenied(String),
    #[error("Timeout after {0}s")]
    Timeout(u64),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// The core tool trait. Every tool implements this.
///
/// Authorization is NOT decided by individual tools. The `is_mutating()` and
/// `requires_approval()` methods exist only as backward-compatible defaults
/// derived from the semantic type system (`tool_semantics_bridge`). The
/// canonical permission oracle is `PolicyKernel::evaluate()` in pipit-core.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique name matching the LLM tool declaration.
    fn name(&self) -> &str;

    /// JSON Schema for arguments.
    fn schema(&self) -> Value;

    /// Human-readable description.
    fn description(&self) -> &str;

    /// Whether this tool modifies the filesystem.
    /// Default: derived from the semantic type system. Tools should NOT override.
    fn is_mutating(&self) -> bool {
        tool_semantics_bridge::builtin_descriptor(self.name()).is_mutating()
    }

    /// Whether this tool requires user approval at the given level.
    /// Default: derived from the semantic type system. Tools should NOT override.
    /// The actual approval decision is made by PolicyKernel in the agent loop.
    fn requires_approval(&self, mode: ApprovalMode) -> bool {
        tool_semantics_bridge::builtin_descriptor(self.name()).requires_approval(mode)
    }

    /// Execute the tool.
    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError>;

    /// Whether this tool can run concurrently with other concurrency-safe tools for the
    /// given arguments. Read-only tools (read_file, grep, glob, list_directory) return true.
    /// Mutating tools (write_file, edit_file, bash) return false.
    ///
    /// The `StreamingToolExecutor` uses this to schedule parallel reads while serializing writes.
    fn is_concurrency_safe(&self, _args: &Value) -> bool {
        !self.is_mutating()
    }
}

/// Context passed to every tool execution.
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// Current working directory — updated by `cd` commands in bash.
    /// Uses interior mutability so bash tool can update it through `&ToolContext`.
    pub cwd: Arc<Mutex<PathBuf>>,
    pub project_root: PathBuf,
    pub approval_mode: ApprovalMode,
    /// Session ID for lineage tracking.
    pub session_id: Option<String>,
    /// Lineage branch ID from the core lineage DAG.
    pub lineage_branch_id: Option<String>,
    /// Contract excerpt to inject into subagent briefings.
    /// This is a rendered slice of the session's ArchitectureIR.
    pub architecture_contract_excerpt: Option<String>,
}

impl ToolContext {
    pub fn new(project_root: PathBuf, approval_mode: ApprovalMode) -> Self {
        // Canonicalize the project root to resolve symlinks (e.g. /tmp → /private/tmp on macOS).
        // This prevents path-containment checks from failing when file paths are canonical
        // but the project root is a symlink.
        let canonical_root = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.clone());
        Self {
            cwd: Arc::new(Mutex::new(canonical_root.clone())),
            project_root: canonical_root,
            approval_mode,
            session_id: None,
            lineage_branch_id: None,
            architecture_contract_excerpt: None,
        }
    }

    /// Get the current working directory.
    ///
    /// Tolerates a poisoned mutex: if a previous tool panicked while holding
    /// the lock, we recover the inner value instead of propagating the panic.
    /// This prevents a single tool failure from cascading to all subsequent
    /// bash/cd calls in the session.
    pub fn current_dir(&self) -> PathBuf {
        self.cwd
            .lock()
            .unwrap_or_else(|poisoned| {
                tracing::warn!("ToolContext cwd mutex was poisoned; recovering");
                poisoned.into_inner()
            })
            .clone()
    }

    /// Update the current working directory (called by bash tool on `cd`).
    pub fn set_cwd(&self, new_cwd: PathBuf) {
        *self.cwd.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("ToolContext cwd mutex was poisoned; recovering");
            poisoned.into_inner()
        }) = new_cwd;
    }
}

/// Result returned from tool execution.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub content: String,
    pub display: Option<ToolDisplay>,
    pub mutated: bool,
    pub content_bytes: usize,
    /// Evidence artifacts from typed tools (empty for legacy tools).
    pub artifacts: Vec<crate::typed_tool::ArtifactKind>,
    /// Realized file edits from typed tools (empty for legacy tools).
    pub edits: Vec<crate::typed_tool::RealizedEdit>,
}

impl ToolResult {
    pub fn text(content: impl Into<String>) -> Self {
        let content = content.into();
        let bytes = content.len();
        Self {
            content,
            display: None,
            mutated: false,
            content_bytes: bytes,
            artifacts: Vec::new(),
            edits: Vec::new(),
        }
    }

    pub fn mutating(content: impl Into<String>) -> Self {
        let content = content.into();
        let bytes = content.len();
        Self {
            content,
            display: None,
            mutated: true,
            content_bytes: bytes,
            artifacts: Vec::new(),
            edits: Vec::new(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        let content = message.into();
        let bytes = content.len();
        Self {
            content,
            display: None,
            mutated: false,
            content_bytes: bytes,
            artifacts: Vec::new(),
            edits: Vec::new(),
        }
    }
}

/// Optional structured display for TUI rendering.
#[derive(Debug, Clone)]
pub enum ToolDisplay {
    Diff {
        path: PathBuf,
        diff: String,
    },
    FileContent {
        path: PathBuf,
        content: String,
        start_line: Option<u32>,
    },
    ShellOutput {
        command: String,
        stdout: String,
        stderr: String,
        exit_code: Option<i32>,
    },
}

/// Convert a ToolResult into a Message for the LLM.
impl ToolResult {
    pub fn into_message(self, call_id: String) -> pipit_provider::Message {
        pipit_provider::Message::tool_result(call_id, self.content, false)
    }
}
