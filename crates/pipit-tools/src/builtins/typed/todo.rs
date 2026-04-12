//! TodoWrite — replace-all semantics with Ledger-backed persistence (Task 5).
//!
//! One tool, one verb: the model replaces the entire todo list each call.
//! State is persisted as a `SessionEvent::TodoWrite` in the ledger,
//! reconstructable on session resume and branch switch.
//!
//! Replaces `UnifiedTaskTool` which had:
//!   (a) `Arc<Mutex<TaskStore>>` with std::sync::Mutex in async code
//!   (b) Six actions forcing the model to pick between create/update/get/list/stop/output
//!   (c) No durability or branching support

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::typed_tool::*;
use crate::{ToolContext, ToolError};

/// Input: the model sends the complete todo list every time (replace-all).
/// This is idempotent: `apply(apply(s, t)) == apply(s, t)`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TodoWriteInput {
    /// The complete list of todos. Replaces any previous list.
    /// Each item has content, status, and optionally active_form.
    ///
    /// Rules:
    /// - Exactly ONE item must have status "in_progress" at any time (not less, not more)
    /// - Include ALL items — both existing and new — in every call
    /// - When all items are completed, the list auto-clears
    pub todos: Vec<TodoItemInput>,
}

/// A single todo item from the model.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TodoItemInput {
    /// Concise action-oriented description (3-10 words).
    pub content: String,
    /// Human-readable active form for TUI display (e.g. "Running cargo test…").
    /// Only meaningful when status is "in_progress".
    #[serde(default)]
    pub active_form: Option<String>,
    /// Status: pending, in_progress, or completed.
    pub status: TodoStatusInput,
}

/// Three-state status matching the ledger's TodoStatus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatusInput {
    Pending,
    InProgress,
    Completed,
}

/// The TodoWrite tool.
pub struct TodoWriteTool;

#[async_trait]
impl TypedTool for TodoWriteTool {
    type Input = TodoWriteInput;
    const NAME: &'static str = "todo";
    const CAPABILITIES: CapabilitySet = CapabilitySet::SESSION_WRITE;
    const PURITY: Purity = Purity::Mutating;

    // Task 8: Rich tool prompt calibrated to abc-src density
    fn describe() -> ToolCard {
        ToolCard {
            name: "todo".into(),
            summary: "Track and manage your task list with replace-all semantics".into(),
            when_to_use: r#"Use this tool to plan and track progress on multi-step tasks. Call it to set up your task list, mark tasks in progress, and complete them.

== RULES (STRICT) ==
1. Exactly ONE task must be "in_progress" at any time — not zero, not two.
2. Every call REPLACES the entire list. Include ALL items (existing + new).
3. Mark a task "in_progress" BEFORE starting work on it.
4. Mark it "completed" IMMEDIATELY after finishing — do not batch completions.
5. When ALL items are completed, the list auto-clears.
6. Keep task descriptions concise: 3-10 words, action-oriented.

== WHEN TO USE ==
- Complex multi-step work that benefits from visible progress tracking
- When the user provides multiple tasks or numbered requests
- After receiving new instructions that require 3+ steps
- Before starting work on any task (mark in_progress)
- After completing each task (mark completed individually)

== WHEN NOT TO USE ==
- Single, trivial tasks completable in one step
- Purely conversational/informational requests
- Simple file reads or searches

== EXAMPLES ==

<reasoning>I need to implement auth and write tests. Let me plan the tasks.</reasoning>
{ "todos": [
    { "content": "Read existing auth module", "status": "in_progress", "active_form": "Reading auth.rs…" },
    { "content": "Implement JWT validation", "status": "pending" },
    { "content": "Write unit tests for auth", "status": "pending" },
    { "content": "Run tests and verify", "status": "pending" }
] }

<reasoning>Auth module read complete. Moving to JWT implementation.</reasoning>
{ "todos": [
    { "content": "Read existing auth module", "status": "completed" },
    { "content": "Implement JWT validation", "status": "in_progress", "active_form": "Editing auth.rs…" },
    { "content": "Write unit tests for auth", "status": "pending" },
    { "content": "Run tests and verify", "status": "pending" }
] }

== DON'T DO THIS ==
Bad: Sending only changed items (the list is REPLACE-ALL, not a delta)
Bad: Having zero or multiple "in_progress" items
Bad: Marking everything completed in one call without doing the work
Bad: Forgetting to include previously pending items"#.into(),
            examples: vec![
                ToolExample {
                    description: "Set up a task list".into(),
                    input: serde_json::json!({
                        "todos": [
                            { "content": "Explore the codebase structure", "status": "in_progress", "active_form": "Reading project layout…" },
                            { "content": "Implement the feature", "status": "pending" },
                            { "content": "Write tests", "status": "pending" },
                            { "content": "Verify all tests pass", "status": "pending" }
                        ]
                    }),
                },
                ToolExample {
                    description: "Mark a task completed and start next".into(),
                    input: serde_json::json!({
                        "todos": [
                            { "content": "Explore the codebase structure", "status": "completed" },
                            { "content": "Implement the feature", "status": "in_progress", "active_form": "Editing main.rs…" },
                            { "content": "Write tests", "status": "pending" },
                            { "content": "Verify all tests pass", "status": "pending" }
                        ]
                    }),
                },
            ],
            tags: vec!["todo".into(), "task".into(), "plan".into(), "progress".into()],
            purity: Purity::Mutating,
            capabilities: CapabilitySet::SESSION_WRITE.0,
        }
    }

    async fn execute(
        &self,
        input: TodoWriteInput,
        _ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<TypedToolResult, ToolError> {
        let todos = &input.todos;

        // Validate: exactly one in_progress (unless all completed or list is empty)
        let in_progress_count = todos
            .iter()
            .filter(|t| matches!(t.status, TodoStatusInput::InProgress))
            .count();
        let all_completed = !todos.is_empty()
            && todos
                .iter()
                .all(|t| matches!(t.status, TodoStatusInput::Completed));
        let all_pending = !todos.is_empty()
            && todos
                .iter()
                .all(|t| matches!(t.status, TodoStatusInput::Pending));

        if !all_completed && !all_pending && !todos.is_empty() && in_progress_count != 1 {
            let hint = if in_progress_count == 0 {
                "Exactly ONE task must be in_progress. Mark the task you're about to work on."
            } else {
                "Only ONE task can be in_progress at a time. Complete or revert the others."
            };
            return Ok(TypedToolResult::text(format!(
                "⚠️ Invalid todo list: {} items are in_progress (expected 1). {hint}",
                in_progress_count,
            )));
        }

        // Build display
        let mut display = String::new();
        for (i, todo) in todos.iter().enumerate() {
            let icon = match todo.status {
                TodoStatusInput::Pending => "○",
                TodoStatusInput::InProgress => "◉",
                TodoStatusInput::Completed => "✓",
            };
            let form = todo
                .active_form
                .as_deref()
                .unwrap_or(&todo.content);
            display.push_str(&format!("{} {}. {}\n", icon, i + 1, form));
        }

        // Task 6: Verification nudge — if this is an all-done list of ≥3 items
        // and none mention "verif", append a reminder.
        let mut nudge = String::new();
        if all_completed && todos.len() >= 3 {
            let has_verification = todos.iter().any(|t| {
                t.content.to_ascii_lowercase().contains("verif")
                    || t.content.to_ascii_lowercase().contains("test")
            });
            if !has_verification {
                nudge = "\n\n⚠️ VERIFICATION REMINDER: You completed 3+ tasks without any \
                         verification step. Before writing your final summary, spawn a \
                         verification subagent to confirm the changes work correctly:\n\
                         subagent({ task: \"Verify the recent changes by running tests and \
                         checking for regressions\", tools: [\"read_file\", \"bash\", \"grep\"] })"
                    .to_string();
            }
        }

        let summary = if all_completed {
            format!(
                "All {} tasks completed. ✓\n{}{nudge}",
                todos.len(),
                display,
            )
        } else {
            let completed = todos
                .iter()
                .filter(|t| matches!(t.status, TodoStatusInput::Completed))
                .count();
            let pending = todos
                .iter()
                .filter(|t| matches!(t.status, TodoStatusInput::Pending))
                .count();
            format!(
                "{}/{} tasks done, {} pending, {} in progress\n{}",
                completed,
                todos.len(),
                pending,
                in_progress_count,
                display,
            )
        };

        let mut result = TypedToolResult::mutating(summary);
        result.artifacts.push(ArtifactKind::Custom {
            kind: format!("todo_write:{}_items", todos.len()),
        });

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn todo_write_basic() {
        let tool = TodoWriteTool;
        let ctx = ToolContext::new(PathBuf::from("/tmp"), pipit_config::ApprovalMode::FullAuto);
        let cancel = CancellationToken::new();

        let result = tool
            .execute(
                TodoWriteInput {
                    todos: vec![
                        TodoItemInput {
                            content: "Read code".into(),
                            active_form: Some("Reading…".into()),
                            status: TodoStatusInput::InProgress,
                        },
                        TodoItemInput {
                            content: "Write tests".into(),
                            active_form: None,
                            status: TodoStatusInput::Pending,
                        },
                    ],
                },
                &ctx,
                cancel,
            )
            .await
            .unwrap();
        assert!(result.mutated);
        assert!(result.content.contains("0/2 tasks done"));
    }

    #[tokio::test]
    async fn todo_write_rejects_multiple_in_progress() {
        let tool = TodoWriteTool;
        let ctx = ToolContext::new(PathBuf::from("/tmp"), pipit_config::ApprovalMode::FullAuto);
        let cancel = CancellationToken::new();

        let result = tool
            .execute(
                TodoWriteInput {
                    todos: vec![
                        TodoItemInput {
                            content: "Task A".into(),
                            active_form: None,
                            status: TodoStatusInput::InProgress,
                        },
                        TodoItemInput {
                            content: "Task B".into(),
                            active_form: None,
                            status: TodoStatusInput::InProgress,
                        },
                    ],
                },
                &ctx,
                cancel,
            )
            .await
            .unwrap();
        assert!(result.content.contains("Invalid todo list"));
    }

    #[tokio::test]
    async fn todo_write_verification_nudge() {
        let tool = TodoWriteTool;
        let ctx = ToolContext::new(PathBuf::from("/tmp"), pipit_config::ApprovalMode::FullAuto);
        let cancel = CancellationToken::new();

        let result = tool
            .execute(
                TodoWriteInput {
                    todos: vec![
                        TodoItemInput {
                            content: "Read code".into(),
                            active_form: None,
                            status: TodoStatusInput::Completed,
                        },
                        TodoItemInput {
                            content: "Write implementation".into(),
                            active_form: None,
                            status: TodoStatusInput::Completed,
                        },
                        TodoItemInput {
                            content: "Update docs".into(),
                            active_form: None,
                            status: TodoStatusInput::Completed,
                        },
                    ],
                },
                &ctx,
                cancel,
            )
            .await
            .unwrap();
        // Should trigger verification nudge — 3+ completed, none mention "verif" or "test"
        assert!(result.content.contains("VERIFICATION REMINDER"));
    }

    #[tokio::test]
    async fn todo_write_no_nudge_with_test_step() {
        let tool = TodoWriteTool;
        let ctx = ToolContext::new(PathBuf::from("/tmp"), pipit_config::ApprovalMode::FullAuto);
        let cancel = CancellationToken::new();

        let result = tool
            .execute(
                TodoWriteInput {
                    todos: vec![
                        TodoItemInput {
                            content: "Read code".into(),
                            active_form: None,
                            status: TodoStatusInput::Completed,
                        },
                        TodoItemInput {
                            content: "Write implementation".into(),
                            active_form: None,
                            status: TodoStatusInput::Completed,
                        },
                        TodoItemInput {
                            content: "Run tests and verify".into(),
                            active_form: None,
                            status: TodoStatusInput::Completed,
                        },
                    ],
                },
                &ctx,
                cancel,
            )
            .await
            .unwrap();
        // Should NOT trigger nudge — one item mentions "test"/"verify"
        assert!(!result.content.contains("VERIFICATION REMINDER"));
    }
}
