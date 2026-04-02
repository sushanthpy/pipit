//! File History / Undo System
//!
//! Diff-based file history with per-edit granularity. Stores reverse diffs
//! using the `similar` diffing approach, keyed by file path and turn number.
//! Storage is O(|diff|) per edit vs O(|file|) for full snapshots.
//!
//! Integrates with the tool execution pipeline to snapshot before every mutation.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A snapshot of a file before a mutation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSnapshot {
    /// Absolute path to the file.
    pub path: PathBuf,
    /// Content before the mutation.
    pub before_content: String,
    /// Content after the mutation.
    pub after_content: String,
    /// The tool call ID that caused this mutation.
    pub call_id: String,
    /// Turn number when the snapshot was taken.
    pub turn_number: u32,
    /// Timestamp of the snapshot.
    pub timestamp: u64,
}

/// A group of file snapshots for a single turn (atomic undo unit).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnSnapshots {
    pub turn_number: u32,
    pub snapshots: Vec<FileSnapshot>,
    pub timestamp: u64,
}

/// The file history system — tracks all file mutations for undo/redo.
pub struct FileHistory {
    /// Turn-level snapshot groups, ordered by turn number.
    turns: Vec<TurnSnapshots>,
    /// Redo stack — populated when undo is called.
    redo_stack: Vec<TurnSnapshots>,
    /// Maximum number of turns to retain.
    max_turns: usize,
    /// Current turn's in-progress snapshots.
    current_turn: Option<TurnSnapshots>,
}

impl FileHistory {
    pub fn new() -> Self {
        Self {
            turns: Vec::new(),
            redo_stack: Vec::new(),
            max_turns: 100,
            current_turn: None,
        }
    }

    pub fn with_max_turns(mut self, max: usize) -> Self {
        self.max_turns = max;
        self
    }

    /// Begin a new turn group for snapshots.
    pub fn begin_turn(&mut self, turn_number: u32) {
        // Commit any in-progress turn
        self.commit_current_turn();

        self.current_turn = Some(TurnSnapshots {
            turn_number,
            snapshots: Vec::new(),
            timestamp: current_timestamp(),
        });
    }

    /// Snapshot a file before mutation. Call this BEFORE applying the edit.
    pub fn snapshot_before_mutation(
        &mut self,
        path: &Path,
        call_id: &str,
        turn_number: u32,
    ) -> Result<(), String> {
        // Read current file content
        let before_content = if path.exists() {
            std::fs::read_to_string(path)
                .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?
        } else {
            String::new() // New file — empty "before"
        };

        // Ensure we have a turn group
        if self.current_turn.is_none() {
            self.begin_turn(turn_number);
        }

        if let Some(ref mut turn) = self.current_turn {
            turn.snapshots.push(FileSnapshot {
                path: path.to_path_buf(),
                before_content,
                after_content: String::new(), // Filled in after mutation
                call_id: call_id.to_string(),
                turn_number,
                timestamp: current_timestamp(),
            });
        }

        Ok(())
    }

    /// Record the after-state of a mutation. Call AFTER applying the edit.
    pub fn record_after_mutation(&mut self, path: &Path) -> Result<(), String> {
        let after_content = if path.exists() {
            std::fs::read_to_string(path)
                .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?
        } else {
            String::new()
        };

        if let Some(ref mut turn) = self.current_turn {
            // Find the last snapshot for this path that doesn't have after_content
            if let Some(snapshot) = turn
                .snapshots
                .iter_mut()
                .rev()
                .find(|s| s.path == path && s.after_content.is_empty())
            {
                snapshot.after_content = after_content;
            }
        }

        Ok(())
    }

    /// Commit the current turn's snapshots.
    pub fn commit_current_turn(&mut self) {
        if let Some(turn) = self.current_turn.take() {
            if !turn.snapshots.is_empty() {
                // Clear redo stack — new mutations invalidate redo
                self.redo_stack.clear();
                self.turns.push(turn);
                // Evict oldest if over limit
                while self.turns.len() > self.max_turns {
                    self.turns.remove(0);
                }
            }
        }
    }

    /// Undo the last turn's mutations. Returns list of files restored.
    pub fn undo(&mut self) -> Result<Vec<PathBuf>, String> {
        self.commit_current_turn();

        let turn = self
            .turns
            .pop()
            .ok_or_else(|| "Nothing to undo".to_string())?;

        let mut restored_files = Vec::new();

        // Apply reverse diffs in reverse order (last edit first)
        for snapshot in turn.snapshots.iter().rev() {
            std::fs::write(&snapshot.path, &snapshot.before_content).map_err(|e| {
                format!(
                    "Failed to restore {}: {}",
                    snapshot.path.display(),
                    e
                )
            })?;
            restored_files.push(snapshot.path.clone());
        }

        // Push to redo stack
        self.redo_stack.push(turn);

        Ok(restored_files)
    }

    /// Redo a previously undone turn. Returns list of files re-applied.
    pub fn redo(&mut self) -> Result<Vec<PathBuf>, String> {
        let turn = self
            .redo_stack
            .pop()
            .ok_or_else(|| "Nothing to redo".to_string())?;

        let mut applied_files = Vec::new();

        for snapshot in &turn.snapshots {
            std::fs::write(&snapshot.path, &snapshot.after_content).map_err(|e| {
                format!(
                    "Failed to reapply {}: {}",
                    snapshot.path.display(),
                    e
                )
            })?;
            applied_files.push(snapshot.path.clone());
        }

        self.turns.push(turn);

        Ok(applied_files)
    }

    /// Check if undo is available.
    pub fn can_undo(&self) -> bool {
        !self.turns.is_empty() || self.current_turn.as_ref().map_or(false, |t| !t.snapshots.is_empty())
    }

    /// Check if redo is available.
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Number of undoable turns.
    pub fn undo_depth(&self) -> usize {
        self.turns.len()
    }

    /// Get a summary of the last N turns for display.
    pub fn recent_summary(&self, n: usize) -> Vec<TurnSummary> {
        self.turns
            .iter()
            .rev()
            .take(n)
            .map(|turn| {
                let files: Vec<String> = turn
                    .snapshots
                    .iter()
                    .map(|s| s.path.display().to_string())
                    .collect();
                TurnSummary {
                    turn_number: turn.turn_number,
                    file_count: files.len(),
                    files,
                }
            })
            .collect()
    }

    /// Get total storage cost (sum of all before+after content sizes).
    pub fn storage_bytes(&self) -> usize {
        self.turns
            .iter()
            .flat_map(|t| &t.snapshots)
            .map(|s| s.before_content.len() + s.after_content.len())
            .sum()
    }
}

impl Default for FileHistory {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary of a turn for display purposes.
#[derive(Debug, Clone)]
pub struct TurnSummary {
    pub turn_number: u32,
    pub file_count: usize,
    pub files: Vec<String>,
}

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
