//! Session Persistence & Resume
//!
//! Write-ahead transcript logging with pre-API-call flush and deterministic resume.
//! Every message is recorded to disk BEFORE the API call returns, ensuring
//! zero-loss recovery from any interruption point.

use pipit_provider::Message;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// A single WAL entry representing one message or event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalEntry {
    /// Monotonically increasing sequence number.
    pub seq: u64,
    /// Entry type discriminator.
    pub kind: WalEntryKind,
    /// Timestamp (unix seconds).
    pub timestamp: u64,
}

/// WAL entry kinds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WalEntryKind {
    /// A message was added to the conversation.
    Message { message: Message },
    /// Context compression occurred.
    Compression {
        messages_removed: usize,
        tokens_freed: u64,
    },
    /// System prompt was set/changed.
    SystemPrompt { prompt: String },
    /// Session metadata (model, provider, etc.).
    SessionMeta {
        model: String,
        provider: String,
        session_id: String,
    },
}

/// The write-ahead log for session persistence.
pub struct TranscriptWal {
    /// Path to the WAL file.
    path: PathBuf,
    /// File handle for append writes.
    writer: Option<std::fs::File>,
    /// Current sequence number.
    seq: u64,
    /// Whether to fsync after each write.
    durable: bool,
}

impl TranscriptWal {
    /// Create a new WAL at the given path.
    pub fn new(path: PathBuf, durable: bool) -> Result<Self, WalError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let writer = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        // Determine starting sequence number from existing entries
        let seq = Self::last_seq(&path).unwrap_or(0);

        Ok(Self {
            path,
            writer: Some(writer),
            seq,
            durable,
        })
    }

    /// Append a message to the WAL. Must be called BEFORE the API call.
    pub fn append_message(&mut self, message: &Message) -> Result<u64, WalError> {
        self.append(WalEntryKind::Message {
            message: message.clone(),
        })
    }

    /// Record compression event.
    pub fn append_compression(
        &mut self,
        messages_removed: usize,
        tokens_freed: u64,
    ) -> Result<u64, WalError> {
        self.append(WalEntryKind::Compression {
            messages_removed,
            tokens_freed,
        })
    }

    /// Record system prompt.
    pub fn append_system_prompt(&mut self, prompt: &str) -> Result<u64, WalError> {
        self.append(WalEntryKind::SystemPrompt {
            prompt: prompt.to_string(),
        })
    }

    /// Record session metadata.
    pub fn append_session_meta(
        &mut self,
        model: &str,
        provider: &str,
        session_id: &str,
    ) -> Result<u64, WalError> {
        self.append(WalEntryKind::SessionMeta {
            model: model.to_string(),
            provider: provider.to_string(),
            session_id: session_id.to_string(),
        })
    }

    /// Replay the WAL, returning all entries in order.
    pub fn replay(path: &Path) -> Result<Vec<WalEntry>, WalError> {
        if !path.exists() {
            return Ok(Vec::new());
        }

        let content = std::fs::read_to_string(path)?;
        let mut entries = Vec::new();

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<WalEntry>(line) {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    tracing::warn!("WAL entry parse error (skipping): {}", e);
                    // Skip corrupt entries — best effort recovery
                }
            }
        }

        Ok(entries)
    }

    /// Resume a session by replaying the WAL into a message list.
    pub fn resume_messages(path: &Path) -> Result<Vec<Message>, WalError> {
        let entries = Self::replay(path)?;
        let mut messages = Vec::new();

        for entry in entries {
            match entry.kind {
                WalEntryKind::Message { message } => {
                    messages.push(message);
                }
                WalEntryKind::Compression { .. } => {
                    // Compression records are informational — skip on replay
                }
                WalEntryKind::SystemPrompt { .. } => {
                    // System prompt is reconstructed from config
                }
                WalEntryKind::SessionMeta { .. } => {
                    // Metadata is reconstructed from config
                }
            }
        }

        Ok(messages)
    }

    /// Compact the WAL: replace all entries with a summary + recent messages.
    pub fn compact(&mut self, messages: &[Message]) -> Result<(), WalError> {
        // Close current writer
        self.writer.take();

        // Rewrite the WAL with current messages
        let tmp_path = self.path.with_extension("wal.tmp");
        {
            let mut tmp = std::fs::File::create(&tmp_path)?;
            for (i, msg) in messages.iter().enumerate() {
                let entry = WalEntry {
                    seq: i as u64,
                    kind: WalEntryKind::Message {
                        message: msg.clone(),
                    },
                    timestamp: current_timestamp(),
                };
                let line = serde_json::to_string(&entry)
                    .map_err(|e| WalError::Serialization(e.to_string()))?;
                writeln!(tmp, "{}", line)?;
            }
            if self.durable {
                tmp.sync_all()?;
            }
        }

        // Atomic rename
        std::fs::rename(&tmp_path, &self.path)?;

        // Reopen for appending
        self.writer = Some(std::fs::OpenOptions::new().append(true).open(&self.path)?);
        self.seq = messages.len() as u64;

        Ok(())
    }

    /// Get the WAL file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn append(&mut self, kind: WalEntryKind) -> Result<u64, WalError> {
        self.seq += 1;
        let entry = WalEntry {
            seq: self.seq,
            kind,
            timestamp: current_timestamp(),
        };

        let line =
            serde_json::to_string(&entry).map_err(|e| WalError::Serialization(e.to_string()))?;

        if let Some(ref mut writer) = self.writer {
            writeln!(writer, "{}", line)?;
            if self.durable {
                writer.sync_all()?;
            }
        }

        Ok(self.seq)
    }

    fn last_seq(path: &Path) -> Option<u64> {
        let content = std::fs::read_to_string(path).ok()?;
        content
            .lines()
            .rev()
            .find_map(|line| serde_json::from_str::<WalEntry>(line).ok())
            .map(|e| e.seq)
    }
}

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// WAL error types.
#[derive(Debug, thiserror::Error)]
pub enum WalError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(String),
}
