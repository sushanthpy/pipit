use crate::{Tool, ToolContext, ToolDisplay, ToolError, ToolResult};
use async_trait::async_trait;
use pipit_config::ApprovalMode;
use serde_json::Value;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

/// Apply a search/replace edit to a file using the edit engine.
pub struct EditFileTool;

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path relative to project root"
                },
                "search": {
                    "type": "string",
                    "description": "The exact text to search for in the file"
                },
                "replace": {
                    "type": "string",
                    "description": "The replacement text"
                }
            },
            "required": ["path", "search", "replace"]
        })
    }

    fn description(&self) -> &str {
        "Apply a surgical search/replace edit to a file. The search text must match exactly \
         (whitespace-normalized fuzzy matching is used as fallback). Uses atomic writes."
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
        let search = args["search"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArgs("missing 'search'".to_string()))?;
        let replace = args["replace"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArgs("missing 'replace'".to_string()))?;

        let abs_path = ctx.project_root.join(path_str);

        // Security: prevent path traversal — fail-closed if canonicalization fails
        let project_canonical = ctx.project_root.canonicalize().map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "Cannot canonicalize project root '{}': {}",
                ctx.project_root.display(),
                e
            ))
        })?;
        if abs_path.exists() {
            let canonical = abs_path.canonicalize().map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "Cannot canonicalize path '{}': {}",
                    abs_path.display(),
                    e
                ))
            })?;
            if !canonical.starts_with(&project_canonical) {
                return Err(ToolError::PermissionDenied(
                    "Path is outside project root".to_string(),
                ));
            }
        } else {
            // For new files, verify the parent directory is within project root
            let parent = abs_path.parent().ok_or_else(|| {
                ToolError::PermissionDenied("Invalid path: no parent directory".to_string())
            })?;
            if parent.exists() {
                let parent_canonical = parent.canonicalize().map_err(|e| {
                    ToolError::ExecutionFailed(format!(
                        "Cannot canonicalize parent '{}': {}",
                        parent.display(),
                        e
                    ))
                })?;
                if !parent_canonical.starts_with(&project_canonical) {
                    return Err(ToolError::PermissionDenied(
                        "Path is outside project root".to_string(),
                    ));
                }
            }
        }

        use pipit_edit::{EditFormat, EditOp, SearchReplaceFormat};

        let op = if search.is_empty() {
            // Empty search = create file
            EditOp::CreateFile {
                path: PathBuf::from(path_str),
                content: replace.to_string(),
            }
        } else {
            EditOp::SearchReplace {
                path: PathBuf::from(path_str),
                search: search.to_string(),
                replace: replace.to_string(),
            }
        };

        let format = SearchReplaceFormat;
        match format.apply(&op, &ctx.project_root) {
            Ok(applied) => {
                let mut result = ToolResult::mutating(format!(
                    "Applied edit to {}:\n{}",
                    path_str, applied.diff
                ));
                result.display = Some(ToolDisplay::Diff {
                    path: PathBuf::from(path_str),
                    diff: applied.diff,
                });
                Ok(result)
            }
            Err(e) => Ok(ToolResult::error(format!(
                "Edit failed: {}. Make sure the search text matches the file exactly.",
                e
            ))),
        }
    }
}
