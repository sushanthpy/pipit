use crate::{Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use pipit_config::ApprovalMode;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// List files in a directory, respecting .gitignore.
pub struct ListDirectoryTool;

#[async_trait]
impl Tool for ListDirectoryTool {
    fn name(&self) -> &str {
        "list_directory"
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory path relative to project root (default: '.')"
                },
                "recursive": {
                    "type": "boolean",
                    "description": "Whether to list recursively (default: false)"
                }
            }
        })
    }

    fn description(&self) -> &str {
        "List files and directories. Respects .gitignore."
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let path_str = args["path"].as_str().unwrap_or(".");
        let recursive = args["recursive"].as_bool().unwrap_or(false);

        let abs_path = ctx.project_root.join(path_str);

        if !abs_path.exists() {
            return Err(ToolError::ExecutionFailed(format!(
                "Directory not found: {}",
                path_str
            )));
        }

        let walker = ignore::WalkBuilder::new(&abs_path)
            .hidden(true)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .max_depth(if recursive { None } else { Some(1) })
            .build();

        let mut entries = Vec::new();
        for entry in walker.flatten() {
            let path = entry.path();
            if path == abs_path {
                continue;
            }
            let rel = path
                .strip_prefix(&ctx.project_root)
                .unwrap_or(path);
            let suffix = if path.is_dir() { "/" } else { "" };
            entries.push(format!("{}{}", rel.display(), suffix));
        }

        entries.sort();
        Ok(ToolResult::text(entries.join("\n")))
    }
}
