use crate::{Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use pipit_config::ApprovalMode;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Write/create a file with atomic write (tempfile + rename).
pub struct WriteFileTool;

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path relative to project root"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    fn description(&self) -> &str {
        "Create or overwrite a file with the given content. Uses atomic writes."
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
        let content = args["content"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArgs("missing 'content'".to_string()))?;

        let abs_path = ctx.project_root.join(path_str);

        // Security: prevent writing outside project root
        // Normalize without requiring existence
        if let Ok(project_canonical) = ctx.project_root.canonicalize() {
            // For new files, check that the parent is within project root
            if let Some(parent) = abs_path.parent() {
                if parent.exists() {
                    if let Ok(parent_canonical) = parent.canonicalize() {
                        if !parent_canonical.starts_with(&project_canonical) {
                            return Err(ToolError::PermissionDenied(
                                "Path is outside project root".to_string(),
                            ));
                        }
                    }
                }
            }
        }

        // Create parent directories
        if let Some(parent) = abs_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| ToolError::ExecutionFailed(format!("Cannot create directories: {}", e)))?;
        }

        // Atomic write via tempfile + rename
        let dir = abs_path
            .parent()
            .ok_or_else(|| ToolError::ExecutionFailed("No parent directory".to_string()))?;

        let tmp = tempfile::NamedTempFile::new_in(dir)
            .map_err(|e| ToolError::ExecutionFailed(format!("Cannot create temp file: {}", e)))?;

        tokio::fs::write(tmp.path(), content.as_bytes())
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Cannot write temp file: {}", e)))?;

        // Preserve permissions if file exists
        if abs_path.exists() {
            if let Ok(metadata) = tokio::fs::metadata(&abs_path).await {
                if let Err(e) = tokio::fs::set_permissions(tmp.path(), metadata.permissions()).await {
                    tracing::warn!("Failed to preserve permissions for {}: {}", path_str, e);
                }
            }
        }

        tmp.persist(&abs_path)
            .map_err(|e| ToolError::ExecutionFailed(format!("Cannot persist file: {}", e)))?;

        let line_count = content.lines().count();
        Ok(ToolResult::mutating(format!(
            "Successfully wrote {} lines to {}",
            line_count, path_str
        )))
    }
}
