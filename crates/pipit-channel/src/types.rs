use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

// ---------------------------------------------------------------------------
// Channel identity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelId {
    Telegram,
    Discord,
    Slack,
    Webhook,
    Api,
    Cron,
    Cli,
}

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Telegram => write!(f, "telegram"),
            Self::Discord => write!(f, "discord"),
            Self::Slack => write!(f, "slack"),
            Self::Webhook => write!(f, "webhook"),
            Self::Api => write!(f, "api"),
            Self::Cron => write!(f, "cron"),
            Self::Cli => write!(f, "cli"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChannelMeta {
    pub display_name: String,
    pub supports_streaming: bool,
    pub supports_threads: bool,
    pub supports_reactions: bool,
    pub max_message_length: Option<usize>,
}

// ---------------------------------------------------------------------------
// Message origin — tagged enum for channel-specific routing
// ---------------------------------------------------------------------------

/// Return-address for task updates. Pattern-matched by the reporter to
/// format channel-native messages and route replies back to the correct
/// conversation/thread/chat.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "channel", rename_all = "snake_case")]
pub enum MessageOrigin {
    Telegram {
        chat_id: i64,
        message_id: Option<i64>,
        /// The message ID of the progress message being edited in-place.
        progress_message_id: Option<i64>,
        user_id: Option<i64>,
    },
    Discord {
        guild_id: Option<u64>,
        channel_id: u64,
        message_id: Option<u64>,
        thread_id: Option<u64>,
    },
    Slack {
        team_id: String,
        channel_id: String,
        thread_ts: Option<String>,
    },
    Webhook {
        callback_url: Option<String>,
        request_id: String,
    },
    Api {
        client_id: Option<String>,
    },
    Cron {
        schedule_name: String,
        /// Channel to deliver results to (e.g., a Telegram chat).
        notification_origin: Option<Box<MessageOrigin>>,
    },
    Cli,
}

impl MessageOrigin {
    pub fn channel_id(&self) -> ChannelId {
        match self {
            Self::Telegram { .. } => ChannelId::Telegram,
            Self::Discord { .. } => ChannelId::Discord,
            Self::Slack { .. } => ChannelId::Slack,
            Self::Webhook { .. } => ChannelId::Webhook,
            Self::Api { .. } => ChannelId::Api,
            Self::Cron { .. } => ChannelId::Cron,
            Self::Cli => ChannelId::Cli,
        }
    }
}

// ---------------------------------------------------------------------------
// Normalized task — the single inbound type consumed by the queue
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    Low = 0,
    Normal = 1,
    High = 2,
}

impl Default for TaskPriority {
    fn default() -> Self {
        Self::Normal
    }
}

/// A task as seen by the queue and agent pool. Channel adapters normalize
/// their channel-specific message format into this.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedTask {
    pub task_id: String,
    pub project: String,
    pub prompt: String,
    pub priority: TaskPriority,
    pub origin: MessageOrigin,
    pub submitted_at: DateTime<Utc>,
    pub sender_name: Option<String>,
    /// Optional files to include in context.
    pub attached_files: Vec<String>,
}

impl NormalizedTask {
    pub fn new(project: String, prompt: String, origin: MessageOrigin) -> Self {
        Self {
            task_id: uuid::Uuid::new_v4().to_string(),
            project,
            prompt,
            priority: TaskPriority::default(),
            origin,
            submitted_at: Utc::now(),
            sender_name: None,
            attached_files: Vec::new(),
        }
    }

    pub fn with_priority(mut self, priority: TaskPriority) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_sender(mut self, name: String) -> Self {
        self.sender_name = Some(name);
        self
    }
}

// ---------------------------------------------------------------------------
// Task lifecycle
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Persistent task record stored in SochDB at `tasks/{task_id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: String,
    pub project: String,
    pub prompt: String,
    pub priority: TaskPriority,
    pub status: TaskStatus,
    pub origin: MessageOrigin,
    pub submitted_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub turns: Option<u32>,
    pub total_tokens: Option<u64>,
    pub cost: Option<f64>,
    pub result_summary: Option<String>,
    pub error: Option<String>,
    pub files_modified: Vec<String>,
    pub branch: Option<String>,
    pub sender_name: Option<String>,
}

impl TaskRecord {
    pub fn from_task(task: &NormalizedTask) -> Self {
        Self {
            task_id: task.task_id.clone(),
            project: task.project.clone(),
            prompt: task.prompt.clone(),
            priority: task.priority,
            status: TaskStatus::Queued,
            origin: task.origin.clone(),
            submitted_at: task.submitted_at,
            started_at: None,
            completed_at: None,
            turns: None,
            total_tokens: None,
            cost: None,
            result_summary: None,
            error: None,
            files_modified: Vec::new(),
            branch: None,
            sender_name: task.sender_name.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Task updates — outbound from reporter to channel
// ---------------------------------------------------------------------------

/// Fine-grained status updates sent to channels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskUpdate {
    pub task_id: String,
    pub origin: MessageOrigin,
    pub kind: TaskUpdateKind,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskUpdateKind {
    Started {
        project: String,
        model: String,
    },
    Progress {
        /// Formatted progress text (channel-aware).
        text: String,
        /// Tool calls executed so far in this batch.
        tool_log: Vec<String>,
    },
    ToolStarted {
        name: String,
        args_preview: Option<String>,
    },
    ToolCompleted {
        name: String,
        success: bool,
        duration_ms: u64,
    },
    Error {
        message: String,
    },
    Completed {
        summary: String,
        turns: u32,
        cost: f64,
        files_modified: Vec<String>,
    },
    Cancelled,
}

impl TaskUpdate {
    pub fn new(task_id: String, origin: MessageOrigin, kind: TaskUpdateKind) -> Self {
        Self {
            task_id,
            origin,
            kind,
            timestamp: Utc::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// Stream handle for edit-in-place delivery
// ---------------------------------------------------------------------------

/// Handle returned by `StreamingChannel::send_streaming`. The `update`
/// method edits the original message in-place.
pub struct StreamHandle {
    /// Closure that performs the edit (e.g., Telegram's editMessageText).
    updater: Box<
        dyn Fn(String) -> futures::future::BoxFuture<'static, Result<(), ChannelError>>
            + Send
            + Sync,
    >,
}

impl StreamHandle {
    pub fn new<F, Fut>(f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), ChannelError>> + Send + 'static,
    {
        Self {
            updater: Box::new(move |text| Box::pin(f(text))),
        }
    }

    pub async fn update(&self, text: String) -> Result<(), ChannelError> {
        (self.updater)(text).await
    }
}

impl fmt::Debug for StreamHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamHandle").finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Task sink — how channels submit tasks to the queue
// ---------------------------------------------------------------------------

/// Sender half of the task submission channel. Cloned into each channel adapter.
pub type TaskSink = tokio::sync::mpsc::Sender<NormalizedTask>;

/// Receiver half consumed by the queue.
pub type TaskReceiver = tokio::sync::mpsc::Receiver<NormalizedTask>;

/// Create a bounded task submission channel.
pub fn task_channel(capacity: usize) -> (TaskSink, TaskReceiver) {
    tokio::sync::mpsc::channel(capacity)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("channel not connected: {0}")]
    NotConnected(String),

    #[error("rate limited: retry after {retry_after_ms}ms")]
    RateLimited { retry_after_ms: u64 },

    #[error("message too long: {len} > {max}")]
    MessageTooLong { len: usize, max: usize },

    #[error("authentication failed: {0}")]
    AuthFailed(String),

    #[error("network error: {0}")]
    Network(String),

    #[error("channel error: {0}")]
    Other(String),
}
