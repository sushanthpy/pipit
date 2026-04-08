//! Session Replay Protocol
//!
//! Enables SDK consumers to reconstruct conversation state from any
//! resumed session without re-executing tool calls. Historical messages
//! are yielded as `EngineEvent::Replay` before live streaming begins.
//!
//! Replay cost: O(k) where k = WAL entries since last snapshot.

use crate::sdk::EngineEvent;
use pipit_context::transcript::{TranscriptWal, WalEntry, WalEntryKind, WalError};
use pipit_provider::Message;
use std::path::Path;

/// A replay event carrying a historical message and its sequence number.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReplayMessage {
    /// The historical message.
    pub message: Message,
    /// Sequence number in the WAL.
    pub seq: u64,
    /// Whether this was the last replay message before live streaming starts.
    pub is_last: bool,
}

/// Replay a WAL file and produce a sequence of EngineEvents for SDK consumers.
///
/// Returns the replayed messages (for injecting into ContextManager) and
/// the corresponding EngineEvents (for yielding to the SDK consumer).
pub fn replay_session(wal_path: &Path) -> Result<(Vec<Message>, Vec<EngineEvent>), WalError> {
    let entries = TranscriptWal::replay(wal_path)?;
    let mut messages = Vec::new();
    let mut events = Vec::new();

    let total = entries.len();

    for (i, entry) in entries.into_iter().enumerate() {
        let is_last = i == total - 1;

        match entry.kind {
            WalEntryKind::Message { message } => {
                events.push(EngineEvent::Replay {
                    message: message.clone(),
                    seq: entry.seq,
                    is_last,
                });
                messages.push(message);
            }
            WalEntryKind::Compression {
                messages_removed,
                tokens_freed,
            } => {
                events.push(EngineEvent::CompactBoundary {
                    preserved_count: messages_removed,
                    freed_tokens: tokens_freed,
                });
            }
            WalEntryKind::SystemPrompt { .. } | WalEntryKind::SessionMeta { .. } => {
                // Metadata is reconstructed from config, not replayed
            }
        }
    }

    Ok((messages, events))
}

/// Replay only the messages from a WAL (for ContextManager injection).
pub fn replay_messages(wal_path: &Path) -> Result<Vec<Message>, WalError> {
    TranscriptWal::resume_messages(wal_path)
}
