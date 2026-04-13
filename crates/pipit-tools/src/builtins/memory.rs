//! Memory Tool — persistent key-value knowledge store for the agent.
//!
//! Exposes `remember(key, value)`, `forget(key)`, `recall(query)` through the
//! pipit-memory `MemoryManager` and its log-structured write pipeline.
//! All writes pass through `secret_scanner` before disk persistence.

use crate::{Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// The memory tool. Delegates to a shared `pipit_memory::MemoryManager`.
pub struct MemoryTool {
    manager: Arc<Mutex<pipit_memory::MemoryManager>>,
}

impl MemoryTool {
    pub fn new(manager: Arc<Mutex<pipit_memory::MemoryManager>>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["remember", "forget", "recall"],
                    "description": "remember: Store a key-value fact. forget: Remove a fact by key. recall: Search memory for relevant facts."
                },
                "key": {
                    "type": "string",
                    "description": "The category/key for the memory entry (e.g., 'coding-style', 'deployment-config', 'project-structure')."
                },
                "value": {
                    "type": "string",
                    "description": "The value to store (for 'remember' action). Will be secret-scanned before persistence."
                },
                "query": {
                    "type": "string",
                    "description": "Search query for 'recall' action. Matches against memory categories and content."
                }
            },
            "required": ["action"]
        })
    }

    fn description(&self) -> &str {
        "Persistent memory store. Use 'remember' to save facts, preferences, or learned patterns. Use 'recall' to retrieve them. Use 'forget' to remove outdated facts. Memory persists across sessions."
    }

    async fn execute(
        &self,
        args: Value,
        _ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let action = args["action"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArgs("missing 'action'".into()))?;

        match action {
            "remember" => {
                let key = args["key"]
                    .as_str()
                    .ok_or_else(|| ToolError::InvalidArgs("'remember' requires 'key'".into()))?;
                let value = args["value"]
                    .as_str()
                    .ok_or_else(|| ToolError::InvalidArgs("'remember' requires 'value'".into()))?;

                let mut mgr = self.manager.lock().map_err(|e| {
                    ToolError::ExecutionFailed(format!("Memory lock poisoned: {e}"))
                })?;

                // Write through the log-structured pipeline (secret scan + dedup + commit)
                mgr.append_memory(value, key, "memory_tool", 0.8)
                    .map_err(|e| ToolError::ExecutionFailed(format!("Memory write failed: {e}")))?;

                // Flush to MEMORY.md (runs secret_scanner on write)
                let (committed, rejected) = mgr.flush_pending().map_err(|e| {
                    ToolError::ExecutionFailed(format!("Memory flush failed: {e}"))
                })?;

                if rejected > 0 {
                    Ok(ToolResult::text(format!(
                        "Stored under '{}' ({} committed, {} rejected by secret scanner)",
                        key, committed, rejected
                    )))
                } else {
                    Ok(ToolResult::text(format!(
                        "Remembered under '{}': {}",
                        key,
                        if value.len() > 80 {
                            format!("{}...", &value[..80])
                        } else {
                            value.to_string()
                        }
                    )))
                }
            }
            "forget" => {
                let key = args["key"]
                    .as_str()
                    .ok_or_else(|| ToolError::InvalidArgs("'forget' requires 'key'".into()))?;

                let mut mgr = self.manager.lock().map_err(|e| {
                    ToolError::ExecutionFailed(format!("Memory lock poisoned: {e}"))
                })?;

                let mem = mgr.project_memory_mut();
                let before = mem.body.lines().count();
                // Remove lines in the matching category section.
                // A section starts at `## key` and ends at the next `## ` (same or higher level).
                // Subsection headers (`### `) within the matched section are also removed.
                let mut in_section = false;
                let mut section_level = 0u32; // heading depth of the matched section
                let mut new_body = String::new();
                for line in mem.body.lines() {
                    let heading_level = if line.starts_with("#### ") {
                        4
                    } else if line.starts_with("### ") {
                        3
                    } else if line.starts_with("## ") {
                        2
                    } else {
                        0
                    };

                    if heading_level > 0 {
                        if in_section && heading_level > section_level {
                            // Subsection of the section being removed — skip it
                            continue;
                        }
                        // Same or higher level heading — check if it matches key
                        let heading_text = line.trim_start_matches('#').trim();
                        if heading_text.eq_ignore_ascii_case(key) {
                            in_section = true;
                            section_level = heading_level;
                            continue; // skip this header
                        } else {
                            in_section = false;
                        }
                    }

                    if !in_section {
                        new_body.push_str(line);
                        new_body.push('\n');
                    }
                }
                let after = new_body.lines().count();
                mem.body = new_body.trim_end().to_string();
                let _ = mem.save();

                Ok(ToolResult::text(format!(
                    "Forgot '{}' ({} lines removed)",
                    key,
                    before.saturating_sub(after)
                )))
            }
            "recall" => {
                let query = args["query"]
                    .as_str()
                    .or_else(|| args["key"].as_str())
                    .unwrap_or("");

                let mgr = self.manager.lock().map_err(|e| {
                    ToolError::ExecutionFailed(format!("Memory lock poisoned: {e}"))
                })?;

                let prompt = mgr.build_prompt();
                if prompt.is_empty() {
                    return Ok(ToolResult::text("No memories stored yet.".to_string()));
                }

                if query.is_empty() {
                    // Return all memory
                    return Ok(ToolResult::text(prompt));
                }

                // Simple substring search across memory content
                let query_lower = query.to_lowercase();
                let matches: Vec<&str> = prompt
                    .lines()
                    .filter(|line| line.to_lowercase().contains(&query_lower))
                    .collect();

                if matches.is_empty() {
                    Ok(ToolResult::text(format!(
                        "No memories matching '{}'. Full memory:\n{}",
                        query, prompt
                    )))
                } else {
                    Ok(ToolResult::text(format!(
                        "Memories matching '{}':\n{}",
                        query,
                        matches.join("\n")
                    )))
                }
            }
            _ => Err(ToolError::InvalidArgs(format!(
                "Unknown action '{}'. Use 'remember', 'forget', or 'recall'.",
                action
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipit_config::ApprovalMode;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn make_tool(project_root: &std::path::Path) -> MemoryTool {
        let mgr = pipit_memory::MemoryManager::new(project_root);
        MemoryTool::new(Arc::new(Mutex::new(mgr)))
    }

    fn make_ctx(project_root: &std::path::Path) -> ToolContext {
        ToolContext::new(project_root.to_path_buf(), ApprovalMode::FullAuto)
    }

    #[tokio::test]
    async fn remember_stores_value() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let ctx = make_ctx(dir.path());
        let cancel = CancellationToken::new();

        let args = serde_json::json!({
            "action": "remember",
            "key": "coding-style",
            "value": "Always use explicit error handling"
        });
        let result = tool.execute(args, &ctx, cancel).await.unwrap();
        assert!(result.content.contains("Remembered under 'coding-style'"));
    }

    #[tokio::test]
    async fn recall_retrieves_stored_value() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let ctx = make_ctx(dir.path());

        // Remember first
        let args = serde_json::json!({
            "action": "remember",
            "key": "project",
            "value": "Uses Tokio for async runtime"
        });
        tool.execute(args, &ctx, CancellationToken::new())
            .await
            .unwrap();

        // Recall
        let args = serde_json::json!({
            "action": "recall",
            "query": "Tokio"
        });
        let result = tool
            .execute(args, &ctx, CancellationToken::new())
            .await
            .unwrap();
        assert!(result.content.contains("Tokio"));
    }

    #[tokio::test]
    async fn recall_empty_returns_all() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let ctx = make_ctx(dir.path());

        // Remember
        let args = serde_json::json!({
            "action": "remember",
            "key": "preferences",
            "value": "Use snake_case for variables"
        });
        tool.execute(args, &ctx, CancellationToken::new())
            .await
            .unwrap();

        // Recall with empty query returns all memory
        let args = serde_json::json!({
            "action": "recall",
            "query": ""
        });
        let result = tool
            .execute(args, &ctx, CancellationToken::new())
            .await
            .unwrap();
        assert!(result.content.contains("snake_case"));
    }

    #[tokio::test]
    async fn recall_no_memories_yet() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let ctx = make_ctx(dir.path());

        let args = serde_json::json!({
            "action": "recall",
            "query": "anything"
        });
        let result = tool
            .execute(args, &ctx, CancellationToken::new())
            .await
            .unwrap();
        assert!(result.content.contains("No memories stored yet"));
    }

    #[tokio::test]
    async fn forget_removes_section() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let ctx = make_ctx(dir.path());

        // Remember two different keys
        tool.execute(
            serde_json::json!({"action": "remember", "key": "keep-me", "value": "important fact"}),
            &ctx,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        tool.execute(
            serde_json::json!({"action": "remember", "key": "forget-me", "value": "temporary fact"}),
            &ctx,
            CancellationToken::new(),
        )
        .await
        .unwrap();

        // Forget one key
        let result = tool
            .execute(
                serde_json::json!({"action": "forget", "key": "forget-me"}),
                &ctx,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.content.contains("Forgot 'forget-me'"));

        // Recall should still have the other key
        let result = tool
            .execute(
                serde_json::json!({"action": "recall", "query": "important"}),
                &ctx,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.content.contains("important fact"));

        // Forgotten key should be gone
        let result = tool
            .execute(
                serde_json::json!({"action": "recall", "query": "temporary"}),
                &ctx,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.content.contains("temporary fact"));
    }

    #[tokio::test]
    async fn forget_removes_subsections_too() {
        let dir = tempdir().unwrap();
        // Manually create a MEMORY.md with subsections
        let pipit_dir = dir.path().join(".pipit");
        std::fs::create_dir_all(&pipit_dir).unwrap();
        std::fs::write(
            pipit_dir.join("MEMORY.md"),
            "---\nversion: 1\n---\n\n## project\n\n- fact1\n\n### sub-topic\n\n- sub-fact\n\n## preferences\n\n- keep this\n",
        )
        .unwrap();

        let tool = make_tool(dir.path());
        let ctx = make_ctx(dir.path());

        // Forget "project" — should also remove "### sub-topic"
        tool.execute(
            serde_json::json!({"action": "forget", "key": "project"}),
            &ctx,
            CancellationToken::new(),
        )
        .await
        .unwrap();

        // "preferences" section should survive
        let result = tool
            .execute(
                serde_json::json!({"action": "recall", "query": "keep"}),
                &ctx,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.content.contains("keep this"));

        // "sub-topic" should be gone from memory content
        let result = tool
            .execute(
                serde_json::json!({"action": "recall", "query": ""}),
                &ctx,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.content.contains("sub-fact"));
        assert!(!result.content.contains("fact1"));
        assert!(!result.content.contains("## project"));
    }

    #[tokio::test]
    async fn invalid_action_returns_error() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let ctx = make_ctx(dir.path());

        let result = tool
            .execute(
                serde_json::json!({"action": "invalid"}),
                &ctx,
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn remember_missing_key_returns_error() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let ctx = make_ctx(dir.path());

        let result = tool
            .execute(
                serde_json::json!({"action": "remember", "value": "no key given"}),
                &ctx,
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn remember_missing_value_returns_error() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let ctx = make_ctx(dir.path());

        let result = tool
            .execute(
                serde_json::json!({"action": "remember", "key": "k"}),
                &ctx,
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn remember_long_value_truncated_in_response() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let ctx = make_ctx(dir.path());

        let long_value = "x".repeat(200);
        let result = tool
            .execute(
                serde_json::json!({"action": "remember", "key": "k", "value": long_value}),
                &ctx,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        // Response should truncate at 80 chars
        assert!(result.content.contains("..."));
    }

    #[test]
    fn tool_metadata() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        assert_eq!(tool.name(), "memory");
        assert!(!tool.description().is_empty());
        let schema = tool.schema();
        assert!(schema["properties"]["action"].is_object());
    }
}
