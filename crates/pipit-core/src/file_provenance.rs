//! Session-Scoped File Provenance
//!
//! Tracks which files the agent read, wrote, or edited across the active
//! session. Answers "what did the agent touch in this session?" independently
//! of Git state.
//!
//! Incremental maintenance: update a `HashMap<Path, FileActivity>` on each
//! tool event for amortized O(1) per file-touch event. Render O(F log F)
//! when sorting F touched files by timestamp.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// How a file was touched during the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileAction {
    Read,
    Write,
    Edit,
    Create,
    Delete,
}

impl std::fmt::Display for FileAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read => write!(f, "read"),
            Self::Write => write!(f, "write"),
            Self::Edit => write!(f, "edit"),
            Self::Create => write!(f, "create"),
            Self::Delete => write!(f, "delete"),
        }
    }
}

/// Activity record for a single file within a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileActivity {
    /// Absolute path to the file.
    pub path: PathBuf,
    /// All actions performed on this file, in order.
    pub actions: Vec<FileActionRecord>,
    /// Whether any action was mutating (write/edit/create/delete).
    pub mutated: bool,
    /// Timestamp of the first touch (monotonic, session-relative).
    #[serde(skip)]
    pub first_touch: Option<Instant>,
    /// Timestamp of the most recent touch.
    #[serde(skip)]
    pub last_touch: Option<Instant>,
    /// Epoch millis of first touch (serializable).
    pub first_touch_epoch_ms: u64,
    /// Epoch millis of last touch (serializable).
    pub last_touch_epoch_ms: u64,
}

/// A single action on a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileActionRecord {
    pub action: FileAction,
    pub tool_call_id: String,
    pub tool_name: String,
    pub turn_number: u32,
    pub epoch_ms: u64,
}

/// Session-scoped file provenance tracker.
///
/// Amortized O(1) per `record()` call via HashMap.
/// O(F log F) for sorted retrieval of F touched files.
pub struct FileProvenance {
    files: HashMap<PathBuf, FileActivity>,
    session_start: Instant,
}

impl FileProvenance {
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
            session_start: Instant::now(),
        }
    }

    /// Record a file touch. Called on each relevant tool event.
    pub fn record(
        &mut self,
        path: &Path,
        action: FileAction,
        tool_call_id: &str,
        tool_name: &str,
        turn_number: u32,
    ) {
        let now = Instant::now();
        let epoch_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let is_mutating = matches!(
            action,
            FileAction::Write | FileAction::Edit | FileAction::Create | FileAction::Delete
        );

        let record = FileActionRecord {
            action,
            tool_call_id: tool_call_id.to_string(),
            tool_name: tool_name.to_string(),
            turn_number,
            epoch_ms,
        };

        let activity = self
            .files
            .entry(path.to_path_buf())
            .or_insert_with(|| FileActivity {
                path: path.to_path_buf(),
                actions: Vec::new(),
                mutated: false,
                first_touch: Some(now),
                last_touch: Some(now),
                first_touch_epoch_ms: epoch_ms,
                last_touch_epoch_ms: epoch_ms,
            });

        activity.actions.push(record);
        activity.last_touch = Some(now);
        activity.last_touch_epoch_ms = epoch_ms;
        if is_mutating {
            activity.mutated = true;
        }
    }

    /// Get all touched files sorted by most recent activity (descending).
    /// Complexity: O(F log F) over F touched files.
    pub fn touched_files_by_recency(&self) -> Vec<&FileActivity> {
        let mut files: Vec<&FileActivity> = self.files.values().collect();
        files.sort_by(|a, b| b.last_touch_epoch_ms.cmp(&a.last_touch_epoch_ms));
        files
    }

    /// Get only mutated files sorted by most recent activity.
    pub fn mutated_files_by_recency(&self) -> Vec<&FileActivity> {
        let mut files: Vec<&FileActivity> = self.files.values().filter(|f| f.mutated).collect();
        files.sort_by(|a, b| b.last_touch_epoch_ms.cmp(&a.last_touch_epoch_ms));
        files
    }

    /// Get only read (non-mutated) files sorted by most recent activity.
    pub fn read_only_files(&self) -> Vec<&FileActivity> {
        let mut files: Vec<&FileActivity> = self.files.values().filter(|f| !f.mutated).collect();
        files.sort_by(|a, b| b.last_touch_epoch_ms.cmp(&a.last_touch_epoch_ms));
        files
    }

    /// Get activity for a specific file.
    pub fn file_activity(&self, path: &Path) -> Option<&FileActivity> {
        self.files.get(path)
    }

    /// Total number of unique files touched.
    pub fn total_files(&self) -> usize {
        self.files.len()
    }

    /// Total number of unique files mutated.
    pub fn total_mutated(&self) -> usize {
        self.files.values().filter(|f| f.mutated).count()
    }

    /// Total number of file-touch events recorded.
    pub fn total_events(&self) -> usize {
        self.files.values().map(|f| f.actions.len()).sum()
    }

    /// Get a summary suitable for display.
    pub fn summary(&self) -> ProvenanceSummary {
        let all = self.touched_files_by_recency();
        let mutated_count = all.iter().filter(|f| f.mutated).count();
        let read_count = all.len() - mutated_count;

        ProvenanceSummary {
            total_files: all.len(),
            mutated_files: mutated_count,
            read_only_files: read_count,
            total_events: self.total_events(),
            files: all
                .iter()
                .map(|f| FileSummaryEntry {
                    path: f.path.clone(),
                    mutated: f.mutated,
                    action_count: f.actions.len(),
                    last_action: f.actions.last().map(|a| a.action),
                    last_touch_epoch_ms: f.last_touch_epoch_ms,
                })
                .collect(),
        }
    }

    /// Clear all provenance data.
    pub fn clear(&mut self) {
        self.files.clear();
    }
}

impl Default for FileProvenance {
    fn default() -> Self {
        Self::new()
    }
}

/// Serializable summary of session file provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceSummary {
    pub total_files: usize,
    pub mutated_files: usize,
    pub read_only_files: usize,
    pub total_events: usize,
    pub files: Vec<FileSummaryEntry>,
}

/// Summary entry for a single file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSummaryEntry {
    pub path: PathBuf,
    pub mutated: bool,
    pub action_count: usize,
    pub last_action: Option<FileAction>,
    pub last_touch_epoch_ms: u64,
}

/// Extract file path and action from a tool call for provenance tracking.
///
/// Returns `Some((path, action))` if the tool call touches a file.
pub fn extract_file_touch(
    tool_name: &str,
    args: &serde_json::Value,
    success: bool,
) -> Option<(PathBuf, FileAction)> {
    if !success {
        return None;
    }

    let action = match tool_name {
        "read_file" => FileAction::Read,
        "write_file" => {
            // Check if file existed before (heuristic: if args has "create" flag)
            FileAction::Write
        }
        "edit_file" | "multi_edit" => FileAction::Edit,
        "create_file" => FileAction::Create,
        _ => return None,
    };

    // Extract path from args
    let path_str = args
        .get("path")
        .or_else(|| args.get("file_path"))
        .or_else(|| args.get("filePath"))
        .and_then(|v| v.as_str())?;

    Some((PathBuf::from(path_str), action))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_query() {
        let mut prov = FileProvenance::new();
        prov.record(
            Path::new("/src/main.rs"),
            FileAction::Read,
            "call-1",
            "read_file",
            1,
        );
        prov.record(
            Path::new("/src/main.rs"),
            FileAction::Edit,
            "call-2",
            "edit_file",
            1,
        );
        prov.record(
            Path::new("/src/lib.rs"),
            FileAction::Read,
            "call-3",
            "read_file",
            2,
        );

        assert_eq!(prov.total_files(), 2);
        assert_eq!(prov.total_mutated(), 1);
        assert_eq!(prov.total_events(), 3);

        let mutated = prov.mutated_files_by_recency();
        assert_eq!(mutated.len(), 1);
        assert_eq!(mutated[0].path, Path::new("/src/main.rs"));
        assert_eq!(mutated[0].actions.len(), 2);
    }

    #[test]
    fn extract_file_touch_from_tool() {
        let args = serde_json::json!({"path": "/foo/bar.rs"});
        let result = extract_file_touch("read_file", &args, true);
        assert_eq!(
            result,
            Some((PathBuf::from("/foo/bar.rs"), FileAction::Read))
        );

        let result = extract_file_touch("edit_file", &args, true);
        assert_eq!(
            result,
            Some((PathBuf::from("/foo/bar.rs"), FileAction::Edit))
        );

        // Failed tool call should not be tracked
        assert!(extract_file_touch("read_file", &args, false).is_none());

        // Unknown tool
        assert!(extract_file_touch("bash", &args, true).is_none());
    }

    #[test]
    fn summary_generation() {
        let mut prov = FileProvenance::new();
        prov.record(Path::new("/a.rs"), FileAction::Read, "c1", "read_file", 1);
        prov.record(Path::new("/b.rs"), FileAction::Edit, "c2", "edit_file", 2);

        let summary = prov.summary();
        assert_eq!(summary.total_files, 2);
        assert_eq!(summary.mutated_files, 1);
        assert_eq!(summary.read_only_files, 1);
    }
}
