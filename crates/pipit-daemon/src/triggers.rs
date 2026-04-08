//! Event Triggers — wake workers on external events.
//!
//! Three trigger types beyond the existing cron scheduler:
//! 1. FileSystemTrigger: watch paths for changes (notify crate)
//! 2. WebhookTrigger: HTTP endpoint that receives external events
//! 3. QueueTrigger: watch the task queue for matching patterns
//!
//! All triggers implement the EventTrigger trait and produce WorkerMessages.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

use crate::worker::{WorkerHandle, WorkerMessage};

// ── Trigger trait ───────────────────────────────────────────────────

/// Common interface for all event triggers.
#[async_trait::async_trait]
pub trait EventTrigger: Send + Sync + 'static {
    /// Unique identifier for this trigger.
    fn id(&self) -> &str;

    /// Start listening for events. Sends WorkerMessages to the provided sender.
    async fn start(&self, sender: mpsc::Sender<TriggerEvent>) -> Result<(), TriggerError>;

    /// Stop listening.
    async fn stop(&self) -> Result<(), TriggerError>;
}

/// An event produced by a trigger.
#[derive(Debug, Clone)]
pub struct TriggerEvent {
    /// Which trigger produced this event.
    pub trigger_id: String,
    /// What kind of event.
    pub kind: TriggerEventKind,
    /// Timestamp.
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Additional context as key-value pairs.
    pub context: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerEventKind {
    /// File was created, modified, or deleted.
    FileChanged {
        path: String,
        change_type: FileChangeType,
    },
    /// Webhook received.
    WebhookReceived {
        source: String,
        event_type: String,
        payload_preview: String,
    },
    /// Pattern matched in queue.
    QueueMatch { task_id: String, pattern: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeType {
    Created,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, thiserror::Error)]
pub enum TriggerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Watch error: {0}")]
    Watch(String),
    #[error("Trigger already running: {0}")]
    AlreadyRunning(String),
}

// ── File system trigger ─────────────────────────────────────────────

/// Watch file paths for changes and emit events.
///
/// Uses debouncing (500ms) to avoid flooding on rapid saves.
/// Supports glob patterns for path filtering.
pub struct FileSystemTrigger {
    id: String,
    /// Paths to watch (directories).
    watch_paths: Vec<PathBuf>,
    /// Glob patterns to filter — only matching files trigger events.
    include_patterns: Vec<String>,
    /// Glob patterns to exclude.
    exclude_patterns: Vec<String>,
    /// Debounce interval in milliseconds.
    debounce_ms: u64,
}

impl FileSystemTrigger {
    pub fn new(
        id: impl Into<String>,
        watch_paths: Vec<PathBuf>,
        include_patterns: Vec<String>,
        exclude_patterns: Vec<String>,
    ) -> Self {
        Self {
            id: id.into(),
            watch_paths,
            include_patterns,
            exclude_patterns,
            debounce_ms: 500,
        }
    }

    /// Check if a path matches the include/exclude filters.
    fn matches_filters(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();

        // If include patterns exist, path must match at least one
        if !self.include_patterns.is_empty() {
            let matches_include = self
                .include_patterns
                .iter()
                .any(|p| glob_match(p, &path_str));
            if !matches_include {
                return false;
            }
        }

        // Must not match any exclude pattern
        !self
            .exclude_patterns
            .iter()
            .any(|p| glob_match(p, &path_str))
    }
}

#[async_trait::async_trait]
impl EventTrigger for FileSystemTrigger {
    fn id(&self) -> &str {
        &self.id
    }

    async fn start(&self, sender: mpsc::Sender<TriggerEvent>) -> Result<(), TriggerError> {
        use tokio::sync::mpsc as tokio_mpsc;
        let (notify_tx, mut notify_rx) = tokio_mpsc::channel::<(PathBuf, FileChangeType)>(128);

        let watch_paths = self.watch_paths.clone();
        let debounce_ms = self.debounce_ms;
        let include = self.include_patterns.clone();
        let exclude = self.exclude_patterns.clone();
        let trigger_id = self.id.clone();

        // Spawn the notify watcher in a blocking thread
        std::thread::spawn(move || {
            use notify::{RecursiveMode, Watcher, event::EventKind};

            let tx = notify_tx.clone();
            let mut watcher =
                notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                    if let Ok(event) = res {
                        let change_type = match event.kind {
                            EventKind::Create(_) => FileChangeType::Created,
                            EventKind::Modify(_) => FileChangeType::Modified,
                            EventKind::Remove(_) => FileChangeType::Deleted,
                            _ => return,
                        };
                        for path in event.paths {
                            let _ = tx.blocking_send((path, change_type.clone()));
                        }
                    }
                })
                .expect("Failed to create file watcher");

            for path in &watch_paths {
                if let Err(e) = watcher.watch(path, RecursiveMode::Recursive) {
                    tracing::warn!("Failed to watch {}: {}", path.display(), e);
                }
            }

            // Keep the watcher alive
            std::thread::park();
        });

        // Spawn debounced event processor
        let trigger_id_clone = trigger_id;
        tokio::spawn(async move {
            let mut last_events: HashMap<String, std::time::Instant> = HashMap::new();
            let debounce = std::time::Duration::from_millis(debounce_ms);

            while let Some((path, change_type)) = notify_rx.recv().await {
                let path_str = path.to_string_lossy().to_string();

                // Check filters
                let matches_include =
                    include.is_empty() || include.iter().any(|p| glob_match(p, &path_str));
                let matches_exclude = exclude.iter().any(|p| glob_match(p, &path_str));

                if !matches_include || matches_exclude {
                    continue;
                }

                // Debounce: skip if we saw this path recently
                let now = std::time::Instant::now();
                if let Some(last) = last_events.get(&path_str) {
                    if now.duration_since(*last) < debounce {
                        continue;
                    }
                }
                last_events.insert(path_str.clone(), now);

                let event = TriggerEvent {
                    trigger_id: trigger_id_clone.clone(),
                    kind: TriggerEventKind::FileChanged {
                        path: path_str,
                        change_type,
                    },
                    timestamp: chrono::Utc::now(),
                    context: HashMap::new(),
                };

                if sender.send(event).await.is_err() {
                    break; // Receiver dropped
                }
            }
        });

        Ok(())
    }

    async fn stop(&self) -> Result<(), TriggerError> {
        // The watcher thread will be cleaned up when the process exits.
        // A more sophisticated implementation would use a cancellation token.
        Ok(())
    }
}

// ── Webhook inbound trigger ─────────────────────────────────────────

/// Configuration for a webhook trigger endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookTriggerConfig {
    pub id: String,
    /// Path suffix for the webhook endpoint (e.g., "/hooks/github").
    pub path: String,
    /// Optional HMAC secret for signature verification.
    pub secret: Option<String>,
    /// Event types to accept (empty = accept all).
    pub accept_events: Vec<String>,
    /// Worker to route events to.
    pub target_worker: String,
}

/// A parsed incoming webhook payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookPayload {
    pub source: String,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub headers: HashMap<String, String>,
    pub received_at: String,
}

impl WebhookPayload {
    /// Extract a human-readable prompt from common webhook formats.
    pub fn to_prompt(&self) -> String {
        // GitHub push event
        if let Some(commits) = self.payload.get("commits") {
            if let Some(arr) = commits.as_array() {
                let messages: Vec<&str> = arr
                    .iter()
                    .filter_map(|c| c.get("message").and_then(|m| m.as_str()))
                    .collect();
                return format!(
                    "Webhook: {} push with {} commits:\n{}",
                    self.source,
                    messages.len(),
                    messages.join("\n")
                );
            }
        }

        // GitHub issue event
        if let Some(issue) = self.payload.get("issue") {
            if let Some(title) = issue.get("title").and_then(|t| t.as_str()) {
                return format!(
                    "Webhook: {} issue '{}': {}",
                    self.source, title, self.event_type
                );
            }
        }

        // Generic fallback
        format!(
            "Webhook event '{}' from {}: {}",
            self.event_type,
            self.source,
            serde_json::to_string(&self.payload)
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect::<String>()
        )
    }
}

// ── Trigger router ──────────────────────────────────────────────────

/// Routes trigger events to the appropriate worker handles.
pub struct TriggerRouter {
    /// Mapping: trigger_id → worker_id.
    routes: HashMap<String, String>,
    /// Available worker handles.
    workers: HashMap<String, WorkerHandle>,
}

impl TriggerRouter {
    pub fn new() -> Self {
        Self {
            routes: HashMap::new(),
            workers: HashMap::new(),
        }
    }

    /// Register a route from a trigger to a worker.
    pub fn add_route(&mut self, trigger_id: impl Into<String>, worker_id: impl Into<String>) {
        self.routes.insert(trigger_id.into(), worker_id.into());
    }

    /// Register a worker handle.
    pub fn add_worker(&mut self, worker_id: impl Into<String>, handle: WorkerHandle) {
        self.workers.insert(worker_id.into(), handle);
    }

    /// Route a trigger event to the appropriate worker.
    pub async fn route(&self, event: TriggerEvent) -> bool {
        if let Some(worker_id) = self.routes.get(&event.trigger_id) {
            if let Some(handle) = self.workers.get(worker_id) {
                let prompt = match &event.kind {
                    TriggerEventKind::FileChanged { path, change_type } => {
                        format!("File {:?}: {}", change_type, path)
                    }
                    TriggerEventKind::WebhookReceived {
                        source,
                        event_type,
                        payload_preview,
                    } => {
                        format!(
                            "Webhook {} from {}: {}",
                            event_type, source, payload_preview
                        )
                    }
                    TriggerEventKind::QueueMatch { task_id, pattern } => {
                        format!("Queue match: task {} matched pattern {}", task_id, pattern)
                    }
                };

                let task_id = uuid::Uuid::new_v4().to_string();
                return handle
                    .send_task(
                        task_id,
                        prompt,
                        event
                            .context
                            .into_iter()
                            .map(|(k, v)| (k, serde_json::Value::String(v)))
                            .collect(),
                    )
                    .await
                    .is_ok();
            }
        }
        false
    }
}

impl Default for TriggerRouter {
    fn default() -> Self {
        Self::new()
    }
}

/// Simple glob matching (supports * and **).
fn glob_match(pattern: &str, path: &str) -> bool {
    // Use globset for proper matching if available,
    // fall back to simple contains check.
    if let Ok(glob) = globset::Glob::new(pattern) {
        let matcher = glob.compile_matcher();
        return matcher.is_match(path);
    }
    // Fallback: simple substring match
    path.contains(pattern.trim_matches('*'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_match() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(glob_match("src/**/*.rs", "src/lib.rs"));
        assert!(!glob_match("*.py", "main.rs"));
    }

    #[test]
    fn test_fs_trigger_filters() {
        let trigger = FileSystemTrigger::new(
            "test",
            vec![PathBuf::from("/project")],
            vec!["*.rs".to_string()],
            vec!["target/**".to_string()],
        );

        assert!(trigger.matches_filters(Path::new("main.rs")));
        assert!(!trigger.matches_filters(Path::new("main.py")));
    }

    #[test]
    fn test_webhook_prompt_github_push() {
        let payload = WebhookPayload {
            source: "github".into(),
            event_type: "push".into(),
            payload: serde_json::json!({
                "commits": [
                    {"message": "fix: resolve race condition"},
                    {"message": "test: add regression test"}
                ]
            }),
            headers: HashMap::new(),
            received_at: "2026-03-30T00:00:00Z".into(),
        };

        let prompt = payload.to_prompt();
        assert!(prompt.contains("2 commits"));
        assert!(prompt.contains("race condition"));
    }
}
