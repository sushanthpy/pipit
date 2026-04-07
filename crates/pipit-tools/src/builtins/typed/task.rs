//! Unified Task Tool — single tool for task lifecycle management.
//!
//! Schema: `{ action: create|get|list|update|stop|output, ... }`
//! Storage: in-memory task store (extensible to SessionKernel events).
//! Purity: Mutating (session state).

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::typed_tool::*;
use crate::{ToolContext, ToolError};

/// Lenient deserializer for plan steps.
/// Accepts:
///   1. A proper Vec<PlanStep> array
///   2. A JSON string containing a serialized array (models sometimes stringify)
///   3. A plain text string (split by newlines into steps)
///   4. null/missing → empty vec
fn deserialize_plan_lenient<'de, D>(deserializer: D) -> Result<Vec<PlanStep>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;

    let value: serde_json::Value = serde_json::Value::deserialize(deserializer)?;

    match &value {
        serde_json::Value::Array(arr) => {
            // Try parsing each element as a PlanStep
            let mut steps = Vec::new();
            for item in arr {
                match serde_json::from_value::<PlanStep>(item.clone()) {
                    Ok(step) => steps.push(step),
                    Err(_) => {
                        // If it's a string in the array, treat as a description
                        if let Some(desc) = item.as_str() {
                            steps.push(PlanStep {
                                description: desc.to_string(),
                                status: TaskStatus::NotStarted,
                            });
                        }
                    }
                }
            }
            Ok(steps)
        }
        serde_json::Value::String(s) => {
            // Try parsing as JSON array first
            if let Ok(steps) = serde_json::from_str::<Vec<PlanStep>>(s) {
                return Ok(steps);
            }
            // Try parsing as JSON array of strings
            if let Ok(strings) = serde_json::from_str::<Vec<String>>(s) {
                return Ok(strings.into_iter().map(|desc| PlanStep {
                    description: desc,
                    status: TaskStatus::NotStarted,
                }).collect());
            }
            // Fall back: split by newlines
            Ok(s.lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| PlanStep {
                    description: l.trim().trim_start_matches(|c: char| c == '-' || c == '*' || c == '•' || c.is_ascii_digit() || c == '.' || c == ')').trim().to_string(),
                    status: TaskStatus::NotStarted,
                })
                .collect())
        }
        serde_json::Value::Null => Ok(Vec::new()),
        _ => Err(Error::custom("plan must be an array, a JSON string, or null")),
    }
}

/// Task status.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    NotStarted,
    InProgress,
    Completed,
    Blocked,
    Cancelled,
}

/// A single step in a task plan.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanStep {
    pub description: String,
    pub status: TaskStatus,
}

/// A task in the task store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub status: TaskStatus,
    pub plan: Vec<PlanStep>,
    pub notes: String,
    pub parent: Option<String>,
    pub output: Vec<String>,
    pub created_at: String,
}

/// Filter for listing tasks.
#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
pub struct TaskFilter {
    /// Filter by status.
    pub status: Option<TaskStatus>,
    /// Filter by parent task ID.
    pub parent: Option<String>,
}

/// Unified task input — one tool, multiple actions.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum TaskInput {
    /// Create a new task with optional plan steps.
    Create {
        title: String,
        /// Plan steps as an array of {description, status} objects.
        /// Also accepts a JSON string that will be parsed, or a plain
        /// text string that will be split into steps by newline.
        #[serde(default, deserialize_with = "deserialize_plan_lenient")]
        plan: Vec<PlanStep>,
        parent: Option<String>,
    },
    /// Update a task's status or notes.
    Update {
        id: String,
        status: Option<TaskStatus>,
        notes: Option<String>,
    },
    /// Get a task by ID.
    Get {
        id: String,
    },
    /// List tasks with optional filter.
    List {
        #[serde(default)]
        filter: TaskFilter,
    },
    /// Stop/cancel a task.
    Stop {
        id: String,
        reason: String,
    },
    /// Append output to a task.
    Output {
        id: String,
        output: String,
        #[serde(default)]
        is_final: bool,
    },
}

/// In-memory task store.
#[derive(Default)]
pub struct TaskStore {
    tasks: HashMap<String, Task>,
    next_id: u32,
}

/// The unified task tool.
pub struct UnifiedTaskTool {
    store: Arc<Mutex<TaskStore>>,
}

impl UnifiedTaskTool {
    pub fn new() -> Self {
        Self {
            store: Arc::new(Mutex::new(TaskStore::default())),
        }
    }
}

#[async_trait]
impl TypedTool for UnifiedTaskTool {
    type Input = TaskInput;
    const NAME: &'static str = "task";
    const CAPABILITIES: CapabilitySet = CapabilitySet::SESSION_WRITE;
    const PURITY: Purity = Purity::Mutating;

    fn describe() -> ToolCard {
        ToolCard {
            name: "task".into(),
            summary: "Manage tasks and todo items with plan tracking".into(),
            when_to_use: "When you need to create, track, or update tasks. Use for breaking down complex work into steps, tracking progress, and managing dependencies.".into(),
            examples: vec![
                ToolExample {
                    description: "Create a task with plan".into(),
                    input: serde_json::json!({
                        "action": "create",
                        "title": "Implement auth module",
                        "plan": [
                            {"description": "Add login endpoint", "status": "not_started"},
                            {"description": "Add JWT validation", "status": "not_started"}
                        ]
                    }),
                },
                ToolExample {
                    description: "Update task status".into(),
                    input: serde_json::json!({
                        "action": "update",
                        "id": "task_1",
                        "status": "in_progress"
                    }),
                },
            ],
            tags: vec!["task".into(), "todo".into(), "plan".into(), "project".into()],
            purity: Purity::Mutating,
            capabilities: CapabilitySet::SESSION_WRITE.0,
        }
    }

    async fn execute(
        &self,
        input: TaskInput,
        _ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<TypedToolResult, ToolError> {
        let mut store = self.store.lock().map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        match input {
            TaskInput::Create { title, plan, parent } => {
                store.next_id += 1;
                let id = format!("task_{}", store.next_id);
                let task = Task {
                    id: id.clone(),
                    title: title.clone(),
                    status: TaskStatus::NotStarted,
                    plan,
                    notes: String::new(),
                    parent,
                    output: Vec::new(),
                    created_at: chrono::Utc::now().to_rfc3339(),
                };
                store.tasks.insert(id.clone(), task);
                Ok(TypedToolResult::mutating(format!(
                    "Created task '{title}' with id {id}"
                )))
            }

            TaskInput::Update { id, status, notes } => {
                let task = store.tasks.get_mut(&id)
                    .ok_or_else(|| ToolError::InvalidArgs(format!("Task not found: {id}")))?;
                if let Some(s) = status {
                    task.status = s;
                }
                if let Some(n) = notes {
                    task.notes = n;
                }
                let summary = serde_json::to_string_pretty(task)
                    .unwrap_or_else(|_| format!("Updated task {id}"));
                Ok(TypedToolResult::mutating(summary))
            }

            TaskInput::Get { id } => {
                let task = store.tasks.get(&id)
                    .ok_or_else(|| ToolError::InvalidArgs(format!("Task not found: {id}")))?;
                let json = serde_json::to_string_pretty(task)
                    .unwrap_or_else(|_| format!("Task {id}"));
                Ok(TypedToolResult::text(json))
            }

            TaskInput::List { filter } => {
                let tasks: Vec<&Task> = store.tasks.values()
                    .filter(|t| {
                        if let Some(ref status) = filter.status {
                            if t.status != *status { return false; }
                        }
                        if let Some(ref parent) = filter.parent {
                            if t.parent.as_deref() != Some(parent.as_str()) { return false; }
                        }
                        true
                    })
                    .collect();

                let summary = tasks.iter()
                    .map(|t| format!("[{}] {} ({})", t.id, t.title, format!("{:?}", t.status).to_lowercase()))
                    .collect::<Vec<_>>()
                    .join("\n");

                Ok(TypedToolResult::text(if summary.is_empty() {
                    "No tasks found.".into()
                } else {
                    format!("{} task(s):\n{}", tasks.len(), summary)
                }))
            }

            TaskInput::Stop { id, reason } => {
                let task = store.tasks.get_mut(&id)
                    .ok_or_else(|| ToolError::InvalidArgs(format!("Task not found: {id}")))?;
                task.status = TaskStatus::Cancelled;
                task.notes = format!("{}\nStopped: {}", task.notes, reason);
                Ok(TypedToolResult::mutating(format!("Stopped task {id}: {reason}")))
            }

            TaskInput::Output { id, output, is_final } => {
                let task = store.tasks.get_mut(&id)
                    .ok_or_else(|| ToolError::InvalidArgs(format!("Task not found: {id}")))?;
                task.output.push(output.clone());
                if is_final {
                    task.status = TaskStatus::Completed;
                }
                Ok(TypedToolResult::mutating(format!(
                    "Appended output to task {id}{}",
                    if is_final { " (final)" } else { "" }
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn task_create_and_get() {
        let tool = UnifiedTaskTool::new();
        let ctx = ToolContext::new(PathBuf::from("/tmp"), pipit_config::ApprovalMode::FullAuto);
        let cancel = CancellationToken::new();

        let result = tool.execute(
            TaskInput::Create {
                title: "Test task".into(),
                plan: vec![],
                parent: None,
            },
            &ctx, cancel.clone(),
        ).await.unwrap();
        assert!(result.mutated);
        assert!(result.content.contains("task_1"));

        let result = tool.execute(
            TaskInput::Get { id: "task_1".into() },
            &ctx, cancel,
        ).await.unwrap();
        assert!(result.content.contains("Test task"));
    }
}
