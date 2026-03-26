use crate::{Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use pipit_config::ApprovalMode;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Read file contents with optional line range.
pub struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path relative to project root"
                },
                "start_line": {
                    "type": "integer",
                    "description": "Starting line number (1-indexed, optional)"
                },
                "end_line": {
                    "type": "integer",
                    "description": "Ending line number (1-indexed, inclusive, optional)"
                }
            },
            "required": ["path"]
        })
    }

    fn description(&self) -> &str {
        "Read the contents of a file. Supports optional line ranges."
    }

    fn is_mutating(&self) -> bool {
        false
    }

    fn requires_approval(&self, _mode: ApprovalMode) -> bool {
        false
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let path_str = args["path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArgs("missing 'path'".to_string()))?;

        let abs_path = ctx.project_root.join(path_str);

        // Security: prevent path traversal
        let canonical = abs_path
            .canonicalize()
            .map_err(|e| ToolError::ExecutionFailed(format!("Cannot resolve path: {}", e)))?;

        let project_canonical = ctx
            .project_root
            .canonicalize()
            .map_err(|e| ToolError::ExecutionFailed(format!("Cannot resolve project root: {}", e)))?;

        if !canonical.starts_with(&project_canonical) {
            return Err(ToolError::PermissionDenied(
                "Path is outside project root".to_string(),
            ));
        }

        let content = tokio::fs::read_to_string(&canonical)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Cannot read file: {}", e)))?;

        let start_line = args["start_line"].as_u64().map(|n| n as usize);
        let end_line = args["end_line"].as_u64().map(|n| n as usize);

        let result = match (start_line, end_line) {
            (Some(start), Some(end)) => {
                let lines: Vec<&str> = content.lines().collect();
                let start = start.saturating_sub(1); // 1-indexed to 0-indexed
                let end = end.min(lines.len());
                let selected: Vec<String> = lines[start..end]
                    .iter()
                    .enumerate()
                    .map(|(i, line)| format!("{:>4} | {}", start + i + 1, line))
                    .collect();
                selected.join("\n")
            }
            (Some(start), None) => {
                let lines: Vec<&str> = content.lines().collect();
                let start = start.saturating_sub(1);
                let selected: Vec<String> = lines[start..]
                    .iter()
                    .enumerate()
                    .map(|(i, line)| format!("{:>4} | {}", start + i + 1, line))
                    .collect();
                selected.join("\n")
            }
            _ => {
                let lines: Vec<String> = content
                    .lines()
                    .enumerate()
                    .map(|(i, line)| format!("{:>4} | {}", i + 1, line))
                    .collect();
                lines.join("\n")
            }
        };

        Ok(ToolResult::text(result))
    }
}
