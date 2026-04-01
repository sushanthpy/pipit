//! Worker Actor — long-lived, event-driven skill executor.
//!
//! Fundamentally different from AgentLoop:
//! - AgentLoop: "receive prompt → run until done → exit"
//! - Worker:    "stay alive → wake on events → choose skills → pursue outcome → sleep"
//!
//! Execution model: actor with a mailbox (tokio::mpsc), supervision via
//! WorkerSupervisor, and persistent identity/memory across wake cycles.
//!
//! ```text
//! EventTrigger ──→ WorkerMailbox ──→ Worker.handle()
//!                                        ↓
//!                               SkillPipeline.execute()
//!                                        ↓
//!                               WorkerMemory.record()
//!                                        ↓
//!                               WorkerMetrics.emit()
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;

// ── Worker identity ─────────────────────────────────────────────────

/// Unique, persistent identity for a worker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct WorkerId(pub String);

/// Worker configuration — defines what a worker does and how.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerConfig {
    pub id: WorkerId,
    /// Human-readable name.
    pub name: String,
    /// Which project this worker operates on.
    pub project: String,
    /// Skill pipeline to execute (or single skill name).
    pub pipeline: Option<String>,
    /// Single skill for simple workers.
    pub skill: Option<String>,
    /// Maximum concurrent tasks.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    /// How long to keep the worker alive without events before hibernation.
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
    /// Restart policy on failure.
    #[serde(default)]
    pub restart_policy: RestartPolicy,
}

fn default_max_concurrent() -> u32 {
    1
}

fn default_idle_timeout_secs() -> u64 {
    3600
}

/// What to do when a worker crashes.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RestartPolicy {
    /// Restart immediately (max 3 retries).
    #[default]
    Always,
    /// Don't restart, let it die.
    Never,
    /// Restart with exponential backoff (1s, 2s, 4s, ...).
    Backoff,
}

// ── Worker messages ─────────────────────────────────────────────────

/// Messages the worker can receive in its mailbox.
#[derive(Debug)]
pub enum WorkerMessage {
    /// A new task event arrived (from trigger or queue).
    Task {
        task_id: String,
        prompt: String,
        inputs: HashMap<String, serde_json::Value>,
    },
    /// Steering: inject a message mid-execution.
    Steer(String),
    /// Graceful shutdown request.
    Shutdown,
    /// Health check ping — reply via oneshot.
    Ping(tokio::sync::oneshot::Sender<WorkerStatus>),
}

/// Worker lifecycle state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    /// Starting up, loading context.
    Initializing,
    /// Waiting for events.
    Idle,
    /// Executing a skill/pipeline.
    Working,
    /// Shutting down gracefully.
    ShuttingDown,
    /// Terminated.
    Stopped,
    /// Crashed — awaiting restart decision.
    Failed,
}

/// Snapshot of worker health for supervision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerStatus {
    pub id: WorkerId,
    pub state: WorkerState,
    pub tasks_handled: u64,
    pub tasks_failed: u64,
    pub uptime_secs: u64,
    pub last_active: Option<String>,
    pub current_task: Option<String>,
}

// ── Worker memory ───────────────────────────────────────────────────

/// Persistent memory for a worker — survives restarts.
/// Stored at `.pipit/workers/{worker_id}/memory.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkerMemory {
    /// Accumulated learnings from handled cases.
    pub learnings: Vec<WorkerLearning>,
    /// Running statistics.
    pub stats: WorkerLifetimeStats,
    /// Custom key-value state the worker can persist.
    pub state: HashMap<String, serde_json::Value>,
}

/// A single learning extracted from a completed task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerLearning {
    pub concept: String,
    pub outcome: String,
    pub timestamp: String,
    pub confidence: f64,
}

/// Lifetime statistics for a worker.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkerLifetimeStats {
    pub total_tasks: u64,
    pub successful_tasks: u64,
    pub failed_tasks: u64,
    pub escalated_tasks: u64,
    pub total_cost_usd: f64,
    pub total_turns: u64,
    pub avg_resolution_secs: f64,
}

impl WorkerMemory {
    /// Load from disk.
    pub fn load(project_root: &Path, worker_id: &WorkerId) -> Self {
        let path = Self::path(project_root, worker_id);
        if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Self::default()
        }
    }

    /// Persist to disk.
    pub fn save(&self, project_root: &Path, worker_id: &WorkerId) {
        let path = Self::path(project_root, worker_id);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    fn path(project_root: &Path, worker_id: &WorkerId) -> PathBuf {
        project_root
            .join(".pipit")
            .join("workers")
            .join(&worker_id.0)
            .join("memory.json")
    }

    /// Record a learning from a completed task.
    pub fn record_learning(&mut self, concept: &str, outcome: &str, confidence: f64) {
        // Check for existing learning on same concept — update if exists
        if let Some(existing) = self.learnings.iter_mut().find(|l| l.concept == concept) {
            existing.outcome = outcome.to_string();
            existing.timestamp = chrono::Utc::now().to_rfc3339();
            existing.confidence = (existing.confidence + confidence) / 2.0;
        } else {
            self.learnings.push(WorkerLearning {
                concept: concept.to_string(),
                outcome: outcome.to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                confidence,
            });
        }

        // Cap learnings at 500 most recent
        if self.learnings.len() > 500 {
            self.learnings.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
            self.learnings.truncate(500);
        }
    }

    /// Update stats with a completed task.
    pub fn record_completion(&mut self, success: bool, cost_usd: f64, turns: u32, elapsed_secs: f64) {
        self.stats.total_tasks += 1;
        if success {
            self.stats.successful_tasks += 1;
        } else {
            self.stats.failed_tasks += 1;
        }
        self.stats.total_cost_usd += cost_usd;
        self.stats.total_turns += turns as u64;

        // Running average of resolution time
        let prev_total = (self.stats.total_tasks - 1) as f64;
        let new_total = self.stats.total_tasks as f64;
        self.stats.avg_resolution_secs =
            (self.stats.avg_resolution_secs * prev_total + elapsed_secs) / new_total;
    }

    /// Get the success rate.
    pub fn success_rate(&self) -> f64 {
        if self.stats.total_tasks == 0 {
            return 1.0;
        }
        self.stats.successful_tasks as f64 / self.stats.total_tasks as f64
    }
}

// ── Worker actor ────────────────────────────────────────────────────

/// The worker actor: receives messages from its mailbox and processes them.
pub struct Worker {
    pub config: WorkerConfig,
    pub state: WorkerState,
    pub memory: WorkerMemory,
    pub started_at: std::time::Instant,
    pub tasks_handled: u64,
    pub tasks_failed: u64,
    pub current_task: Option<String>,
    rx: mpsc::Receiver<WorkerMessage>,
}

/// Handle for sending messages to a worker.
#[derive(Clone)]
pub struct WorkerHandle {
    pub id: WorkerId,
    tx: mpsc::Sender<WorkerMessage>,
}

impl WorkerHandle {
    /// Send a task to the worker.
    pub async fn send_task(
        &self,
        task_id: String,
        prompt: String,
        inputs: HashMap<String, serde_json::Value>,
    ) -> Result<(), mpsc::error::SendError<WorkerMessage>> {
        self.tx
            .send(WorkerMessage::Task {
                task_id,
                prompt,
                inputs,
            })
            .await
    }

    /// Send a steering message.
    pub async fn steer(&self, msg: String) -> Result<(), mpsc::error::SendError<WorkerMessage>> {
        self.tx.send(WorkerMessage::Steer(msg)).await
    }

    /// Request graceful shutdown.
    pub async fn shutdown(&self) -> Result<(), mpsc::error::SendError<WorkerMessage>> {
        self.tx.send(WorkerMessage::Shutdown).await
    }

    /// Ping the worker for its status.
    pub async fn ping(&self) -> Option<WorkerStatus> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tx.send(WorkerMessage::Ping(tx)).await.ok()?;
        rx.await.ok()
    }
}

impl Worker {
    /// Spawn a new worker, returning its handle.
    pub fn spawn(config: WorkerConfig, project_root: &Path) -> WorkerHandle {
        let (tx, rx) = mpsc::channel(64);
        let id = config.id.clone();
        let memory = WorkerMemory::load(project_root, &config.id);
        let project_root = project_root.to_path_buf();

        let mut worker = Worker {
            config,
            state: WorkerState::Initializing,
            memory,
            started_at: std::time::Instant::now(),
            tasks_handled: 0,
            tasks_failed: 0,
            current_task: None,
            rx,
        };

        tokio::spawn(async move {
            worker.run(&project_root).await;
        });

        WorkerHandle { id, tx }
    }

    /// Main event loop: receive messages, dispatch, record.
    async fn run(&mut self, project_root: &Path) {
        self.state = WorkerState::Idle;
        let idle_timeout = Duration::from_secs(self.config.idle_timeout_secs);

        loop {
            let msg = tokio::time::timeout(idle_timeout, self.rx.recv()).await;

            match msg {
                Ok(Some(WorkerMessage::Task { task_id, prompt, inputs })) => {
                    self.state = WorkerState::Working;
                    self.current_task = Some(task_id.clone());

                    tracing::info!(
                        worker = %self.config.id.0,
                        task = %task_id,
                        "Worker handling task"
                    );

                    // Execute the task (delegated to skill/pipeline runner)
                    let result = self.execute_task(&prompt, &inputs).await;

                    // Record in memory
                    self.memory.record_completion(
                        result.success,
                        result.cost_usd,
                        result.turns,
                        result.elapsed_secs,
                    );

                    if result.success {
                        self.tasks_handled += 1;
                    } else {
                        self.tasks_failed += 1;
                    }

                    self.memory.save(project_root, &self.config.id);
                    self.current_task = None;
                    self.state = WorkerState::Idle;
                }
                Ok(Some(WorkerMessage::Steer(msg))) => {
                    tracing::debug!(
                        worker = %self.config.id.0,
                        "Worker received steering: {}",
                        msg
                    );
                    // Steering is handled by the active agent loop if working,
                    // or stored as context for next task if idle.
                    self.memory.state.insert(
                        "last_steer".to_string(),
                        serde_json::Value::String(msg),
                    );
                }
                Ok(Some(WorkerMessage::Ping(reply))) => {
                    let _ = reply.send(self.status());
                }
                Ok(Some(WorkerMessage::Shutdown)) => {
                    tracing::info!(worker = %self.config.id.0, "Worker shutting down");
                    self.state = WorkerState::ShuttingDown;
                    self.memory.save(project_root, &self.config.id);
                    break;
                }
                Ok(None) => {
                    // Channel closed — supervisor dropped handle.
                    tracing::warn!(worker = %self.config.id.0, "Worker mailbox closed");
                    self.state = WorkerState::Stopped;
                    break;
                }
                Err(_) => {
                    // Idle timeout — hibernate.
                    tracing::info!(
                        worker = %self.config.id.0,
                        "Worker idle timeout, hibernating"
                    );
                    self.memory.save(project_root, &self.config.id);
                    // Don't break — keep listening, but could be used for
                    // resource cleanup in future.
                }
            }
        }

        self.state = WorkerState::Stopped;
    }

    /// Execute a task — placeholder for actual skill/pipeline invocation.
    /// In production this calls into the AgentLoop or SkillPipeline executor.
    async fn execute_task(
        &self,
        _prompt: &str,
        _inputs: &HashMap<String, serde_json::Value>,
    ) -> TaskResult {
        // This is the integration point where the worker calls:
        // 1. SkillPipeline::execute() for pipeline-based workers
        // 2. AgentLoop::run() for single-skill workers
        // For now, return a placeholder.
        TaskResult {
            success: true,
            cost_usd: 0.0,
            turns: 0,
            elapsed_secs: 0.0,
        }
    }

    /// Get current status snapshot.
    fn status(&self) -> WorkerStatus {
        WorkerStatus {
            id: self.config.id.clone(),
            state: self.state.clone(),
            tasks_handled: self.tasks_handled,
            tasks_failed: self.tasks_failed,
            uptime_secs: self.started_at.elapsed().as_secs(),
            last_active: None,
            current_task: self.current_task.clone(),
        }
    }
}

/// Internal task result from execution.
struct TaskResult {
    success: bool,
    cost_usd: f64,
    turns: u32,
    elapsed_secs: f64,
}

// ── Worker supervisor ───────────────────────────────────────────────

/// Manages a pool of workers with restart policies.
pub struct WorkerSupervisor {
    workers: HashMap<WorkerId, WorkerHandle>,
    configs: HashMap<WorkerId, WorkerConfig>,
    project_root: PathBuf,
    restart_counts: HashMap<WorkerId, u32>,
}

impl WorkerSupervisor {
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            workers: HashMap::new(),
            configs: HashMap::new(),
            project_root,
            restart_counts: HashMap::new(),
        }
    }

    /// Start a worker under supervision.
    pub fn start_worker(&mut self, config: WorkerConfig) -> WorkerHandle {
        let handle = Worker::spawn(config.clone(), &self.project_root);
        self.workers.insert(config.id.clone(), handle.clone());
        self.configs.insert(config.id.clone(), config);
        handle
    }

    /// Get a handle to an existing worker.
    pub fn get(&self, id: &WorkerId) -> Option<&WorkerHandle> {
        self.workers.get(id)
    }

    /// Check all workers and restart failed ones per policy.
    pub async fn health_check(&mut self) {
        let mut to_restart = Vec::new();

        for (id, handle) in &self.workers {
            if let Some(status) = handle.ping().await {
                if status.state == WorkerState::Failed || status.state == WorkerState::Stopped {
                    if let Some(config) = self.configs.get(id) {
                        match config.restart_policy {
                            RestartPolicy::Always => {
                                let count = self.restart_counts.entry(id.clone()).or_insert(0);
                                if *count < 3 {
                                    *count += 1;
                                    to_restart.push(config.clone());
                                }
                            }
                            RestartPolicy::Backoff => {
                                let count = self.restart_counts.entry(id.clone()).or_insert(0);
                                if *count < 5 {
                                    let delay = Duration::from_secs(1 << *count);
                                    tokio::time::sleep(delay).await;
                                    *count += 1;
                                    to_restart.push(config.clone());
                                }
                            }
                            RestartPolicy::Never => {}
                        }
                    }
                }
            }
        }

        for config in to_restart {
            tracing::info!(worker = %config.id.0, "Restarting failed worker");
            let handle = Worker::spawn(config.clone(), &self.project_root);
            self.workers.insert(config.id.clone(), handle);
        }
    }

    /// Shut down all workers gracefully.
    pub async fn shutdown_all(&self) {
        for (_, handle) in &self.workers {
            let _ = handle.shutdown().await;
        }
    }

    /// List all worker statuses.
    pub async fn all_statuses(&self) -> Vec<WorkerStatus> {
        let mut statuses = Vec::new();
        for (_, handle) in &self.workers {
            if let Some(status) = handle.ping().await {
                statuses.push(status);
            }
        }
        statuses
    }

    /// Number of managed workers.
    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worker_memory_persistence() {
        let id = WorkerId("test-worker".to_string());
        let mut memory = WorkerMemory::default();

        memory.record_learning("error handling", "use Result<T,E>", 0.9);
        memory.record_completion(true, 0.05, 3, 12.5);
        memory.record_completion(false, 0.08, 5, 25.0);

        assert_eq!(memory.stats.total_tasks, 2);
        assert_eq!(memory.stats.successful_tasks, 1);
        assert_eq!(memory.stats.failed_tasks, 1);
        assert!((memory.success_rate() - 0.5).abs() < 0.01);
        assert_eq!(memory.learnings.len(), 1);
    }

    #[test]
    fn test_worker_learning_dedup() {
        let mut memory = WorkerMemory::default();

        memory.record_learning("testing", "use property-based", 0.7);
        memory.record_learning("testing", "use snapshot tests", 0.9);

        // Should update existing, not duplicate
        assert_eq!(memory.learnings.len(), 1);
        assert_eq!(memory.learnings[0].outcome, "use snapshot tests");
        // Confidence averaged: (0.7 + 0.9) / 2 = 0.8
        assert!((memory.learnings[0].confidence - 0.8).abs() < 0.01);
    }
}
