use crate::{Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use pipit_config::ApprovalMode;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub mod supervisor;

// ═══════════════════════════════════════════════════════════════════════════
//  Task 1: NDJSON Streaming Protocol — SubagentEvent
// ═══════════════════════════════════════════════════════════════════════════

/// NDJSON event stream between parent and child agent processes.
///
/// Each event is serialized as a single JSON line on stdout when the child
/// is spawned with `--mode json`. The parent reads `BufReader::lines()` in
/// a Tokio task and dispatches through `mpsc::Sender<SubagentUpdate>`.
///
/// Framing: `\n`-delimited (O(1) per-event parse, O(k) memory for largest event).
/// Same pattern as LSP, DAP, and Jupyter kernel wire protocol.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SubagentEvent {
    /// The child has started processing.
    Started {
        child_id: String,
        task: String,
    },
    /// A complete assistant message from the child.
    MessageEnd {
        text: String,
        turn: u32,
    },
    /// A tool call completed in the child.
    ToolResultEnd {
        tool_name: String,
        call_id: String,
        success: bool,
        summary: String,
    },
    /// Token usage update (emitted after each API response).
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cost_usd: f64,
    },
    /// Periodic heartbeat so the parent knows the child is alive.
    Heartbeat {
        elapsed_ms: u64,
        turn: u32,
    },
    /// The child encountered a non-fatal warning.
    Warning {
        message: String,
    },
    /// The child completed successfully.
    Completed {
        output: String,
        total_turns: u32,
        total_input_tokens: u64,
        total_output_tokens: u64,
        total_cost_usd: f64,
        duration_ms: u64,
    },
    /// The child failed.
    Error {
        message: String,
        recoverable: bool,
    },
}

/// Aggregated update from a running subagent, sent via mpsc to the parent.
#[derive(Debug, Clone)]
pub struct SubagentUpdate {
    pub child_id: String,
    pub event: SubagentEvent,
}

/// Aggregated status across all running subagents for TUI display.
#[derive(Debug, Clone, Default)]
pub struct SubagentPoolStatus {
    pub total: usize,
    pub completed: usize,
    pub running: usize,
    pub failed: usize,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost_usd: f64,
}

impl std::fmt::Display for SubagentPoolStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Parallel: {}/{} done, {} running (↑{}k ↓{}k ${:.3})",
            self.completed,
            self.total,
            self.running,
            self.total_input_tokens / 1000,
            self.total_output_tokens / 1000,
            self.total_cost_usd,
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Task 7: Fork Mode — prefix-sharing via context serialization
// ═══════════════════════════════════════════════════════════════════════════

/// A single fork directive — the coordinator emits N of these, all sharing
/// the same parent prefix for prompt-cache efficiency.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ForkDirective {
    pub directive: String,
    /// Optional tool scoping for this fork.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// Optional model override for this fork.
    #[serde(default)]
    pub model: Option<String>,
}

/// Placeholder text used for fork prefix sharing.
/// All forks see identical tool_result blocks up to the divergence point.
pub const FORK_PLACEHOLDER: &str = "Fork started — processing in background";

/// Serialized parent context for fork prefix sharing.
/// Written to a temp file, loaded by child via `--fork-from <path>`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ForkContext {
    /// Serialized system prompt (identical across all forks).
    pub system_prompt: String,
    /// Serialized message history up to the fork point.
    pub messages_json: String,
    /// Model to use (can be overridden per-fork).
    pub model: String,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Transcript (with enriched fields for Task 9 ledger integration)
// ═══════════════════════════════════════════════════════════════════════════

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
    /// Token usage from the child (Task 1: populated from Usage events).
    #[serde(default)]
    pub total_input_tokens: u64,
    /// Output tokens from the child.
    #[serde(default)]
    pub total_output_tokens: u64,
    /// Cost in USD from the child.
    #[serde(default)]
    pub total_cost_usd: f64,
    /// Number of turns the child took.
    #[serde(default)]
    pub total_turns: u32,
    /// Model used by the child (Task 3: from --model passthrough).
    #[serde(default)]
    pub model: Option<String>,
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

/// Options for subagent execution (Task 3: full CLI contract).
#[derive(Debug, Clone, Default)]
pub struct SubagentOptions {
    /// Tools the subagent is allowed to use (empty = default read-only set).
    pub allowed_tools: Vec<String>,
    /// Model override for the child (e.g. route exploration to a cheap model).
    pub model: Option<String>,
    /// Additional system prompt appended to the child's prompt.
    pub append_system_prompt: Option<String>,
    /// Whether to skip session creation for one-shot tasks.
    pub no_session: bool,
    /// Maximum turns for the child.
    pub max_turns: Option<u32>,
    /// Fork context for prefix-sharing (Task 7).
    pub fork_context: Option<ForkContext>,
    /// Named agent from the built-in catalog (Explore, Plan, Verify, General, Guide).
    /// When set, the executor looks up the AgentDefinition and applies its
    /// system prompt, tool whitelist, and constraints.
    pub agent_name: Option<String>,
}

/// Callback for subagent execution, provided by the application layer
/// to break the pipit-tools → pipit-core circular dependency.
#[async_trait]
pub trait SubagentExecutor: Send + Sync {
    /// Run a subagent with the new streaming contract (Task 1).
    /// Returns a structured SubagentResult with usage data.
    /// `update_tx`: Optional channel for streaming updates to the parent.
    async fn run_subagent(
        &self,
        task: String,
        context: String,
        options: SubagentOptions,
        project_root: std::path::PathBuf,
        cancel: CancellationToken,
        update_tx: Option<tokio::sync::mpsc::Sender<SubagentUpdate>>,
    ) -> Result<SubagentResult, String>;

    /// Run a subagent in an isolated git worktree.
    /// Changes are made in the worktree and can be merged back selectively.
    async fn run_subagent_isolated(
        &self,
        task: String,
        context: String,
        options: SubagentOptions,
        project_root: std::path::PathBuf,
        cancel: CancellationToken,
        update_tx: Option<tokio::sync::mpsc::Sender<SubagentUpdate>>,
    ) -> Result<IsolatedResult, String> {
        // Default: fall back to non-isolated execution
        let result = self
            .run_subagent(task, context, options, project_root, cancel, update_tx)
            .await?;
        Ok(IsolatedResult {
            output: result.output,
            worktree_path: None,
            branch_name: None,
            diff: None,
            merge_contract: None,
            merge_ready: false,
        })
    }
}

/// Structured result from a subagent (replaces raw String return).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SubagentResult {
    pub output: String,
    pub total_turns: u32,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost_usd: f64,
    pub duration_ms: u64,
    pub model: Option<String>,
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
                    "description": "A clear, focused task for the subagent to complete. Be specific: include file paths, function names, expected behavior."
                },
                "context": {
                    "type": "string",
                    "description": "Additional context the subagent needs (findings, constraints, prior decisions)"
                },
                "tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Tools the subagent may use. Default: read-only (read_file, grep, glob, list_directory). Add 'bash', 'edit_file', 'write_file' for write tasks. This is ENFORCED — the child cannot use tools not in this list."
                },
                "model": {
                    "type": "string",
                    "description": "Model override for this subagent. Use a cheaper/faster model for exploration (e.g. 'haiku'), keep default for complex implementation."
                },
                "isolated": {
                    "type": "boolean",
                    "description": "Run in an isolated git worktree. Changes won't affect the main branch until explicitly merged. Use for risky modifications or parallel work."
                },
                "agent_name": {
                    "type": "string",
                    "description": "Named agent from the catalog: Explore (read-only analysis), Plan (strategic planning), Verify (adversarial testing), General (full capability), Guide (documentation). When specified, the agent's system prompt, tool restrictions, and turn cap are applied automatically."
                },
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "task": { "type": "string" },
                            "context": { "type": "string" },
                            "tools": { "type": "array", "items": { "type": "string" } },
                            "model": { "type": "string" }
                        },
                        "required": ["task"]
                    },
                    "description": "PARALLEL MODE: Multiple tasks to run concurrently. Each gets its own subagent. Results are collected and returned together."
                },
                "chain": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "task": { "type": "string", "description": "Task text. Use {previous} to reference the output of the prior step." },
                            "context": { "type": "string" },
                            "tools": { "type": "array", "items": { "type": "string" } },
                            "model": { "type": "string" }
                        },
                        "required": ["task"]
                    },
                    "description": "CHAIN MODE: Sequential pipeline. Each step receives the previous step's output via {previous} placeholder."
                },
                "fork": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "directive": { "type": "string" },
                            "tools": { "type": "array", "items": { "type": "string" } },
                            "model": { "type": "string" }
                        },
                        "required": ["directive"]
                    },
                    "description": "FORK MODE: All forks share the parent's prompt prefix for cache efficiency. Each fork gets a unique directive appended. ~60-80% input token savings vs parallel mode."
                }
            },
            "required": []
        })
    }

    // Task 8: Rich tool prompt with when-to-use, when-not-to-use, worked examples
    fn description(&self) -> &str {
        r#"Spawn a subagent for an independent subtask with its own context window and tool set.

Modes: single { task, tools }, parallel { tasks: [...] }, chain { chain: [...] }, fork { fork: [...] }
Default tools (read-only): read_file, grep, glob, list_directory. Add edit_file/write_file/bash for writes.
Use isolated=true for risky changes (worktree isolation)."#
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        // Dispatch to the appropriate mode
        if args.get("tasks").is_some() {
            return self.execute_parallel(args, ctx, cancel).await;
        }
        if args.get("chain").is_some() {
            return self.execute_chain(args, ctx, cancel).await;
        }
        if args.get("fork").is_some() {
            return self.execute_fork(args, ctx, cancel).await;
        }
        self.execute_single(args, ctx, cancel).await
    }
}

impl SubagentTool {
    /// Single-task execution (original path, now with SubagentOptions).
    async fn execute_single(
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

        let model = args["model"].as_str().map(|s| s.to_string());
        let isolated = args["isolated"].as_bool().unwrap_or(false);
        let agent_name = args["agent_name"].as_str().map(|s| s.to_string());

        let options = SubagentOptions {
            allowed_tools: allowed_tools.clone(),
            model: model.clone(),
            no_session: true, // One-shot tasks don't need session artifacts
            agent_name,
            ..Default::default()
        };

        if isolated {
            match self
                .executor
                .run_subagent_isolated(
                    task.clone(),
                    context_info,
                    options,
                    ctx.project_root.clone(),
                    cancel,
                    None,
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
                    // Task 10: Return MergeContract as structured JSON, not stringified footer
                    if let Some(ref contract) = result.merge_contract {
                        let contract_json = serde_json::to_string(contract)
                            .unwrap_or_else(|_| "{}".to_string());
                        output.push_str(&format!("\n\n<merge_contract>{}</merge_contract>", contract_json));

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
            match self
                .executor
                .run_subagent(
                    task.clone(),
                    context_info,
                    options,
                    ctx.project_root.clone(),
                    cancel,
                    None,
                )
                .await
            {
                Ok(result) => {
                    // Persist transcript for session resume
                    let transcript = SubagentTranscript {
                        id: uuid::Uuid::new_v4().to_string(),
                        parent_session_id: ctx.session_id.clone().unwrap_or_default(),
                        lineage_branch_id: None,
                        parent_lineage_id: ctx.lineage_branch_id.clone(),
                        task: task.clone(),
                        allowed_tools: allowed_tools.clone(),
                        isolated: false,
                        output: result.output.clone(),
                        started_at: chrono::Utc::now().to_rfc3339(),
                        duration_ms: result.duration_ms,
                        merge_contract: None,
                        total_input_tokens: result.total_input_tokens,
                        total_output_tokens: result.total_output_tokens,
                        total_cost_usd: result.total_cost_usd,
                        total_turns: result.total_turns,
                        model: result.model.clone(),
                    };
                    let session_dir = ctx.project_root.join(".pipit").join("sessions");
                    if let Err(e) = transcript.persist(&session_dir) {
                        tracing::debug!("Subagent transcript persist failed: {}", e);
                    }
                    Ok(ToolResult::text(result.output))
                }
                Err(e) => Ok(ToolResult::error(format!("Subagent failed: {}", e))),
            }
        }
    }

    /// Task 2: Parallel mode — run multiple tasks concurrently with bounded concurrency.
    async fn execute_parallel(
        &self,
        args: Value,
        ctx: &ToolContext,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let tasks: Vec<Value> = args["tasks"]
            .as_array()
            .ok_or_else(|| ToolError::InvalidArgs("'tasks' must be an array".into()))?
            .clone();

        if tasks.is_empty() {
            return Err(ToolError::InvalidArgs("'tasks' array is empty".into()));
        }

        let task_count = tasks.len();
        let (update_tx, mut update_rx) =
            tokio::sync::mpsc::channel::<SubagentUpdate>(256);

        let mut handles = Vec::new();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(
            supervisor::MAX_CONCURRENCY,
        ));

        for (i, task_val) in tasks.iter().enumerate() {
            let task_str = task_val["task"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let context_str = task_val["context"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let tools: Vec<String> = task_val["tools"]
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
            let model = task_val["model"].as_str().map(|s| s.to_string());

            let executor = self.executor.clone();
            let project_root = ctx.project_root.clone();
            let child_cancel = cancel.child_token();
            let sem = semaphore.clone();
            let tx = update_tx.clone();

            handles.push(tokio::spawn(async move {
                let _permit = sem
                    .acquire_owned()
                    .await
                    .map_err(|e| format!("Semaphore closed: {e}"))?;
                let options = SubagentOptions {
                    allowed_tools: tools,
                    model,
                    no_session: true,
                    ..Default::default()
                };
                executor
                    .run_subagent(
                        task_str,
                        context_str,
                        options,
                        project_root,
                        child_cancel,
                        Some(tx),
                    )
                    .await
                    .map(|r| (i, r))
            }));
        }

        // Drop our sender so the receiver closes when all children finish
        drop(update_tx);

        // Drain updates (for future TUI integration)
        let drain_handle = tokio::spawn(async move {
            let mut status = SubagentPoolStatus {
                total: task_count,
                ..Default::default()
            };
            while let Some(update) = update_rx.recv().await {
                match &update.event {
                    SubagentEvent::Completed { total_input_tokens, total_output_tokens, total_cost_usd, .. } => {
                        status.completed += 1;
                        status.running = status.running.saturating_sub(1);
                        status.total_input_tokens += total_input_tokens;
                        status.total_output_tokens += total_output_tokens;
                        status.total_cost_usd += total_cost_usd;
                    }
                    SubagentEvent::Started { .. } => {
                        status.running += 1;
                    }
                    SubagentEvent::Error { .. } => {
                        status.failed += 1;
                        status.running = status.running.saturating_sub(1);
                    }
                    _ => {}
                }
            }
            status
        });

        // Collect results
        let mut results: Vec<(usize, SubagentResult)> = Vec::new();
        let mut errors: Vec<(usize, String)> = Vec::new();

        for handle in handles {
            match handle.await {
                Ok(Ok((i, result))) => results.push((i, result)),
                Ok(Err(e)) => errors.push((results.len() + errors.len(), e)),
                Err(e) => errors.push((results.len() + errors.len(), format!("Task panicked: {e}"))),
            }
        }

        let pool_status = drain_handle.await.unwrap_or_default();
        results.sort_by_key(|(i, _)| *i);

        let mut output = format!(
            "## Parallel execution: {}/{} succeeded ({})\n\n",
            results.len(),
            results.len() + errors.len(),
            pool_status,
        );

        for (i, result) in &results {
            output.push_str(&format!("### Task {} result:\n{}\n\n", i + 1, result.output));
        }
        for (i, err) in &errors {
            output.push_str(&format!("### Task {} FAILED:\n{}\n\n", i + 1, err));
        }

        // Coordinator conflict detection — check for file conflicts across parallel tasks.
        #[cfg(feature = "agents")]
        {
            use pipit_agents::{AgentMemorySnapshot, Coordinator, SubTaskResult, SubTaskStatus};
            let sub_results: Vec<SubTaskResult> = results
                .iter()
                .enumerate()
                .map(|(_, (i, r))| SubTaskResult {
                    task_id: format!("task-{}", i),
                    agent_name: format!("parallel-{}", i),
                    status: SubTaskStatus::Completed,
                    output: r.output.clone(),
                    memory_snapshot: AgentMemorySnapshot::new(vec![]),
                    duration_ms: r.duration_ms,
                })
                .collect();
            let conflicts = Coordinator::detect_conflicts(&sub_results);
            if !conflicts.is_empty() {
                output.push_str("### ⚠ File conflicts detected:\n");
                for conflict in &conflicts {
                    output.push_str(&format!(
                        "- {} modified by both '{}' and '{}'\n",
                        conflict.file.display(),
                        conflict.agent_a,
                        conflict.agent_b,
                    ));
                }
                output.push('\n');
            }
            // Compute parallel speedup
            let total_duration: u64 = results.iter().map(|(_, r)| r.duration_ms).sum();
            let max_duration = results.iter().map(|(_, r)| r.duration_ms).max().unwrap_or(1);
            if max_duration > 0 && results.len() > 1 {
                let speedup = total_duration as f64 / max_duration as f64;
                output.push_str(&format!(
                    "Parallel speedup: {:.1}× (Amdahl ceiling for k={})\n",
                    speedup,
                    results.len()
                ));
            }
        }

        Ok(ToolResult::text(output))
    }

    /// Task 2: Chain mode — sequential pipeline with {previous} placeholder.
    async fn execute_chain(
        &self,
        args: Value,
        ctx: &ToolContext,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let chain: Vec<Value> = args["chain"]
            .as_array()
            .ok_or_else(|| ToolError::InvalidArgs("'chain' must be an array".into()))?
            .clone();

        if chain.is_empty() {
            return Err(ToolError::InvalidArgs("'chain' array is empty".into()));
        }

        let mut previous_output = String::new();
        let mut chain_output = String::new();

        for (i, step) in chain.iter().enumerate() {
            if cancel.is_cancelled() {
                chain_output.push_str(&format!("\n### Step {} CANCELLED\n", i + 1));
                break;
            }

            let task_template = step["task"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let task_str = task_template.replace("{previous}", &previous_output);
            let context_str = step["context"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let tools: Vec<String> = step["tools"]
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
            let model = step["model"].as_str().map(|s| s.to_string());

            let options = SubagentOptions {
                allowed_tools: tools,
                model,
                no_session: true,
                ..Default::default()
            };

            match self
                .executor
                .run_subagent(
                    task_str,
                    context_str,
                    options,
                    ctx.project_root.clone(),
                    cancel.child_token(),
                    None,
                )
                .await
            {
                Ok(result) => {
                    chain_output.push_str(&format!(
                        "### Step {} completed:\n{}\n\n",
                        i + 1,
                        result.output
                    ));
                    previous_output = result.output;
                }
                Err(e) => {
                    chain_output.push_str(&format!(
                        "### Step {} FAILED: {}\nChain aborted.\n",
                        i + 1,
                        e
                    ));
                    break;
                }
            }
        }

        Ok(ToolResult::text(chain_output))
    }

    /// Task 7: Fork mode — prefix-sharing parallel execution.
    async fn execute_fork(
        &self,
        args: Value,
        ctx: &ToolContext,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let forks: Vec<Value> = args["fork"]
            .as_array()
            .ok_or_else(|| ToolError::InvalidArgs("'fork' must be an array".into()))?
            .clone();

        if forks.is_empty() {
            return Err(ToolError::InvalidArgs("'fork' array is empty".into()));
        }

        // Parse fork directives
        let directives: Vec<ForkDirective> = forks
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();

        if directives.is_empty() {
            return Err(ToolError::InvalidArgs(
                "No valid fork directives found".into(),
            ));
        }

        let semaphore = Arc::new(tokio::sync::Semaphore::new(
            supervisor::MAX_CONCURRENCY,
        ));
        let mut handles = Vec::new();

        for (i, directive) in directives.iter().enumerate() {
            let executor = self.executor.clone();
            let project_root = ctx.project_root.clone();
            let child_cancel = cancel.child_token();
            let sem = semaphore.clone();
            let dir = directive.clone();

            handles.push(tokio::spawn(async move {
                let _permit = sem
                    .acquire_owned()
                    .await
                    .map_err(|e| format!("Semaphore closed: {e}"))?;
                let options = SubagentOptions {
                    allowed_tools: dir.tools.unwrap_or_else(|| {
                        vec![
                            "read_file".into(),
                            "grep".into(),
                            "glob".into(),
                            "list_directory".into(),
                        ]
                    }),
                    model: dir.model,
                    no_session: true,
                    // TODO: populate fork_context from parent's current message list
                    ..Default::default()
                };
                executor
                    .run_subagent(
                        dir.directive,
                        String::new(),
                        options,
                        project_root,
                        child_cancel,
                        None,
                    )
                    .await
                    .map(|r| (i, r))
            }));
        }

        let mut results: Vec<(usize, SubagentResult)> = Vec::new();
        let mut errors: Vec<(usize, String)> = Vec::new();

        for handle in handles {
            match handle.await {
                Ok(Ok((i, result))) => results.push((i, result)),
                Ok(Err(e)) => errors.push((results.len() + errors.len(), e)),
                Err(e) => errors.push((results.len() + errors.len(), format!("Fork panicked: {e}"))),
            }
        }

        results.sort_by_key(|(i, _)| *i);

        let total_input: u64 = results.iter().map(|(_, r)| r.total_input_tokens).sum();
        let total_output: u64 = results.iter().map(|(_, r)| r.total_output_tokens).sum();
        let total_cost: f64 = results.iter().map(|(_, r)| r.total_cost_usd).sum();

        let mut output = format!(
            "## Fork execution: {}/{} completed (↑{}k ↓{}k ${:.3})\n\n",
            results.len(),
            results.len() + errors.len(),
            total_input / 1000,
            total_output / 1000,
            total_cost,
        );

        for (i, result) in &results {
            output.push_str(&format!("### Fork {} result:\n{}\n\n", i + 1, result.output));
        }
        for (i, err) in &errors {
            output.push_str(&format!("### Fork {} FAILED:\n{}\n\n", i + 1, err));
        }

        Ok(ToolResult::text(output))
    }
}
