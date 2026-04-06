use crate::{Tool, ToolContext, ToolError, ToolResult};
use crate::builtins::read_file::FILE_STATE_CACHE;
use async_trait::async_trait;
use pipit_config::ApprovalMode;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};
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
        // Lexical normalization resolves .. without requiring existence
        let normalized = normalize_lexical(&abs_path);
        if let Ok(project_canonical) = ctx.project_root.canonicalize() {
            // Lexical check: even if parent doesn't exist, path must be under project root
            if !normalized.starts_with(&project_canonical)
                && !normalized.starts_with(&ctx.project_root)
            {
                return Err(ToolError::PermissionDenied(
                    "Path is outside project root".to_string(),
                ));
            }
            // Canonical check: when parent exists, verify with resolved symlinks
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

        // Stale-write detection: if we previously read this file, verify
        // it hasn't been modified by another tool call since then
        if abs_path.exists() {
            if let Ok(current_on_disk) = tokio::fs::read_to_string(&abs_path).await {
                if let Err(stale_msg) = FILE_STATE_CACHE.check_stale(&abs_path, &current_on_disk) {
                    return Err(ToolError::ExecutionFailed(stale_msg));
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

/// Lexically normalize a path by resolving `.` and `..` without filesystem access.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                components.pop();
            }
            Component::CurDir => {}
            other => components.push(other),
        }
    }
    components.iter().collect()
}
