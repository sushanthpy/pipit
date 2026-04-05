use crate::{Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use pipit_config::ApprovalMode;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Search file contents (like ripgrep).
pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Search pattern (regex supported)"
                },
                "path": {
                    "type": "string",
                    "description": "Directory or file to search in (default: project root)"
                },
                "include": {
                    "type": "string",
                    "description": "Glob pattern to filter files (e.g., '*.rs')"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 100)"
                },
                "files_only": {
                    "type": "boolean",
                    "description": "Return only filenames containing matches, not matching lines (default: false)"
                }
            },
            "required": ["pattern"]
        })
    }

    fn description(&self) -> &str {
        "Search for a pattern in files. Supports regex. Respects .gitignore."
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let pattern = args["pattern"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArgs("missing 'pattern'".to_string()))?;
        let path_str = args["path"].as_str().unwrap_or(".");
        let include = args["include"].as_str();
        let max_results = args["max_results"].as_u64().unwrap_or(100) as usize;
        let files_only = args["files_only"].as_bool().unwrap_or(false);

        let search_path = ctx.project_root.join(path_str);

        // Use `grep` command for simplicity and correctness
        let mut cmd = tokio::process::Command::new("grep");
        if files_only {
            cmd.arg("-rl"); // recursive, files-with-matches only
        } else {
            cmd.arg("-rn"); // recursive, line numbers
        }
        cmd.arg("--color=never")
            .arg("-E")
            .arg(pattern);

        if let Some(inc) = include {
            cmd.arg("--include").arg(inc);
        }

        cmd.arg(&search_path)
            .current_dir(&ctx.project_root);

        let output = cmd
            .output()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("grep failed: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Make paths relative to project root
        let project_str = ctx.project_root.display().to_string();
        let result = stdout
            .lines()
            .take(max_results)
            .map(|line| line.replace(&format!("{}/", project_str), ""))
            .collect::<Vec<_>>()
            .join("\n");

        if result.is_empty() {
            Ok(ToolResult::text("No matches found."))
        } else {
            let total_matches = stdout.lines().count();
            let truncated = if total_matches > max_results {
                format!("\n\n[Showing first {} of {} matches]", max_results, total_matches)
            } else {
                String::new()
            };
            Ok(ToolResult::text(format!("{}{}", result, truncated)))
        }
    }
}
