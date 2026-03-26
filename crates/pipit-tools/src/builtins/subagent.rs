use crate::{Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use pipit_config::ApprovalMode;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Callback for subagent execution, provided by the application layer
/// to break the pipit-tools → pipit-core circular dependency.
#[async_trait]
pub trait SubagentExecutor: Send + Sync {
    async fn run_subagent(
        &self,
        task: String,
        context: String,
        allowed_tools: Vec<String>,
        project_root: std::path::PathBuf,
        cancel: CancellationToken,
    ) -> Result<String, String>;
}

/// Subagent tool — delegates focused subtasks to a child agent.
pub struct SubagentTool {
    executor: Arc<dyn SubagentExecutor>,
}

impl SubagentTool {
    pub fn new(executor: Arc<dyn SubagentExecutor>) -> Self {
        Self { executor }
    }
}

#[async_trait]
impl Tool for SubagentTool {
    fn name(&self) -> &str { "subagent" }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "A clear, focused task for the subagent to complete"
                },
                "context": {
                    "type": "string",
                    "description": "Additional context the subagent needs"
                },
                "tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Tools the subagent may use (default: read-only)"
                }
            },
            "required": ["task"]
        })
    }

    fn description(&self) -> &str {
        "Spawn a focused subagent for an independent subtask. The subagent gets its own context \
         window and returns a summary. Use for research, reading multiple files, or any task \
         that can run independently."
    }

    fn is_mutating(&self) -> bool { false }

    fn requires_approval(&self, mode: ApprovalMode) -> bool {
        matches!(mode, ApprovalMode::Suggest)
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let task = args["task"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArgs("missing 'task'".into()))?
            .to_string();

        let context_info = args["context"].as_str().unwrap_or("").to_string();

        let allowed_tools: Vec<String> = args["tools"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_else(|| vec![
                "read_file".into(), "grep".into(), "glob".into(), "list_directory".into(),
            ]);

        match self.executor.run_subagent(
            task.clone(),
            context_info,
            allowed_tools,
            ctx.project_root.clone(),
            cancel,
        ).await {
            Ok(result) => Ok(ToolResult::text(result)),
            Err(e) => Ok(ToolResult::error(format!("Subagent failed: {}", e))),
        }
    }
}
