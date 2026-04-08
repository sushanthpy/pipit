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

/// Three-way merge for text content using diff-based comparison.
///
/// Previous implementation compared line-by-line at matching indices,
/// which produced false conflicts on any insertion or deletion (every
/// subsequent line shifted and appeared changed). This version uses
/// LCS-based diffing: compute what each side changed relative to the
/// base, then merge non-overlapping changes. Overlapping changes where
/// both sides modified the same base region produce conflict markers.
///
/// Returns (merged_content, has_conflict_markers).
fn three_way_merge(base: &str, ours: &str, theirs: &str) -> (String, bool) {
    // Fast paths
    if ours == theirs {
        return (ours.to_string(), false);
    }
    if ours == base {
        return (theirs.to_string(), false);
    }
    if theirs == base {
        return (ours.to_string(), false);
    }

    // Compute line-level diffs from base to each side
    let base_lines: Vec<&str> = base.lines().collect();
    let our_lines: Vec<&str> = ours.lines().collect();
    let their_lines: Vec<&str> = theirs.lines().collect();

    // Compute LCS between base and each side to identify changed regions
    let our_changes = diff_lines(&base_lines, &our_lines);
    let their_changes = diff_lines(&base_lines, &their_lines);

    // Merge: apply non-conflicting changes from both sides
    let mut merged = Vec::new();
    let mut has_conflicts = false;
    let mut base_idx = 0;

    // Walk through base lines and apply changes
    let max_base = base_lines.len();
    while base_idx <= max_base {
        let our_edit = our_changes.iter().find(|c| c.base_start == base_idx);
        let their_edit = their_changes.iter().find(|c| c.base_start == base_idx);

        match (our_edit, their_edit) {
            (None, None) => {
                // No edits at this position — keep base line
                if base_idx < max_base {
                    merged.push(base_lines[base_idx].to_string());
                }
                base_idx += 1;
            }
            (Some(ours), None) => {
                // Only we changed this region
                merged.extend(ours.new_lines.iter().cloned());
                base_idx = ours.base_end;
            }
            (None, Some(theirs)) => {
                // Only they changed this region
                merged.extend(theirs.new_lines.iter().cloned());
                base_idx = theirs.base_end;
            }
            (Some(ours), Some(theirs)) => {
                // Both changed the same region
                if ours.new_lines == theirs.new_lines {
                    // Same change — no conflict
                    merged.extend(ours.new_lines.iter().cloned());
                } else {
                    // True conflict
                    has_conflicts = true;
                    merged.push("<<<<<<< OURS".to_string());
                    merged.extend(ours.new_lines.iter().cloned());
                    merged.push("=======".to_string());
                    merged.extend(theirs.new_lines.iter().cloned());
                    merged.push(">>>>>>> THEIRS".to_string());
                }
                base_idx = ours.base_end.max(theirs.base_end);
            }
        }
    }

    (merged.join("\n"), has_conflicts)
}

/// A region changed between base and the modified version.
#[derive(Debug)]
struct EditRegion {
    /// Start index in the base (inclusive)
    base_start: usize,
    /// End index in the base (exclusive)
    base_end: usize,
    /// The replacement lines
    new_lines: Vec<String>,
}

/// Compute changed regions between base and modified using a simple
/// greedy LCS approach. Returns a list of EditRegions.
fn diff_lines(base: &[&str], modified: &[&str]) -> Vec<EditRegion> {
    let mut regions = Vec::new();
    let mut bi = 0;
    let mut mi = 0;

    while bi < base.len() || mi < modified.len() {
        if bi < base.len() && mi < modified.len() && base[bi] == modified[mi] {
            // Lines match — advance both
            bi += 1;
            mi += 1;
        } else {
            // Mismatch — find where they re-sync
            let region_start = bi;
            let mod_start = mi;

            // Look ahead for the next matching line
            let mut found = false;
            for look_b in bi..base.len() {
                for look_m in mi..modified.len() {
                    if base[look_b] == modified[look_m] {
                        // Found sync point
                        regions.push(EditRegion {
                            base_start: region_start,
                            base_end: look_b,
                            new_lines: modified[mod_start..look_m]
                                .iter()
                                .map(|s| s.to_string())
                                .collect(),
                        });
                        bi = look_b;
                        mi = look_m;
                        found = true;
                        break;
                    }
                }
                if found {
                    break;
                }
            }

            if !found {
                // No sync point — rest is all changed
                regions.push(EditRegion {
                    base_start: region_start,
                    base_end: base.len(),
                    new_lines: modified[mod_start..]
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                });
                bi = base.len();
                mi = modified.len();
            }
        }
    }

    regions
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
