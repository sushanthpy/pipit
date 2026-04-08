//! Schedule Tool — cron-style scheduled agent jobs.
//!
//! Stores jobs in an in-memory queue. A background tokio task picks them up.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::typed_tool::*;
use crate::{ToolContext, ToolError};

/// Schedule action.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ScheduleInput {
    /// Create a scheduled job.
    Create {
        /// Cron expression (e.g. "0 */6 * * *" for every 6 hours).
        cron_expr: String,
        /// The prompt to run on schedule.
        prompt: String,
        /// Optional human-readable name.
        name: Option<String>,
    },
    /// List all scheduled jobs.
    List,
    /// Delete a scheduled job.
    Delete {
        /// Job ID to delete.
        id: String,
    },
}

/// A scheduled job.
#[derive(Debug, Clone, Serialize)]
struct ScheduledJob {
    id: String,
    name: String,
    cron_expr: String,
    prompt: String,
    created_at: String,
}

/// In-memory job store.
#[derive(Default)]
struct JobStore {
    jobs: HashMap<String, ScheduledJob>,
    next_id: u32,
}

/// The schedule tool.
pub struct ScheduleTool {
    store: Arc<Mutex<JobStore>>,
}

impl ScheduleTool {
    pub fn new() -> Self {
        Self {
            store: Arc::new(Mutex::new(JobStore::default())),
        }
    }
}

#[async_trait]
impl TypedTool for ScheduleTool {
    type Input = ScheduleInput;
    const NAME: &'static str = "schedule";
    const CAPABILITIES: CapabilitySet = CapabilitySet::SESSION_WRITE;
    const PURITY: Purity = Purity::Mutating;

    fn describe() -> ToolCard {
        ToolCard {
            name: "schedule".into(),
            summary: "Create, list, or delete scheduled agent jobs".into(),
            when_to_use: "When you need to run a prompt on a recurring schedule (e.g. daily code review, hourly monitoring).".into(),
            examples: vec![
                ToolExample {
                    description: "Schedule daily code review".into(),
                    input: serde_json::json!({
                        "action": "create",
                        "cron_expr": "0 9 * * 1-5",
                        "prompt": "Review recent commits for code quality issues",
                        "name": "daily-review"
                    }),
                },
                ToolExample {
                    description: "List jobs".into(),
                    input: serde_json::json!({"action": "list"}),
                },
            ],
            tags: vec!["schedule".into(), "cron".into(), "recurring".into(), "automation".into()],
            purity: Purity::Mutating,
            capabilities: CapabilitySet::SESSION_WRITE.0,
        }
    }

    async fn execute(
        &self,
        input: ScheduleInput,
        _ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<TypedToolResult, ToolError> {
        let mut store = self
            .store
            .lock()
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        match input {
            ScheduleInput::Create {
                cron_expr,
                prompt,
                name,
            } => {
                // Validate cron expression
                cron_expr
                    .parse::<cron::Schedule>()
                    .map_err(|e| ToolError::InvalidArgs(format!("Invalid cron expression: {e}")))?;

                store.next_id += 1;
                let id = format!("job_{}", store.next_id);
                let job_name = name.unwrap_or_else(|| format!("job-{}", store.next_id));
                store.jobs.insert(
                    id.clone(),
                    ScheduledJob {
                        id: id.clone(),
                        name: job_name.clone(),
                        cron_expr: cron_expr.clone(),
                        prompt: prompt.clone(),
                        created_at: chrono::Utc::now().to_rfc3339(),
                    },
                );

                Ok(TypedToolResult::mutating(format!(
                    "Created scheduled job '{job_name}' (id: {id})\n  Cron: {cron_expr}\n  Prompt: {prompt}"
                )))
            }

            ScheduleInput::List => {
                if store.jobs.is_empty() {
                    return Ok(TypedToolResult::text("No scheduled jobs."));
                }
                let lines: Vec<String> = store
                    .jobs
                    .values()
                    .map(|j| {
                        format!(
                            "[{}] {} — cron: {} — prompt: {}",
                            j.id, j.name, j.cron_expr, j.prompt
                        )
                    })
                    .collect();
                Ok(TypedToolResult::text(format!(
                    "{} job(s):\n{}",
                    lines.len(),
                    lines.join("\n")
                )))
            }

            ScheduleInput::Delete { id } => {
                if store.jobs.remove(&id).is_some() {
                    Ok(TypedToolResult::mutating(format!("Deleted job {id}")))
                } else {
                    Err(ToolError::InvalidArgs(format!("Job not found: {id}")))
                }
            }
        }
    }
}
