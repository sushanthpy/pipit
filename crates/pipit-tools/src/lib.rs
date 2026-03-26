pub mod registry;
pub mod builtins;

pub use registry::*;

use async_trait::async_trait;
use pipit_config::ApprovalMode;
use serde_json::Value;
use std::path::PathBuf;
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
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique name matching the LLM tool declaration.
    fn name(&self) -> &str;

    /// JSON Schema for arguments.
    fn schema(&self) -> Value;

    /// Human-readable description.
    fn description(&self) -> &str;

    /// Whether this tool modifies the filesystem.
    fn is_mutating(&self) -> bool;

    /// Whether this tool requires user approval at the given level.
    fn requires_approval(&self, mode: ApprovalMode) -> bool;

    /// Execute the tool.
    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError>;
}

/// Context passed to every tool execution.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub project_root: PathBuf,
    pub approval_mode: ApprovalMode,
}

impl ToolContext {
    pub fn new(project_root: PathBuf, approval_mode: ApprovalMode) -> Self {
        Self {
            cwd: project_root.clone(),
            project_root,
            approval_mode,
        }
    }
}

/// Result returned from tool execution.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub content: String,
    pub display: Option<ToolDisplay>,
    pub mutated: bool,
    pub content_bytes: usize,
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
