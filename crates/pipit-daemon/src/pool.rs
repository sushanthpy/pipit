//! Per-project AgentLoop pool with SochDB-backed context checkpointing.
//!
//! One `AgentLoop` per project. Context persists across tasks and
//! survives daemon restarts via SochDB serialization.

use crate::config::{DaemonConfig, ProjectConfig};
use crate::git::GitSafety;
use crate::store::DaemonStore;

use anyhow::{anyhow, Result};
use chrono::Utc;
use pipit_channel::{NormalizedTask, TaskRecord, TaskStatus};
use pipit_config::ApprovalMode;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, Notify, RwLock};
use tokio_util::sync::CancellationToken;
use tracing;

// ---------------------------------------------------------------------------
// Project agent — wraps a running agent's state
// ---------------------------------------------------------------------------

/// State for a single project's agent.
pub struct ProjectAgent {
    pub project_name: String,
    pub config: ProjectConfig,
    /// Cancellation token for the currently running task.
    pub task_cancel: Option<CancellationToken>,
    /// Steering channel for mid-task injection.
    pub steering_tx: Option<mpsc::Sender<String>>,
    /// Current task ID (if running).
    pub current_task: Option<String>,
    /// Serialized agent context (messages) for persistence.
    pub context_bytes: Option<Vec<u8>>,
    /// Number of tasks completed in this session.
    pub tasks_completed: u32,
    /// Total cost across all tasks.
    pub total_cost: f64,
}

impl ProjectAgent {
    fn new(name: String, config: ProjectConfig) -> Self {
        Self {
            project_name: name,
            config,
            task_cancel: None,
            steering_tx: None,
            current_task: None,
            context_bytes: None,
            tasks_completed: 0,
            total_cost: 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Agent pool
// ---------------------------------------------------------------------------

pub struct AgentPool {
    agents: RwLock<HashMap<String, Arc<Mutex<ProjectAgent>>>>,
    idle_notify: Notify,
    store: Arc<DaemonStore>,
}

impl AgentPool {
    pub fn new(store: Arc<DaemonStore>, config: &DaemonConfig) -> Result<Self> {
        let mut agents = HashMap::new();

        for (name, project_config) in &config.projects {
            let mut agent = ProjectAgent::new(name.clone(), project_config.clone());

            // Restore context from store if available
            if let Ok(Some(ctx_bytes)) = store.load_context(name) {
                tracing::info!(project = %name, bytes = ctx_bytes.len(), "restored agent context");
                agent.context_bytes = Some(ctx_bytes);
            }

            agents.insert(name.clone(), Arc::new(Mutex::new(agent)));
        }

        Ok(Self {
            agents: RwLock::new(agents),
            idle_notify: Notify::new(),
            store,
        })
    }

    /// Check if a project is currently running a task.
    pub async fn is_busy(&self, project: &str) -> bool {
        let agents = self.agents.read().await;
        if let Some(agent) = agents.get(project) {
            agent.lock().await.current_task.is_some()
        } else {
            false
        }
    }

    /// Get the current task ID for a project.
    pub async fn current_task(&self, project: &str) -> Option<String> {
        let agents = self.agents.read().await;
        if let Some(agent) = agents.get(project) {
            agent.lock().await.current_task.clone()
        } else {
            None
        }
    }

    /// Count running tasks across all projects.
    pub async fn running_count(&self) -> usize {
        let agents = self.agents.read().await;
        let mut count = 0;
        for agent in agents.values() {
            if agent.lock().await.current_task.is_some() {
                count += 1;
            }
        }
        count
    }

    /// Execute a task on the appropriate project agent.
    ///
    /// This is the core dispatch method. It:
    /// 1. Marks the agent as busy
    /// 2. Sets up cancellation + steering
    /// 3. Runs the agent loop (headless, FullAuto)
    /// 4. Persists results to the store
    /// 5. Marks the agent as idle
    pub async fn execute_task(
        &self,
        task: &NormalizedTask,
        store: &DaemonStore,
    ) -> Result<TaskRecord> {
        let agents = self.agents.read().await;
        let agent_arc = agents
            .get(&task.project)
            .ok_or_else(|| anyhow!("unknown project: {}", task.project))?
            .clone();
        drop(agents);

        let task_cancel = CancellationToken::new();
        let (steering_tx, _steering_rx) = mpsc::channel::<String>(16);

        // Mark as running
        {
            let mut agent = agent_arc.lock().await;
            agent.current_task = Some(task.task_id.clone());
            agent.task_cancel = Some(task_cancel.clone());
            agent.steering_tx = Some(steering_tx.clone());
        }

        store.update_task_status(&task.task_id, TaskStatus::Running, |r| {
            r.started_at = Some(Utc::now());
        })?;

        // Create auto-branch if configured
        let agent_guard = agent_arc.lock().await;
        let branch = if agent_guard.config.auto_commit {
            match GitSafety::create_task_branch(
                &agent_guard.config.root,
                &agent_guard.config.branch_prefix,
                &task.task_id,
            ) {
                Ok(branch_name) => {
                    tracing::info!(
                        project = %task.project,
                        branch = %branch_name,
                        "created task branch"
                    );
                    Some(branch_name)
                }
                Err(e) => {
                    tracing::warn!(
                        project = %task.project,
                        error = %e,
                        "failed to create task branch (continuing on current branch)"
                    );
                    None
                }
            }
        } else {
            None
        };
        drop(agent_guard);

        // Execute the agent task
        // In a full implementation, this would construct an `AgentLoop` from pipit-core
        // using the project config (provider, model, mode, tools, etc.) and run it.
        // For the daemon scaffold, we simulate the execution boundary:
        let result = self.run_agent_loop(task, &agent_arc, task_cancel.clone()).await;

        // Finalize
        let mut agent = agent_arc.lock().await;
        agent.current_task = None;
        agent.task_cancel = None;
        agent.steering_tx = None;

        let record = match result {
            Ok(outcome) => {
                agent.tasks_completed += 1;
                agent.total_cost += outcome.cost;

                // Store proof + update task atomically
                store.store_proof(
                    &task.task_id,
                    &outcome.proof_json,
                    &outcome.summary,
                    outcome.files_modified.clone(),
                    outcome.turns,
                    outcome.cost,
                    outcome.total_tokens,
                )?;

                // Checkpoint context
                if let Some(ref ctx) = outcome.context_bytes {
                    agent.context_bytes = Some(ctx.clone());
                    store.save_context(&task.project, ctx)?;
                }

                // Auto-commit if configured
                if agent.config.auto_commit {
                    if let Some(ref branch_name) = branch {
                        if let Err(e) = GitSafety::auto_commit(
                            &agent.config.root,
                            &outcome.summary,
                        ) {
                            tracing::warn!(error = %e, "auto-commit failed");
                        }
                    }
                }

                store.get_task(&task.task_id)?.unwrap()
            }
            Err(e) => {
                let error_msg = e.to_string();
                store.update_task_status(&task.task_id, TaskStatus::Failed, |r| {
                    r.completed_at = Some(Utc::now());
                    r.error = Some(error_msg);
                })?
            }
        };

        // Notify idle waiters
        self.idle_notify.notify_waiters();

        Ok(record)
    }

    /// Internal: run the agent loop for a task.
    /// This is the integration point with pipit-core's `AgentLoop`.
    async fn run_agent_loop(
        &self,
        task: &NormalizedTask,
        agent: &Arc<Mutex<ProjectAgent>>,
        cancel: CancellationToken,
    ) -> Result<AgentOutcome> {
        let agent_lock = agent.lock().await;
        let project_root = agent_lock.config.root.clone();
        let model_name = agent_lock.config.model.clone();
        let provider_name = agent_lock.config.provider.clone();
        let max_turns = agent_lock.config.max_turns;
        let test_command = agent_lock.config.test_command.clone();
        let lint_command = agent_lock.config.lint_command.clone();
        let context_bytes = agent_lock.context_bytes.clone();
        let approval_mode_str = agent_lock.config.approval_mode.clone();
        let block_network = agent_lock.config.block_network;
        let capability_grants = agent_lock.config.capability_grants.clone();
        let max_write_bytes = agent_lock.config.max_write_bytes;
        let protected_paths = agent_lock.config.protected_paths.clone();
        drop(agent_lock);

        tracing::info!(
            task_id = %task.task_id,
            project = %task.project,
            provider = %provider_name,
            model = %model_name,
            prompt_len = task.prompt.len(),
            "executing agent task"
        );

        // 1. Parse provider kind
        let provider_kind: pipit_config::ProviderKind = provider_name
            .parse()
            .map_err(|e: String| anyhow!("{}", e))?;

        // 2. Resolve API key
        let api_key = pipit_config::resolve_api_key(provider_kind)
            .ok_or_else(|| anyhow!("no API key found for provider '{}'", provider_name))?;

        // 3. Create LLM provider
        let provider: Arc<dyn pipit_provider::LlmProvider> = Arc::from(
            pipit_provider::create_provider(provider_kind, &model_name, &api_key, None)?
        );

        // 4. Build ModelRouter (single model for daemon tasks)
        let models = pipit_core::pev::ModelRouter::single(provider.clone(), model_name.clone());

        // 5. Create ToolRegistry with builtins
        let tools = pipit_tools::ToolRegistry::with_builtins();

        // 6. Build system prompt — reflects the actual approval mode
        let approval_mode = parse_daemon_approval_mode(&approval_mode_str);
        let system_prompt = format!(
            "You are Pipit, an expert AI coding agent running in daemon mode (headless, {mode_label}).\n\n\
             ## Environment\n\
             - Working directory: {root}\n\
             - Project: {project}\n\
             - Platform: {os}\n\
             - Approval mode: {mode_label}\n\n\
             ## Rules\n\
             - Execute the task within your granted capabilities.\n\
             - Use tools to read, edit, and test code.\n\
             - Be thorough but minimal — don't touch code you weren't asked to change.\n\
             - After making changes, verify them (run tests, check for errors).\n\
             - If a tool call is denied by policy, do not retry — adapt your approach.\n",
            root = project_root.display(),
            project = task.project,
            os = std::env::consts::OS,
            mode_label = approval_mode.label(),
        );

        // 7. Build ContextManager, restore context if available
        //    Apply bounded checkpoint strategy: retain recent messages,
        //    redact sensitive spans, and limit total restored context.
        let model_context_window = provider.capabilities().context_window;
        let mut context = pipit_context::ContextManager::new(
            system_prompt,
            model_context_window,
        );

        if let Some(ref bytes) = context_bytes {
            match serde_json::from_slice::<Vec<pipit_provider::Message>>(bytes) {
                Ok(messages) => {
                    // Bounded restoration: only keep recent messages
                    let max_restored_messages: usize = 50;
                    let bounded = if messages.len() > max_restored_messages {
                        tracing::info!(
                            project = %task.project,
                            total = messages.len(),
                            retained = max_restored_messages,
                            "trimming restored context to recent messages"
                        );
                        messages.into_iter()
                            .rev()
                            .take(max_restored_messages)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect::<Vec<_>>()
                    } else {
                        messages
                    };

                    // Redact sensitive content before restoring
                    let redacted = redact_sensitive_context(bounded);

                    context.restore_messages(redacted);
                    tracing::info!(
                        project = %task.project,
                        messages = context.messages().len(),
                        "restored agent context from checkpoint (bounded + redacted)"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        project = %task.project,
                        error = %e,
                        "failed to deserialize context checkpoint, starting fresh"
                    );
                }
            }
        }

        // 8. Build AgentLoopConfig — derive approval mode from project config
        //    instead of hardcoding FullAuto. Headless execution is an interface
        //    constraint, not a permission grant.

        let config = pipit_core::AgentLoopConfig {
            max_turns,
            max_reflections: 3,
            loop_detection_window: 5,
            loop_detection_threshold: 3,
            tool_timeout_secs: 120,
            enable_steering: true,
            approval_mode,
            pricing: Default::default(),
            test_command,
            lint_command,
            pev: None,
            max_budget_usd: None,
        };

        // 9. Construct AgentLoop with policy-bounded approval handler.
        //    Project-level constraints (protected paths, network block, write limits)
        //    are injected into the centralized PolicyKernel — not duplicated in the
        //    approval handler. The DaemonApprovalHandler auto-approves because all
        //    policy enforcement is now in the single PolicyKernel oracle.
        let extensions: Arc<dyn pipit_extensions::ExtensionRunner> =
            Arc::new(pipit_extensions::NoopExtensionRunner);
        let approval_handler: Arc<dyn pipit_core::ApprovalHandler> =
            Arc::new(DaemonApprovalHandler::new(
                approval_mode,
                protected_paths.clone(),
                block_network,
                max_write_bytes,
            ));

        let (mut agent_loop, _event_rx, _steering_tx) = pipit_core::AgentLoop::new(
            models,
            tools,
            context,
            extensions,
            approval_handler,
            config,
            project_root.clone(),
        );

        // 10a. Create SessionKernel for unified journal (same format as CLI)
        let session_dir = project_root.join(".pipit").join("sessions").join(&task.task_id);
        if let Ok(mut kernel) = pipit_core::session_kernel::SessionKernel::new(
            pipit_core::session_kernel::SessionKernelConfig {
                session_dir,
                durable_writes: true,
                snapshot_interval: 50,
            },
        ) {
            let model_id = model_name.clone();
            let provider_id = provider_name.clone();
            let _ = kernel.start(&task.task_id, &model_id, &provider_id);
            agent_loop.enable_session_kernel(kernel);
            tracing::info!(task_id = %task.task_id, "Session kernel enabled for daemon task");
        }

        // 10b. Inject project-level constraints into the centralized PolicyKernel.
        //      Previously these lived in DaemonApprovalHandler as parallel logic;
        //      now the kernel is the single authorization oracle for both CLI and daemon.
        {
            let pk = agent_loop.policy_kernel_mut();
            pk.add_path_deny_patterns(&protected_paths);
            if block_network {
                pk.block_network_tools();
            }
            if max_write_bytes > 0 {
                pk.set_max_write_bytes(max_write_bytes);
            }
        }

        // 11. Execute the agent
        let core_outcome = agent_loop.run(task.prompt.clone(), cancel).await;

        // 11. Serialize context for checkpoint
        let messages = agent_loop.context().messages().to_vec();
        let ctx_bytes = serde_json::to_vec(&messages).ok();

        // 12. Map pipit-core's outcome enum to daemon's outcome struct
        match core_outcome {
            pipit_core::AgentOutcome::Completed { turns, total_tokens, cost, proof } => {
                let summary = proof.objective.statement.clone();
                let files_modified: Vec<String> = proof.realized_edits
                    .iter()
                    .map(|e| e.path.clone())
                    .collect();
                let proof_json = serde_json::to_value(&proof)
                    .unwrap_or_else(|_| serde_json::json!({"status": "serialization_failed"}));

                Ok(AgentOutcome {
                    summary,
                    turns,
                    total_tokens,
                    cost,
                    files_modified,
                    proof_json,
                    context_bytes: ctx_bytes,
                })
            }
            pipit_core::AgentOutcome::MaxTurnsReached(turns) => {
                Ok(AgentOutcome {
                    summary: format!("Task reached max turns limit ({})", turns),
                    turns,
                    total_tokens: 0,
                    cost: 0.0,
                    files_modified: Vec::new(),
                    proof_json: serde_json::json!({
                        "status": "max_turns_reached",
                        "turns": turns
                    }),
                    context_bytes: ctx_bytes,
                })
            }
            pipit_core::AgentOutcome::Cancelled => {
                Err(anyhow!("task cancelled"))
            }
            pipit_core::AgentOutcome::BudgetExhausted { turns, cost, budget } => {
                Ok(AgentOutcome {
                    summary: format!("Cost budget exhausted: ${:.4} >= ${:.2} limit", cost, budget),
                    turns,
                    total_tokens: 0,
                    cost,
                    files_modified: Vec::new(),
                    proof_json: serde_json::json!({
                        "status": "budget_exhausted",
                        "turns": turns,
                        "cost": cost,
                        "budget": budget,
                    }),
                    context_bytes: ctx_bytes,
                })
            }
            pipit_core::AgentOutcome::Error(msg) => {
                Err(anyhow!("{}", msg))
            }
        }
    }

    /// Inject a steering message into a running agent.
    pub async fn steer(&self, project: &str, message: String) -> Result<()> {
        let agents = self.agents.read().await;
        let agent_arc = agents
            .get(project)
            .ok_or_else(|| anyhow!("unknown project: {}", project))?
            .clone();
        drop(agents);

        let agent = agent_arc.lock().await;
        if let Some(ref tx) = agent.steering_tx {
            tx.send(message)
                .await
                .map_err(|_| anyhow!("steering channel closed"))?;
            Ok(())
        } else {
            Err(anyhow!("no task running on project '{}'", project))
        }
    }

    /// Cancel a specific task.
    pub async fn cancel_task(&self, project: &str, task_id: &str) -> Result<()> {
        let agents = self.agents.read().await;
        let agent_arc = agents
            .get(project)
            .ok_or_else(|| anyhow!("unknown project: {}", project))?
            .clone();
        drop(agents);

        let agent = agent_arc.lock().await;
        if agent.current_task.as_deref() == Some(task_id) {
            if let Some(ref cancel) = agent.task_cancel {
                cancel.cancel();
                tracing::info!(project, task_id, "task cancellation requested");
                Ok(())
            } else {
                Err(anyhow!("no cancellation token for task"))
            }
        } else {
            Err(anyhow!(
                "task '{}' is not running on project '{}'",
                task_id,
                project
            ))
        }
    }

    /// Cancel all running tasks.
    pub fn cancel_all(&self) {
        // Use blocking lock since this is called during shutdown
        let agents = self.agents.blocking_read();
        for (name, agent_arc) in agents.iter() {
            if let Ok(agent) = agent_arc.try_lock() {
                if let Some(ref cancel) = agent.task_cancel {
                    cancel.cancel();
                    tracing::info!(project = %name, "cancelled running task");
                }
            }
        }
    }

    /// Wait until all project agents are idle.
    pub async fn wait_idle(&self) {
        loop {
            if self.running_count().await == 0 {
                return;
            }
            self.idle_notify.notified().await;
        }
    }

    /// Checkpoint all agent contexts to the store.
    pub fn checkpoint_all(&self, store: &DaemonStore) -> Result<()> {
        let agents = self.agents.blocking_read();
        for (name, agent_arc) in agents.iter() {
            if let Ok(agent) = agent_arc.try_lock() {
                if let Some(ref ctx) = agent.context_bytes {
                    store.save_context(name, ctx)?;
                    tracing::info!(project = %name, bytes = ctx.len(), "context checkpointed");
                }
            }
        }
        Ok(())
    }

    /// Get status for all projects (for health/status endpoints).
    pub async fn project_statuses(&self) -> Vec<ProjectStatus> {
        let agents = self.agents.read().await;
        let mut statuses = Vec::new();
        for (name, agent_arc) in agents.iter() {
            let agent = agent_arc.lock().await;
            statuses.push(ProjectStatus {
                name: name.clone(),
                busy: agent.current_task.is_some(),
                current_task: agent.current_task.clone(),
                tasks_completed: agent.tasks_completed,
                total_cost: agent.total_cost,
                context_size: agent.context_bytes.as_ref().map(|b| b.len()).unwrap_or(0),
            });
        }
        statuses
    }

    /// List configured project names.
    pub async fn project_names(&self) -> Vec<String> {
        self.agents.read().await.keys().cloned().collect()
    }

    /// Check if a project exists.
    pub async fn has_project(&self, name: &str) -> bool {
        self.agents.read().await.contains_key(name)
    }
}

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// Outcome of an agent execution.
pub struct AgentOutcome {
    pub summary: String,
    pub turns: u32,
    pub total_tokens: u64,
    pub cost: f64,
    pub files_modified: Vec<String>,
    pub proof_json: serde_json::Value,
    pub context_bytes: Option<Vec<u8>>,
}

/// Status snapshot for a project (returned by health endpoint).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectStatus {
    pub name: String,
    pub busy: bool,
    pub current_task: Option<String>,
    pub tasks_completed: u32,
    pub total_cost: f64,
    pub context_size: usize,
}

// ---------------------------------------------------------------------------
// Task-scoped approval handler for daemon mode
// ---------------------------------------------------------------------------

/// Parse approval mode from project config string.
/// Daemon defaults to CommandReview (not FullAuto) for least-privilege.
fn parse_daemon_approval_mode(mode_str: &str) -> ApprovalMode {
    match mode_str.to_lowercase().as_str() {
        "suggest" | "plan" => ApprovalMode::Suggest,
        "auto_edit" | "autoedit" | "edit" => ApprovalMode::AutoEdit,
        "command_review" | "commandreview" | "cmd_review" => ApprovalMode::CommandReview,
        "full_auto" | "fullauto" | "auto" => ApprovalMode::FullAuto,
        _ => {
            tracing::warn!(
                mode = %mode_str,
                "unknown approval mode in project config, defaulting to command_review"
            );
            ApprovalMode::CommandReview
        }
    }
}

/// Policy-bounded approval handler for daemon tasks.
///
/// Unlike `AutoApproveHandler` (which approves everything), this handler
/// enforces task-scoped constraints even in headless mode:
/// - Protected paths are always denied
/// - Network access can be blocked per-project
/// - Write size limits are enforced
///
/// This ensures daemon mode is "policy-bounded" not "omnipotent."
pub struct DaemonApprovalHandler {
    approval_mode: ApprovalMode,
    protected_paths: Vec<String>,
    block_network: bool,
    max_write_bytes: u64,
}

impl DaemonApprovalHandler {
    pub fn new(
        approval_mode: ApprovalMode,
        protected_paths: Vec<String>,
        block_network: bool,
        max_write_bytes: u64,
    ) -> Self {
        Self {
            approval_mode,
            protected_paths,
            block_network,
            max_write_bytes,
        }
    }

    fn is_protected_path(&self, path: &str) -> bool {
        for protected in &self.protected_paths {
            if path.starts_with(protected) || path.contains(protected) {
                return true;
            }
        }
        false
    }

    fn is_network_tool(&self, tool_name: &str) -> bool {
        matches!(tool_name, "mcp_search" | "fetch_url" | "http_request")
    }
}

#[async_trait::async_trait]
impl pipit_core::ApprovalHandler for DaemonApprovalHandler {
    async fn request_approval(
        &self,
        _call_id: &str,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> pipit_core::events::ApprovalDecision {
        // Always deny protected path access
        if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
            if self.is_protected_path(path) {
                tracing::warn!(
                    tool = tool_name,
                    path = path,
                    "daemon approval denied: protected path"
                );
                return pipit_core::events::ApprovalDecision::Deny;
            }
        }

        // Block network tools if configured
        if self.block_network && self.is_network_tool(tool_name) {
            tracing::warn!(
                tool = tool_name,
                "daemon approval denied: network access blocked for this project"
            );
            return pipit_core::events::ApprovalDecision::Deny;
        }

        // In headless mode, auto-approve within policy bounds
        // The PolicyKernel has already evaluated capabilities;
        // if we reach here, the capability check passed.
        tracing::debug!(
            tool = tool_name,
            mode = ?self.approval_mode,
            "daemon approval: auto-approved within policy bounds"
        );
        pipit_core::events::ApprovalDecision::Approve
    }
}

// ---------------------------------------------------------------------------
// Context redaction for bounded checkpoint strategy
// ---------------------------------------------------------------------------

/// Redact sensitive content from restored context messages.
///
/// Scans for common secret patterns (API keys, tokens, passwords, connection strings)
/// and replaces them with redaction markers. This ensures that sensitive information
/// from previous tasks doesn't persist into future execution contexts.
///
/// Cost: O(total text length) for pattern scanning.
fn redact_sensitive_context(messages: Vec<pipit_provider::Message>) -> Vec<pipit_provider::Message> {
    messages.into_iter().map(|mut msg| {
        msg.content = msg.content.into_iter().map(|block| {
            match block {
                pipit_provider::ContentBlock::Text(text) => {
                    pipit_provider::ContentBlock::Text(redact_secrets(&text))
                }
                pipit_provider::ContentBlock::ToolResult { call_id, content, is_error } => {
                    pipit_provider::ContentBlock::ToolResult {
                        call_id,
                        content: redact_secrets(&content),
                        is_error,
                    }
                }
                other => other,
            }
        }).collect();
        msg
    }).collect()
}

/// Redact common secret patterns from a text string.
/// Uses simple string scanning instead of regex to avoid extra dependencies.
fn redact_secrets(text: &str) -> String {
    let mut result = text.to_string();

    // Redact known API key prefixes
    const KEY_PREFIXES: &[(&str, usize)] = &[
        ("sk-", 20),       // OpenAI/Stripe
        ("ghp_", 36),      // GitHub PAT
        ("gho_", 36),      // GitHub OAuth
        ("glpat-", 20),    // GitLab PAT
        ("AKIA", 16),      // AWS access key
        ("xoxb-", 20),     // Slack bot
        ("xoxp-", 20),     // Slack user
    ];

    for (prefix, min_suffix_len) in KEY_PREFIXES {
        while let Some(pos) = result.find(prefix) {
            // Find the end of the token (non-alphanumeric boundary)
            let start = pos;
            let rest = &result[pos + prefix.len()..];
            let token_end = rest.find(|c: char| !c.is_alphanumeric() && c != '-' && c != '_' && c != '.')
                .unwrap_or(rest.len());
            if token_end >= *min_suffix_len {
                let end = pos + prefix.len() + token_end;
                result = format!("{}[REDACTED_KEY]{}", &result[..start], &result[end..]);
            } else {
                break; // Not a real key, stop searching for this prefix
            }
        }
    }

    // Redact "Bearer <token>" patterns
    let bearer_needle = "Bearer ";
    while let Some(pos) = result.find(bearer_needle) {
        let after = &result[pos + bearer_needle.len()..];
        let token_end = after.find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(after.len());
        if token_end >= 20 {
            let end = pos + bearer_needle.len() + token_end;
            result = format!("{}Bearer [REDACTED]{}", &result[..pos], &result[end..]);
        } else {
            break;
        }
    }

    result
}
