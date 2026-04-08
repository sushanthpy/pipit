//! Pipit Channel Abstraction Layer
//!
//! Hexagonal ports-and-adapters architecture for message ingestion.
//! Each channel adapter normalizes inbound messages to `NormalizedTask`
//! and receives outbound updates via `TaskUpdate`.

mod types;

pub use types::*;

use async_trait::async_trait;
use std::any::Any;

// ---------------------------------------------------------------------------
// Layer 0 — Required Channel trait
// ---------------------------------------------------------------------------

/// Core channel interface. Every message source (Telegram, Discord, HTTP API,
/// webhook, cron) implements this trait. The `start` method receives a
/// `TaskSink` that channels push normalized tasks into.
#[async_trait]
pub trait Channel: Send + Sync + 'static {
    /// Unique identifier for this channel instance.
    fn id(&self) -> ChannelId;

    /// Human-readable metadata.
    fn meta(&self) -> ChannelMeta;

    /// Start the channel. The sink receives normalized tasks.
    async fn start(&self, sink: TaskSink) -> Result<(), ChannelError>;

    /// Send a task update to the originating channel.
    async fn send_update(&self, update: TaskUpdate) -> Result<(), ChannelError>;

    /// Gracefully stop the channel.
    async fn stop(&self) -> Result<(), ChannelError>;

    /// Downcast helper for capability probing.
    fn as_any(&self) -> &dyn Any;

    /// Default origin for proactive messages (e.g., cron results).
    fn default_origin(&self) -> Option<MessageOrigin> {
        None
    }
}

// ---------------------------------------------------------------------------
// Layer 1 — Optional capability traits
// ---------------------------------------------------------------------------

/// Channels that support edit-in-place streaming (e.g., Telegram editMessageText).
#[async_trait]
pub trait StreamingChannel: Channel {
    /// Send an initial progress message and return a handle for editing it.
    async fn send_streaming(
        &self,
        origin: &MessageOrigin,
        initial: &str,
    ) -> Result<StreamHandle, ChannelError>;
}

/// Channels that support thread-per-task grouping (e.g., Discord threads).
#[async_trait]
pub trait ThreadedChannel: Channel {
    async fn create_thread(
        &self,
        origin: &MessageOrigin,
        title: &str,
    ) -> Result<MessageOrigin, ChannelError>;
}

/// Channels that support emoji reactions for lightweight controls.
#[async_trait]
pub trait ReactiveChannel: Channel {
    async fn add_reaction(&self, origin: &MessageOrigin, emoji: &str) -> Result<(), ChannelError>;

    async fn remove_reaction(
        &self,
        origin: &MessageOrigin,
        emoji: &str,
    ) -> Result<(), ChannelError>;
}

// ---------------------------------------------------------------------------
// Channel registry
// ---------------------------------------------------------------------------

/// Registry of active channel instances, keyed by ChannelId.
/// Thread-safe for concurrent reads and dynamic registration.
pub struct ChannelRegistry {
    channels: dashmap::DashMap<ChannelId, std::sync::Arc<dyn Channel>>,
}

impl ChannelRegistry {
    pub fn new() -> Self {
        Self {
            channels: dashmap::DashMap::new(),
        }
    }

    /// Register a channel. Safe to call from any thread.
    pub fn register(&self, channel: std::sync::Arc<dyn Channel>) {
        self.channels.insert(channel.id(), channel);
    }

    /// Look up a channel by ID. Returns an Arc clone.
    pub fn get(&self, id: &ChannelId) -> Option<std::sync::Arc<dyn Channel>> {
        self.channels.get(id).map(|entry| entry.value().clone())
    }

    /// Iterate over all registered channels.
    pub fn all(&self) -> Vec<std::sync::Arc<dyn Channel>> {
        self.channels
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// List all registered channel IDs.
    pub fn ids(&self) -> Vec<ChannelId> {
        self.channels.iter().map(|entry| *entry.key()).collect()
    }
}

impl Default for ChannelRegistry {
    fn default() -> Self {
        Self::new()
    }
}
