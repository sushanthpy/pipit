//! Agent Mesh Partition Tolerance
//!
//! Partition-aware merge with conflict detection using version vectors.
//! Concurrent edits are detected when version vectors are incomparable
//! (neither dominates). For file edits, conflict resolution uses
//! three-way merge via the last common ancestor.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// A version vector tracking causal order across agents.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct VersionVector {
    /// agent_id → logical_clock
    clocks: BTreeMap<String, u64>,
}

impl VersionVector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment the clock for the given agent.
    pub fn increment(&mut self, agent_id: &str) {
        let entry = self.clocks.entry(agent_id.to_string()).or_insert(0);
        *entry += 1;
    }

    /// Get the clock value for an agent.
    pub fn get(&self, agent_id: &str) -> u64 {
        self.clocks.get(agent_id).copied().unwrap_or(0)
    }

    /// Merge another version vector (take component-wise max).
    pub fn merge(&mut self, other: &VersionVector) {
        for (id, &clock) in &other.clocks {
            let entry = self.clocks.entry(id.clone()).or_insert(0);
            *entry = (*entry).max(clock);
        }
    }

    /// Check if this vector dominates another (happens-before or equal).
    /// V_a dominates V_b iff ∀i: V_a[i] ≥ V_b[i]
    pub fn dominates(&self, other: &VersionVector) -> bool {
        // Check that every component in other is ≤ our component
        for (id, &clock) in &other.clocks {
            if self.get(id) < clock {
                return false;
            }
        }
        true
    }

    /// Check if two vectors are concurrent (neither dominates).
    pub fn is_concurrent_with(&self, other: &VersionVector) -> bool {
        !self.dominates(other) && !other.dominates(self)
    }
}

/// A file edit operation that can participate in conflict detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionedEdit {
    /// File path.
    pub path: String,
    /// The agent that made this edit.
    pub agent_id: String,
    /// Version vector at the time of the edit.
    pub version: VersionVector,
    /// The content after this edit.
    pub content: String,
    /// The base content (last common ancestor) for three-way merge.
    pub base_content: Option<String>,
    /// Timestamp for display ordering.
    pub timestamp: u64,
}

/// Result of conflict detection between two edits.
#[derive(Debug, Clone)]
pub enum ConflictResult {
    /// No conflict — one edit causally precedes the other.
    NoConflict {
        /// The edit that should be applied (the later one).
        winner: PartitionedEdit,
    },
    /// Concurrent edits detected — needs resolution.
    Conflict {
        /// The two concurrent edits.
        edit_a: PartitionedEdit,
        edit_b: PartitionedEdit,
        /// Auto-merged content (if three-way merge succeeded).
        auto_merged: Option<String>,
        /// Conflict markers in the merged content (if merge had conflicts).
        has_markers: bool,
    },
}

/// The partition-aware merge system.
pub struct PartitionMerger {
    /// Per-file version vectors.
    file_versions: HashMap<String, VersionVector>,
    /// Per-file base content (last known common ancestor).
    file_bases: HashMap<String, String>,
    /// Pending conflicts awaiting resolution.
    pending_conflicts: Vec<FileConflict>,
}

/// A file conflict awaiting resolution.
#[derive(Debug, Clone)]
pub struct FileConflict {
    pub path: String,
    pub edit_a: PartitionedEdit,
    pub edit_b: PartitionedEdit,
    pub auto_merged: Option<String>,
    pub resolved: bool,
}

impl PartitionMerger {
    pub fn new() -> Self {
        Self {
            file_versions: HashMap::new(),
            file_bases: HashMap::new(),
            pending_conflicts: Vec::new(),
        }
    }

    /// Record the base content for a file (before any edits).
    pub fn set_base(&mut self, path: &str, content: String) {
        self.file_bases.insert(path.to_string(), content);
    }

    /// Record a local edit (updates the version vector).
    pub fn record_edit(&mut self, path: &str, agent_id: &str, content: String) -> PartitionedEdit {
        let version = self
            .file_versions
            .entry(path.to_string())
            .or_insert_with(VersionVector::new);
        version.increment(agent_id);

        let base_content = self.file_bases.get(path).cloned();

        // Update base to current content
        self.file_bases.insert(path.to_string(), content.clone());

        PartitionedEdit {
            path: path.to_string(),
            agent_id: agent_id.to_string(),
            version: version.clone(),
            content,
            base_content,
            timestamp: current_timestamp(),
        }
    }

    /// Check if a remote edit conflicts with our local state.
    pub fn detect_conflict(&mut self, remote_edit: &PartitionedEdit) -> ConflictResult {
        let local_version = self
            .file_versions
            .get(&remote_edit.path)
            .cloned()
            .unwrap_or_default();

        if local_version.dominates(&remote_edit.version) {
            // We've already seen this edit (or a later one) — no conflict
            return ConflictResult::NoConflict {
                winner: remote_edit.clone(),
            };
        }

        if remote_edit.version.dominates(&local_version) {
            // Remote is strictly newer — apply it
            self.file_versions
                .insert(remote_edit.path.clone(), remote_edit.version.clone());
            self.file_bases
                .insert(remote_edit.path.clone(), remote_edit.content.clone());
            return ConflictResult::NoConflict {
                winner: remote_edit.clone(),
            };
        }

        // Concurrent edits — attempt three-way merge
        let base = remote_edit
            .base_content
            .as_deref()
            .or_else(|| self.file_bases.get(&remote_edit.path).map(|s| s.as_str()))
            .unwrap_or("");

        let local_content = self
            .file_bases
            .get(&remote_edit.path)
            .map(|s| s.as_str())
            .unwrap_or("");

        let (merged, has_markers) = three_way_merge(base, local_content, &remote_edit.content);

        let local_edit = PartitionedEdit {
            path: remote_edit.path.clone(),
            agent_id: "local".to_string(),
            version: local_version,
            content: local_content.to_string(),
            base_content: Some(base.to_string()),
            timestamp: current_timestamp(),
        };

        let conflict = FileConflict {
            path: remote_edit.path.clone(),
            edit_a: local_edit.clone(),
            edit_b: remote_edit.clone(),
            auto_merged: Some(merged.clone()),
            resolved: !has_markers,
        };

        self.pending_conflicts.push(conflict);

        // Merge version vectors
        let version = self
            .file_versions
            .entry(remote_edit.path.clone())
            .or_insert_with(VersionVector::new);
        version.merge(&remote_edit.version);

        ConflictResult::Conflict {
            edit_a: local_edit,
            edit_b: remote_edit.clone(),
            auto_merged: Some(merged),
            has_markers,
        }
    }

    /// Get pending unresolved conflicts.
    pub fn pending_conflicts(&self) -> &[FileConflict] {
        &self.pending_conflicts
    }

    /// Resolve a conflict by choosing the merged content.
    pub fn resolve_conflict(&mut self, path: &str, resolved_content: String) {
        self.file_bases.insert(path.to_string(), resolved_content);
        self.pending_conflicts
            .retain(|c| c.path != path || c.resolved);
    }
}

impl Default for PartitionMerger {
    fn default() -> Self {
        Self::new()
    }
}

/// Simple three-way merge for text content.
/// Returns (merged_content, has_conflict_markers).
fn three_way_merge(base: &str, ours: &str, theirs: &str) -> (String, bool) {
    let base_lines: Vec<&str> = base.lines().collect();
    let our_lines: Vec<&str> = ours.lines().collect();
    let their_lines: Vec<&str> = theirs.lines().collect();

    let mut merged = Vec::new();
    let mut has_conflicts = false;
    let max_lines = our_lines.len().max(their_lines.len()).max(base_lines.len());

    for i in 0..max_lines {
        let base_line = base_lines.get(i).copied().unwrap_or("");
        let our_line = our_lines.get(i).copied().unwrap_or("");
        let their_line = their_lines.get(i).copied().unwrap_or("");

        if our_line == their_line {
            // Both agree — use either
            merged.push(our_line.to_string());
        } else if our_line == base_line {
            // We didn't change it, they did — use theirs
            merged.push(their_line.to_string());
        } else if their_line == base_line {
            // They didn't change it, we did — use ours
            merged.push(our_line.to_string());
        } else {
            // Both changed differently — conflict
            has_conflicts = true;
            merged.push("<<<<<<< OURS".to_string());
            merged.push(our_line.to_string());
            merged.push("=======".to_string());
            merged.push(their_line.to_string());
            merged.push(">>>>>>> THEIRS".to_string());
        }
    }

    (merged.join("\n"), has_conflicts)
}

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_vector_dominance() {
        let mut va = VersionVector::new();
        va.increment("a");
        va.increment("a");

        let mut vb = VersionVector::new();
        vb.increment("a");

        assert!(va.dominates(&vb));
        assert!(!vb.dominates(&va));
        assert!(!va.is_concurrent_with(&vb));
    }

    #[test]
    fn version_vector_concurrent() {
        let mut va = VersionVector::new();
        va.increment("a");

        let mut vb = VersionVector::new();
        vb.increment("b");

        assert!(!va.dominates(&vb));
        assert!(!vb.dominates(&va));
        assert!(va.is_concurrent_with(&vb));
    }

    #[test]
    fn three_way_merge_no_conflict() {
        let base = "line1\nline2\nline3";
        let ours = "line1\nmodified\nline3";
        let theirs = "line1\nline2\nchanged";

        let (merged, has_markers) = three_way_merge(base, ours, theirs);
        assert!(!has_markers);
        assert!(merged.contains("modified"));
        assert!(merged.contains("changed"));
    }

    #[test]
    fn three_way_merge_with_conflict() {
        let base = "line1\noriginal\nline3";
        let ours = "line1\nours_change\nline3";
        let theirs = "line1\ntheirs_change\nline3";

        let (merged, has_markers) = three_way_merge(base, ours, theirs);
        assert!(has_markers);
        assert!(merged.contains("<<<<<<< OURS"));
        assert!(merged.contains("ours_change"));
        assert!(merged.contains("theirs_change"));
        assert!(merged.contains(">>>>>>> THEIRS"));
    }
}
