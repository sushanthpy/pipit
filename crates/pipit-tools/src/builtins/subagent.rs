use crate::{Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use pipit_config::ApprovalMode;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Transcript entry for a subagent invocation, persisted for session resume.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SubagentTranscript {
    pub id: String,
    pub parent_session_id: String,
    /// Lineage branch ID from the core lineage DAG (if available).
    /// Links this transcript to the runtime execution graph.
    #[serde(default)]
    pub lineage_branch_id: Option<String>,
    /// Parent lineage branch ID (for causal tracing).
    #[serde(default)]
    pub parent_lineage_id: Option<String>,
    pub task: String,
    pub allowed_tools: Vec<String>,
    pub isolated: bool,
    pub output: String,
    pub started_at: String,
    pub duration_ms: u64,
    pub merge_contract: Option<MergeContractData>,
}

impl SubagentTranscript {
    /// Persist this transcript to disk in the session directory.
    pub fn persist(&self, session_dir: &std::path::Path) -> Result<(), String> {
        let subagent_dir = session_dir.join("subagents");
        std::fs::create_dir_all(&subagent_dir)
            .map_err(|e| format!("Failed to create subagent dir: {e}"))?;
        let path = subagent_dir.join(format!("{}.json", self.id));
        let json =
            serde_json::to_string_pretty(self).map_err(|e| format!("Serialize error: {e}"))?;
        std::fs::write(&path, json).map_err(|e| format!("Write error: {e}"))?;
        Ok(())
    }

    /// Load all subagent transcripts from a session directory.
    pub fn load_all(session_dir: &std::path::Path) -> Vec<Self> {
        let dir = session_dir.join("subagents");
        let mut transcripts = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|e| e.to_str()) == Some("json") {
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        if let Ok(transcript) = serde_json::from_str::<Self>(&content) {
                            transcripts.push(transcript);
                        }
                    }
                }
            }
        }
        transcripts.sort_by(|a, b| a.started_at.cmp(&b.started_at));
        transcripts
    }
}

/// Callback for subagent execution, provided by the application layer
/// to break the pipit-tools → pipit-core circular dependency.
#[async_trait]
pub trait SubagentExecutor: Send + Sync {
    async fn run_subagent(
        &self,
        task: String,
        context: String,
        allowed_tools: Vec<String>,
        project_root: std::path::PathBuf,
        cancel: CancellationToken,
    ) -> Result<String, String>;

    /// Run a subagent in an isolated git worktree.
    /// Changes are made in the worktree and can be merged back selectively.
    async fn run_subagent_isolated(
        &self,
        task: String,
        context: String,
        allowed_tools: Vec<String>,
        project_root: std::path::PathBuf,
        cancel: CancellationToken,
    ) -> Result<IsolatedResult, String> {
        // Default: fall back to non-isolated execution
        let result = self
            .run_subagent(task, context, allowed_tools, project_root, cancel)
            .await?;
        Ok(IsolatedResult {
            output: result,
            worktree_path: None,
            branch_name: None,
            diff: None,
            merge_contract: None,
            merge_ready: false,
        })
    }
}

/// Result from a worktree-isolated subagent execution.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IsolatedResult {
    pub output: String,
    pub worktree_path: Option<std::path::PathBuf>,
    pub branch_name: Option<String>,
    pub diff: Option<String>,
    /// Structured merge contract — machine-checkable, not a UI hint.
    pub merge_contract: Option<MergeContractData>,
    pub merge_ready: bool,
}

/// Serializable merge contract data for the subagent boundary.
/// Mirrors pipit-core's MergeContract but avoids circular deps.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MergeContractData {
    pub changed_files: Vec<String>,
    pub verification_obligations: Vec<String>,
    pub rollback_point: String,
    pub confidence: f32,
    pub self_reported_complete: bool,
}

/// Subagent tool — delegates focused subtasks to a child agent.
pub struct SubagentTool {
    executor: Arc<dyn SubagentExecutor>,
}

impl SubagentTool {
    pub fn new(executor: Arc<dyn SubagentExecutor>) -> Self {
        Self { executor }
    }
}

#[async_trait]
impl Tool for SubagentTool {
    fn name(&self) -> &str {
        "subagent"
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "A clear, focused task for the subagent to complete"
                },
                "context": {
                    "type": "string",
                    "description": "Additional context the subagent needs"
                },
                "tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Tools the subagent may use (default: read-only)"
                },
                "isolated": {
                    "type": "boolean",
                    "description": "Run in an isolated git worktree. Changes won't affect the main branch until explicitly merged. Use for risky modifications or parallel work."
                }
            },
            "required": ["task"]
        })
    }

    fn description(&self) -> &str {
        "Spawn a focused subagent for an independent subtask. The subagent gets its own context \
         window and returns a summary. Use for research, reading multiple files, or any task \
         that can run independently. Pass isolated=true for worktree isolation."
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let task = args["task"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArgs("missing 'task'".into()))?
            .to_string();

        let context_info = args["context"].as_str().unwrap_or("").to_string();

        let allowed_tools: Vec<String> = args["tools"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_else(|| {
                vec![
                    "read_file".into(),
                    "grep".into(),
                    "glob".into(),
                    "list_directory".into(),
                ]
            });

        let isolated = args["isolated"].as_bool().unwrap_or(false);

        let start = std::time::Instant::now();

        if isolated {
            // Worktree-isolated execution
            match self
                .executor
                .run_subagent_isolated(
                    task.clone(),
                    context_info,
                    allowed_tools.clone(),
                    ctx.project_root.clone(),
                    cancel,
                )
                .await
            {
                Ok(result) => {
                    let mut output = result.output.clone();
                    if let Some(ref branch) = result.branch_name {
                        output.push_str(&format!("\n\n[Isolated on branch: {}]", branch));
                    }
                    if let Some(ref diff) = result.diff {
                        if !diff.is_empty() {
                            output
                                .push_str(&format!("\n[Changes: {} lines]", diff.lines().count()));
                        }
                    }
                    // Enforce structured merge contract — no merge without valid contract
                    if let Some(ref contract) = result.merge_contract {
                        if contract.self_reported_complete
                            && contract.verification_obligations.is_empty()
                            && !contract.rollback_point.is_empty()
                        {
                            output.push_str(&format!(
                                "\n[Merge contract: {} files changed, confidence {:.0}%, rollback={}]",
                                contract.changed_files.len(),
                                contract.confidence * 100.0,
                                contract.rollback_point
                            ));
                            output.push_str("\n[MERGE ALLOWED — contract verified]");
                        } else {
                            let mut reasons = Vec::new();
                            if !contract.self_reported_complete {
                                reasons.push("incomplete");
                            }
                            if !contract.verification_obligations.is_empty() {
                                reasons.push("pending verifications");
                            }
                            if contract.rollback_point.is_empty() {
                                reasons.push("no rollback point");
                            }
                            output.push_str(&format!(
                                "\n[MERGE BLOCKED — contract not satisfied: {}]",
                                reasons.join(", ")
                            ));
                        }
                    } else if result.merge_ready {
                        // Legacy path: merge_ready=true but no contract → blocked
                        output.push_str(
                            "\n[MERGE BLOCKED — no structured merge contract. \
                             Isolated branches must produce a MergeContract to merge.]",
                        );
                    }
                    Ok(ToolResult::text(output))
                }
                Err(e) => Ok(ToolResult::error(format!(
                    "Isolated subagent failed: {}",
                    e
                ))),
            }
        } else {
            // Standard non-isolated execution
            match self
                .executor
                .run_subagent(
                    task.clone(),
                    context_info,
                    allowed_tools.clone(),
                    ctx.project_root.clone(),
                    cancel,
                )
                .await
            {
                Ok(result) => {
                    // Persist transcript for session resume
                    let transcript = SubagentTranscript {
                        id: uuid::Uuid::new_v4().to_string(),
                        parent_session_id: ctx.session_id.clone().unwrap_or_default(),
                        lineage_branch_id: None, // Assigned by the runtime after spawn
                        parent_lineage_id: ctx.lineage_branch_id.clone(),
                        task: task.clone(),
                        allowed_tools: allowed_tools.clone(),
                        isolated: false,
                        output: result.clone(),
                        started_at: chrono::Utc::now().to_rfc3339(),
                        duration_ms: start.elapsed().as_millis() as u64,
                        merge_contract: None,
                    };
                    // Best-effort persistence — don't fail the tool call if persistence fails
                    let session_dir = ctx.project_root.join(".pipit").join("sessions");
                    if let Err(e) = transcript.persist(&session_dir) {
                        tracing::debug!("Subagent transcript persist failed: {}", e);
                    }
                    Ok(ToolResult::text(result))
                }
                Err(e) => Ok(ToolResult::error(format!("Subagent failed: {}", e))),
            }
        }
    }
}
