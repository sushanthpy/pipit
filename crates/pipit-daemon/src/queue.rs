//! Priority task queue with per-project mutual exclusion.
//!
//! Invariant 1: At most `max_concurrent` tasks run simultaneously.
//! Invariant 2: At most 1 task runs per project at any time.

use crate::pool::AgentPool;
use crate::reporter::Reporter;
use crate::store::DaemonStore;

use anyhow::{anyhow, Result};
use pipit_channel::{
    NormalizedTask, TaskReceiver, TaskRecord, TaskStatus, TaskUpdate, TaskUpdateKind,
};
use std::cmp::Ordering as CmpOrdering;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Wrapper for priority-queue ordering: (priority DESC, submitted_at ASC).
struct PriorityTask(NormalizedTask);

impl PartialEq for PriorityTask {
    fn eq(&self, other: &Self) -> bool {
        self.0.priority == other.0.priority && self.0.submitted_at == other.0.submitted_at
    }
}

impl Eq for PriorityTask {}

impl PartialOrd for PriorityTask {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for PriorityTask {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        // Higher priority first, then earlier submission first
        self.0.priority.cmp(&other.0.priority)
            .then_with(|| other.0.submitted_at.cmp(&self.0.submitted_at))
    }
}

struct QueueInner {
    /// Priority heap of tasks ready for dispatch.
    heap: BinaryHeap<PriorityTask>,
    /// Per-project wait-lists for tasks blocked by project mutual exclusion.
    waitlists: HashMap<String, VecDeque<NormalizedTask>>,
    /// Currently running projects.
    running: HashSet<String>,
}

pub struct TaskQueue {
    inner: Mutex<QueueInner>,
    store: Arc<DaemonStore>,
    pool: Arc<AgentPool>,
    max_concurrent: usize,
    max_queue_depth: usize,
}

impl TaskQueue {
    pub fn new(
        store: Arc<DaemonStore>,
        pool: Arc<AgentPool>,
        max_concurrent: usize,
        max_queue_depth: usize,
    ) -> Self {
        Self {
            inner: Mutex::new(QueueInner {
                heap: BinaryHeap::new(),
                waitlists: HashMap::new(),
                running: HashSet::new(),
            }),
            store,
            pool,
            max_concurrent,
            max_queue_depth,
        }
    }

    pub async fn submit(&self, task: NormalizedTask) -> Result<TaskRecord> {
        let mut inner = self.inner.lock().await;

        let total = inner.heap.len()
            + inner.waitlists.values().map(|wl| wl.len()).sum::<usize>();
        if total >= self.max_queue_depth {
            return Err(anyhow!("queue full ({}/{})", total, self.max_queue_depth));
        }

        if !self.pool.has_project(&task.project).await {
            return Err(anyhow!("unknown project: {}", task.project));
        }

        let record = self.store.create_task(&task)?;

        tracing::info!(
            task_id = %record.task_id,
            project = %record.project,
            priority = ?record.priority,
            queue_depth = total + 1,
            "task queued"
        );

        inner.heap.push(PriorityTask(task));
        Ok(record)
    }

    pub async fn cancel_pending(&self, task_id: &str) -> Result<()> {
        let mut inner = self.inner.lock().await;

        // Check heap
        let heap_tasks: Vec<PriorityTask> = inner.heap.drain().collect();
        let mut found = false;
        for pt in heap_tasks {
            if pt.0.task_id == task_id {
                found = true;
            } else {
                inner.heap.push(pt);
            }
        }

        // Check waitlists
        if !found {
            for wl in inner.waitlists.values_mut() {
                let initial_len = wl.len();
                wl.retain(|t| t.task_id != task_id);
                if wl.len() < initial_len {
                    found = true;
                    break;
                }
            }
        }

        if found {
            self.store.update_task_status(task_id, TaskStatus::Cancelled, |r| {
                r.completed_at = Some(chrono::Utc::now());
            })?;
            tracing::info!(task_id, "pending task cancelled");
            Ok(())
        } else {
            Err(anyhow!("task '{}' not found in pending queue", task_id))
        }
    }

    pub async fn cancel_running(&self, task_id: &str) -> Result<()> {
        if let Some(record) = self.store.get_task(task_id)? {
            self.pool.cancel_task(&record.project, task_id).await?;
            self.store.update_task_status(task_id, TaskStatus::Cancelled, |r| {
                r.completed_at = Some(chrono::Utc::now());
            })?;
            Ok(())
        } else {
            Err(anyhow!("task '{}' not found", task_id))
        }
    }

    pub async fn status(&self) -> QueueStatus {
        let inner = self.inner.lock().await;
        let pending = inner.heap.len()
            + inner.waitlists.values().map(|wl| wl.len()).sum::<usize>();
        QueueStatus {
            pending_count: pending,
            running_count: inner.running.len(),
            max_concurrent: self.max_concurrent,
            max_queue_depth: self.max_queue_depth,
            running_projects: inner.running.iter().cloned().collect(),
        }
    }

    pub async fn process_loop(
        self: &Arc<Self>,
        mut task_rx: TaskReceiver,
        reporter: Arc<Reporter>,
        cancel: CancellationToken,
    ) {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!("queue processor shutting down");
                    let inner = self.inner.lock().await;
                    // Drain heap
                    for pt in inner.heap.iter() {
                        let _ = self.store.update_task_status(
                            &pt.0.task_id, TaskStatus::Failed, |r| {
                                r.error = Some("daemon shutting down".to_string());
                                r.completed_at = Some(chrono::Utc::now());
                            },
                        );
                    }
                    // Drain waitlists
                    for wl in inner.waitlists.values() {
                        for task in wl {
                            let _ = self.store.update_task_status(
                                &task.task_id, TaskStatus::Failed, |r| {
                                    r.error = Some("daemon shutting down".to_string());
                                    r.completed_at = Some(chrono::Utc::now());
                                },
                            );
                        }
                    }
                    break;
                }
                Some(task) = task_rx.recv() => {
                    match self.submit(task).await {
                        Ok(record) => tracing::debug!(task_id = %record.task_id, "task accepted"),
                        Err(e) => tracing::error!(error = %e, "failed to submit task"),
                    }
                    self.try_dispatch(&reporter).await;
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
                    self.try_dispatch(&reporter).await;
                }
            }
        }
    }

    async fn try_dispatch(self: &Arc<Self>, reporter: &Arc<Reporter>) {
        loop {
            // Pop highest-priority task from heap, stashing busy-project tasks
            let task = {
                let mut inner = self.inner.lock().await;
                if inner.running.len() >= self.max_concurrent {
                    return;
                }

                let mut stashed = Vec::new();
                let mut dispatched = None;

                while let Some(pt) = inner.heap.pop() {
                    if inner.running.contains(&pt.0.project) {
                        // Project busy — stash to per-project waitlist
                        stashed.push(pt.0);
                    } else {
                        // Dispatch this task
                        inner.running.insert(pt.0.project.clone());
                        dispatched = Some(pt.0);
                        break;
                    }
                }

                // Return stashed tasks to their wait-lists
                for task in stashed {
                    inner.waitlists
                        .entry(task.project.clone())
                        .or_default()
                        .push_back(task);
                }

                match dispatched {
                    Some(t) => t,
                    None => return,
                }
            };

            let store = self.store.clone();
            let pool = self.pool.clone();
            let reporter_clone = reporter.clone();
            let queue_ref = Arc::clone(self);
            let project = task.project.clone();
            let task_id = task.task_id.clone();
            let origin = task.origin.clone();

            tokio::spawn(async move {
                let _ = reporter_clone.handle_update(TaskUpdate::new(
                    task_id.clone(), origin.clone(),
                    TaskUpdateKind::Started { project: project.clone(), model: String::new() },
                )).await;

                match pool.execute_task(&task, &store).await {
                    Ok(record) => {
                        let kind = match record.status {
                            TaskStatus::Completed => TaskUpdateKind::Completed {
                                summary: record.result_summary.unwrap_or_default(),
                                turns: record.turns.unwrap_or(0),
                                cost: record.cost.unwrap_or(0.0),
                                files_modified: record.files_modified,
                            },
                            TaskStatus::Cancelled => TaskUpdateKind::Cancelled,
                            _ => TaskUpdateKind::Error {
                                message: record.error.unwrap_or_else(|| "unknown error".to_string()),
                            },
                        };
                        let _ = reporter_clone.handle_update(TaskUpdate::new(task_id, origin, kind)).await;
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "task execution failed");
                        let _ = reporter_clone.handle_update(TaskUpdate::new(
                            task_id.clone(), origin,
                            TaskUpdateKind::Error { message: e.to_string() },
                        )).await;
                        let _ = store.update_task_status(&task_id, TaskStatus::Failed, |r| {
                            r.completed_at = Some(chrono::Utc::now());
                            r.error = Some(e.to_string());
                        });
                    }
                }

                // Mark project idle and drain its waitlist back into the heap
                let mut inner = queue_ref.inner.lock().await;
                inner.running.remove(&project);

                // Re-queue any waiting tasks for this project
                if let Some(mut wl) = inner.waitlists.remove(&project) {
                    for task in wl.drain(..) {
                        inner.heap.push(PriorityTask(task));
                    }
                }
            });
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct QueueStatus {
    pub pending_count: usize,
    pub running_count: usize,
    pub max_concurrent: usize,
    pub max_queue_depth: usize,
    pub running_projects: Vec<String>,
}
