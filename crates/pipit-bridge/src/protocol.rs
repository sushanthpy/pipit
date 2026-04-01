//! Bridge protocol message types.
//!
//! Uses Lamport timestamps for message ordering with eventual consistency.

use serde::{Deserialize, Serialize};

/// Monotonically increasing message ID with Lamport timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MessageId {
    /// Lamport logical clock value.
    pub timestamp: u64,
    /// Sequence number for ordering within same timestamp.
    pub seq: u32,
}

impl MessageId {
    pub fn new(timestamp: u64, seq: u32) -> Self {
        Self { timestamp, seq }
    }

    /// Advance the clock: max(local, received) + 1.
    pub fn advance(&self, received: &MessageId) -> MessageId {
        MessageId {
            timestamp: self.timestamp.max(received.timestamp) + 1,
            seq: 0,
        }
    }
}

/// A bridge message envelope wrapping commands or events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeMessage {
    /// Message identifier for ordering and deduplication.
    pub id: MessageId,
    /// The payload.
    pub payload: BridgePayload,
}

/// Payload discriminator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BridgePayload {
    /// IDE → Agent command.
    Command(BridgeCommand),
    /// Agent → IDE event.
    Event(BridgeEvent),
    /// Heartbeat for liveness detection.
    Heartbeat { timestamp_ms: u64 },
    /// Acknowledgement of a received message.
    Ack { ack_id: MessageId },
}

/// Commands sent from the IDE to the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command")]
pub enum BridgeCommand {
    /// Submit a user message to the agent.
    SubmitMessage {
        text: String,
        attachments: Vec<FileAttachment>,
    },
    /// Approve a pending tool call.
    ApproveToolCall { call_id: String },
    /// Deny a pending tool call.
    DenyToolCall {
        call_id: String,
        reason: Option<String>,
    },
    /// Inject a steering message.
    SteeringMessage { text: String },
    /// Cancel the current operation.
    Cancel,
    /// Request current status.
    GetStatus,
    /// Set approval mode.
    SetApprovalMode { mode: String },
    /// Request file diff for a specific path.
    RequestDiff { path: String },
    /// Sync configuration from IDE.
    SyncConfig { config: serde_json::Value },
}

/// Events sent from the agent to the IDE.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum BridgeEvent {
    /// Agent is streaming content.
    ContentDelta { text: String },
    /// Agent finished a complete response.
    ContentComplete { full_text: String },
    /// Agent is thinking (extended thinking block).
    ThinkingDelta { text: String },
    /// A tool call is starting.
    ToolCallStart {
        call_id: String,
        name: String,
        args: serde_json::Value,
    },
    /// A tool call completed.
    ToolCallEnd {
        call_id: String,
        name: String,
        success: bool,
        result_summary: String,
    },
    /// A tool call needs approval.
    ApprovalNeeded {
        call_id: String,
        name: String,
        args: serde_json::Value,
        description: String,
    },
    /// File was modified — IDE should show inline diff.
    FileModified {
        path: String,
        diff: String,
        before_content: Option<String>,
        after_content: Option<String>,
    },
    /// Agent status update.
    StatusUpdate {
        phase: String,
        message: String,
        tokens_used: u64,
        tokens_limit: u64,
        cost: f64,
    },
    /// Context compression occurred.
    ContextCompressed {
        messages_removed: usize,
        tokens_freed: u64,
    },
    /// Error occurred.
    Error {
        message: String,
        recoverable: bool,
    },
    /// Agent finished processing.
    Done {
        turns: u32,
        total_tokens: u64,
        cost: f64,
    },
    /// Session state for reconnection.
    SessionState {
        session_id: String,
        turn_count: u32,
        active: bool,
    },
}

/// File attachment from IDE.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAttachment {
    pub path: String,
    pub content: Option<String>,
    pub kind: AttachmentKind,
}

/// Attachment type discriminator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AttachmentKind {
    File,
    Image,
    Selection { start_line: u32, end_line: u32 },
}
