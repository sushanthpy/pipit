use crate::{Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use pipit_config::ApprovalMode;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Find files by glob pattern.
pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern (e.g., '**/*.rs', 'src/**/*.ts')"
                }
            },
            "required": ["pattern"]
        })
    }

    fn description(&self) -> &str {
        "Find files matching a glob pattern. Respects .gitignore."
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
        let pattern = args["pattern"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArgs("missing 'pattern'".to_string()))?;

        let glob = globset::GlobBuilder::new(pattern)
            .literal_separator(false)
            .build()
            .map_err(|e| ToolError::InvalidArgs(format!("Invalid glob: {}", e)))?
            .compile_matcher();

        let walker = ignore::WalkBuilder::new(&ctx.project_root)
            .hidden(true)
            .git_ignore(true)
            .git_global(true)
            .build();

        let mut matches = Vec::new();
        for entry in walker.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let rel = path
                .strip_prefix(&ctx.project_root)
                .unwrap_or(path);

            if glob.is_match(rel) {
                matches.push(rel.display().to_string());
            }
        }

        matches.sort();
        if matches.is_empty() {
            Ok(ToolResult::text("No files matched the pattern."))
        } else {
            Ok(ToolResult::text(format!(
                "{}\n\n({} files)",
                matches.join("\n"),
                matches.len()
            )))
        }
    }
}
