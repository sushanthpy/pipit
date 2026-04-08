//! Debounced reporter with channel-native formatting.
//!
//! Batches rapid tool events with 800ms debounce. Caps tool log at 6 lines.
//! Error events bypass debounce. Event audit trail persisted to SochDB.

use crate::store::DaemonStore;

use anyhow::Result;
use chrono::Utc;
use pipit_channel::{ChannelId, ChannelRegistry, MessageOrigin, TaskUpdate, TaskUpdateKind};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing;

const DEBOUNCE_MS: u64 = 800;
const MAX_TOOL_LOG_LINES: usize = 6;

// ---------------------------------------------------------------------------
// Reporter
// ---------------------------------------------------------------------------

pub struct Reporter {
    store: Arc<DaemonStore>,
    /// Channel registry for delivering updates to originating channels.
    registry: Arc<ChannelRegistry>,
    /// Active task progress states, keyed by task_id.
    progress: Mutex<HashMap<String, TaskProgress>>,
    /// Broadcast channel for SSE/WebSocket subscribers.
    broadcast_tx: tokio::sync::broadcast::Sender<TaskUpdate>,
}

/// Per-task progress accumulator.
struct TaskProgress {
    tool_log: Vec<String>,
    last_flush: std::time::Instant,
    origin: MessageOrigin,
}

impl Reporter {
    pub fn new(store: Arc<DaemonStore>, registry: Arc<ChannelRegistry>) -> Self {
        let (broadcast_tx, _) = tokio::sync::broadcast::channel(256);
        Self {
            store,
            registry,
            progress: Mutex::new(HashMap::new()),
            broadcast_tx,
        }
    }

    /// Subscribe to the broadcast channel for real-time event streaming.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<TaskUpdate> {
        self.broadcast_tx.subscribe()
    }

    /// Handle a task update: persist to store + format for channel delivery.
    pub async fn handle_update(&self, update: TaskUpdate) -> Result<()> {
        // Persist event to store (group-commit, not per-event durable)
        let _ = self.store.append_event(&update.task_id, &update.kind);

        // Track whether this update should be delivered to the channel
        let mut deliver_to_channel = false;

        match &update.kind {
            TaskUpdateKind::Started { project, model } => {
                // Initialize progress tracker
                let mut progress = self.progress.lock().await;
                progress.insert(
                    update.task_id.clone(),
                    TaskProgress {
                        tool_log: Vec::new(),
                        last_flush: std::time::Instant::now(),
                        origin: update.origin.clone(),
                    },
                );

                let text = format_started(&update.origin, project, model);
                tracing::info!(task_id = %update.task_id, project, "{}", text);
                deliver_to_channel = true;
            }

            TaskUpdateKind::ToolStarted { name, args_preview } => {
                let mut progress = self.progress.lock().await;
                if let Some(state) = progress.get_mut(&update.task_id) {
                    let line = format_tool_start(&update.origin, name, args_preview.as_deref());
                    state.tool_log.push(line);

                    // Cap at MAX_TOOL_LOG_LINES (evict oldest)
                    while state.tool_log.len() > MAX_TOOL_LOG_LINES {
                        state.tool_log.remove(0);
                    }

                    // Check debounce — flush if interval elapsed
                    if state.last_flush.elapsed() >= std::time::Duration::from_millis(DEBOUNCE_MS) {
                        let text = state.tool_log.join("\n");
                        state.last_flush = std::time::Instant::now();
                        tracing::debug!(task_id = %update.task_id, "progress: {}", text);
                        deliver_to_channel = true;
                    }
                }
            }

            TaskUpdateKind::ToolCompleted {
                name,
                success,
                duration_ms,
            } => {
                let mut progress = self.progress.lock().await;
                if let Some(state) = progress.get_mut(&update.task_id) {
                    let line = format_tool_complete(&update.origin, name, *success, *duration_ms);
                    state.tool_log.push(line);
                    while state.tool_log.len() > MAX_TOOL_LOG_LINES {
                        state.tool_log.remove(0);
                    }
                }
            }

            TaskUpdateKind::Error { message } => {
                // Errors bypass debounce — deliver immediately
                tracing::error!(task_id = %update.task_id, "error: {}", message);
                deliver_to_channel = true;

                // Clean up progress state
                let mut progress = self.progress.lock().await;
                progress.remove(&update.task_id);
            }

            TaskUpdateKind::Completed {
                summary,
                turns,
                cost,
                files_modified,
            } => {
                let text = format_completed(&update.origin, summary, *turns, *cost, files_modified);
                tracing::info!(task_id = %update.task_id, "{}", text);

                // Use put_durable for terminal events
                let _ = self.store.append_event(&update.task_id, &update.kind);
                deliver_to_channel = true;

                // Clean up progress state
                let mut progress = self.progress.lock().await;
                progress.remove(&update.task_id);
            }

            TaskUpdateKind::Cancelled => {
                tracing::info!(task_id = %update.task_id, "task cancelled");
                deliver_to_channel = true;
                let mut progress = self.progress.lock().await;
                progress.remove(&update.task_id);
            }

            TaskUpdateKind::Progress { text, tool_log } => {
                tracing::debug!(task_id = %update.task_id, "{}", text);
                deliver_to_channel = true;
            }
        }

        // Broadcast to SSE/WebSocket subscribers (best-effort)
        let _ = self.broadcast_tx.send(update.clone());

        // Deliver to originating channel
        if deliver_to_channel {
            let channel_id = update.origin.channel_id();
            if let Some(channel) = self.registry.get(&channel_id) {
                if let Err(e) = channel.send_update(update).await {
                    tracing::warn!(
                        channel = %channel_id,
                        error = %e,
                        "failed to deliver update to channel"
                    );
                }
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Channel-native formatting
// ---------------------------------------------------------------------------

fn format_started(origin: &MessageOrigin, project: &str, model: &str) -> String {
    match origin {
        MessageOrigin::Telegram { .. } => {
            format!(
                "▸ *Working on {}*\n_Model: {}_",
                escape_md(project),
                escape_md(model)
            )
        }
        MessageOrigin::Discord { .. } => {
            format!("**Working on {}**\nModel: `{}`", project, model)
        }
        _ => format!("Working on {} (model: {})", project, model),
    }
}

fn format_tool_start(origin: &MessageOrigin, name: &str, args_preview: Option<&str>) -> String {
    let preview = args_preview.unwrap_or("");
    match origin {
        MessageOrigin::Telegram { .. } => {
            if preview.is_empty() {
                format!("○ {}", escape_md(name))
            } else {
                format!("○ {} `{}`", escape_md(name), escape_md(preview))
            }
        }
        MessageOrigin::Discord { .. } => {
            if preview.is_empty() {
                format!("○ {}", name)
            } else {
                format!("○ {} `{}`", name, preview)
            }
        }
        _ => {
            if preview.is_empty() {
                format!("  ○ {}", name)
            } else {
                format!("  ○ {} {}", name, preview)
            }
        }
    }
}

fn format_tool_complete(
    origin: &MessageOrigin,
    name: &str,
    success: bool,
    duration_ms: u64,
) -> String {
    let icon = if success { "●" } else { "✗" };
    match origin {
        MessageOrigin::Telegram { .. } => {
            format!("{} {} _{}ms_", icon, escape_md(name), duration_ms)
        }
        _ => format!("{} {} ({}ms)", icon, name, duration_ms),
    }
}

fn format_completed(
    origin: &MessageOrigin,
    summary: &str,
    turns: u32,
    cost: f64,
    files: &[String],
) -> String {
    let files_str = if files.is_empty() {
        String::new()
    } else {
        let file_list: Vec<&str> = files.iter().map(|s| s.as_str()).collect();
        format!("\nFiles: {}", file_list.join(", "))
    };

    match origin {
        MessageOrigin::Telegram { .. } => {
            format!(
                "✓ *Done* ({} turns, ${:.4})\n{}{}",
                turns,
                cost,
                escape_md(summary),
                files_str
            )
        }
        MessageOrigin::Discord { .. } => {
            format!(
                "✓ **Done** ({} turns, ${:.4})\n{}{}",
                turns, cost, summary, files_str
            )
        }
        _ => format!(
            "Done ({} turns, ${:.4}): {}{}",
            turns, cost, summary, files_str
        ),
    }
}

/// Escape special characters for Telegram MarkdownV2.
fn escape_md(s: &str) -> String {
    let specials = [
        '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
    ];
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        if specials.contains(&c) {
            result.push('\\');
        }
        result.push(c);
    }
    result
}
