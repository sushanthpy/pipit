//! # Immutable Snapshot Graph
//!
//! Persistent DAG of workspace snapshots keyed by content hash.
//! Each snapshot stores parent ID, branch identity, base commit, touched file set,
//! verification evidence, and restore policy.
//!
//! - Snapshot creation: O(F + D) where F = files, D = diff size
//! - Restore planning: O(A + B) for ancestor search in branch-local lineage
//! - Rollback is explicit via DAG ancestry — no mutable stash semantics

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;

/// Unique identifier for a snapshot (content-addressable hash or UUID).
pub type SnapshotId = String;

/// A single immutable snapshot in the DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// Unique content-addressable ID (SHA-256 of metadata + touched files).
    pub id: SnapshotId,
    /// Parent snapshot ID (None for root snapshots).
    pub parent: Option<SnapshotId>,
    /// Branch or worktree this snapshot belongs to.
    pub branch: String,
    /// Git commit hash at the time of snapshot.
    pub base_commit: String,
    /// Files modified relative to parent snapshot.
    pub touched_files: Vec<String>,
    /// File content hashes for rollback verification.
    pub file_hashes: HashMap<String, String>,
    /// Human-readable description.
    pub message: String,
    /// When this snapshot was created.
    pub created_at: DateTime<Utc>,
    /// Verification evidence attached to this snapshot.
    pub evidence: Vec<VerificationEvidence>,
    /// Restore policy governing how this snapshot can be used.
    pub restore_policy: RestorePolicy,
    /// Workspace ID that created this snapshot.
    pub workspace_id: String,
}

/// Evidence from a verification check attached to a snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationEvidence {
    pub check_name: String,
    pub passed: bool,
    pub output_summary: String,
    pub timestamp: DateTime<Utc>,
}

/// Policy governing how a snapshot can be restored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RestorePolicy {
    /// Can always be restored freely.
    Unrestricted,
    /// Must re-verify after restore.
    RequiresReverification,
    /// Cannot be restored (archive-only).
    ArchiveOnly,
    /// Custom predicate (description for display).
    Custom(String),
}

impl Default for RestorePolicy {
    fn default() -> Self {
        Self::Unrestricted
    }
}

/// The persistent DAG of snapshots.
pub struct SnapshotGraph {
    /// All snapshots indexed by ID.
    snapshots: HashMap<SnapshotId, Snapshot>,
    /// Head snapshot per branch/workspace.
    heads: HashMap<String, SnapshotId>,
    /// Storage directory for persistence.
    storage_dir: PathBuf,
}

impl SnapshotGraph {
    /// Create a new snapshot graph with the given storage directory.
    pub fn new(storage_dir: PathBuf) -> Self {
        Self {
            snapshots: HashMap::new(),
            heads: HashMap::new(),
            storage_dir,
        }
    }

    /// Load the graph from disk.
    pub fn load(storage_dir: PathBuf) -> Result<Self, std::io::Error> {
        let mut graph = Self::new(storage_dir.clone());
        let index_path = storage_dir.join("index.json");
        if index_path.exists() {
            let data = std::fs::read_to_string(&index_path)?;
            let persisted: PersistedGraph = serde_json::from_str(&data)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            graph.snapshots = persisted.snapshots;
            graph.heads = persisted.heads;
        }
        Ok(graph)
    }

    /// Persist the graph to disk.
    pub fn save(&self) -> Result<(), std::io::Error> {
        std::fs::create_dir_all(&self.storage_dir)?;
        let persisted = PersistedGraph {
            snapshots: self.snapshots.clone(),
            heads: self.heads.clone(),
        };
        let data = serde_json::to_string_pretty(&persisted)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = self.storage_dir.join("index.json.tmp");
        let target = self.storage_dir.join("index.json");
        std::fs::write(&tmp, &data)?;
        std::fs::rename(&tmp, &target)?;
        Ok(())
    }

    /// Create a new snapshot. Returns the snapshot ID.
    /// Complexity: O(F) where F = number of touched files.
    pub fn create_snapshot(
        &mut self,
        workspace_id: &str,
        branch: &str,
        base_commit: &str,
        touched_files: Vec<String>,
        file_hashes: HashMap<String, String>,
        message: &str,
    ) -> SnapshotId {
        let parent = self.heads.get(workspace_id).cloned();

        // Content-addressable ID
        let id = Self::compute_id(branch, base_commit, &touched_files, &parent);

        let snapshot = Snapshot {
            id: id.clone(),
            parent,
            branch: branch.to_string(),
            base_commit: base_commit.to_string(),
            touched_files,
            file_hashes,
            message: message.to_string(),
            created_at: Utc::now(),
            evidence: Vec::new(),
            restore_policy: RestorePolicy::default(),
            workspace_id: workspace_id.to_string(),
        };

        self.snapshots.insert(id.clone(), snapshot);
        self.heads.insert(workspace_id.to_string(), id.clone());

        id
    }

    /// Attach verification evidence to a snapshot.
    pub fn attach_evidence(&mut self, snapshot_id: &str, evidence: VerificationEvidence) {
        if let Some(snapshot) = self.snapshots.get_mut(snapshot_id) {
            snapshot.evidence.push(evidence);
        }
    }

    /// Get a snapshot by ID.
    pub fn get(&self, id: &str) -> Option<&Snapshot> {
        self.snapshots.get(id)
    }

    /// Get the head snapshot for a workspace.
    pub fn head(&self, workspace_id: &str) -> Option<&Snapshot> {
        self.heads
            .get(workspace_id)
            .and_then(|id| self.snapshots.get(id))
    }

    /// Walk the ancestry chain from a snapshot back to root.
    /// Complexity: O(A) where A = ancestor count.
    pub fn ancestors(&self, snapshot_id: &str) -> Vec<&Snapshot> {
        let mut result = Vec::new();
        let mut current = snapshot_id;
        while let Some(snapshot) = self.snapshots.get(current) {
            result.push(snapshot);
            match &snapshot.parent {
                Some(parent) => current = parent,
                None => break,
            }
        }
        result
    }

    /// Find the common ancestor between two snapshots.
    /// Complexity: O(A + B) for both ancestor chains.
    pub fn common_ancestor(&self, a: &str, b: &str) -> Option<&Snapshot> {
        let ancestors_a: std::collections::HashSet<&str> = self
            .ancestors(a)
            .iter()
            .map(|s| s.id.as_str())
            .collect();

        for ancestor in self.ancestors(b) {
            if ancestors_a.contains(ancestor.id.as_str()) {
                return Some(ancestor);
            }
        }
        None
    }

    /// Check if restoring a snapshot would conflict with current files.
    /// Returns the set of conflicting file paths.
    /// Complexity: O(F₁ + F₂) for touched-file intersection.
    pub fn conflict_check(
        &self,
        snapshot_id: &str,
        current_modified: &[String],
    ) -> Vec<String> {
        let snapshot = match self.get(snapshot_id) {
            Some(s) => s,
            None => return Vec::new(),
        };

        let touched: std::collections::HashSet<&str> =
            snapshot.touched_files.iter().map(|s| s.as_str()).collect();

        current_modified
            .iter()
            .filter(|f| touched.contains(f.as_str()))
            .cloned()
            .collect()
    }

    /// Total number of snapshots in the graph.
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    /// Whether the graph is empty.
    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// Compute a content-addressable ID for a snapshot.
    fn compute_id(
        branch: &str,
        commit: &str,
        files: &[String],
        parent: &Option<String>,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(branch.as_bytes());
        hasher.update(commit.as_bytes());
        for f in files {
            hasher.update(f.as_bytes());
        }
        if let Some(p) = parent {
            hasher.update(p.as_bytes());
        }
        hasher.update(Utc::now().to_rfc3339().as_bytes());
        format!("snap-{}", hex::encode(&hasher.finalize()[..8]))
    }
}

#[derive(Serialize, Deserialize)]
struct PersistedGraph {
    snapshots: HashMap<SnapshotId, Snapshot>,
    heads: HashMap<String, SnapshotId>,
}

// Minimal hex encoding (avoid extra dependency)
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }
}
