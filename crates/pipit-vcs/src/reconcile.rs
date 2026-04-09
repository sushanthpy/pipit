//! # Two-Phase Workspace Reconciliation
//!
//! Replaces forceful worktree teardown with a structured protocol:
//! FreezeWorkspace → ClassifyState → ProposeAction → CommitDecision → Reconcile
//!
//! - Status extraction: O(F) where F = changed files
//! - Policy classification: O(1) once metadata loaded
//! - Mergeability estimation: O(F₁ + F₂) for touched-file intersection

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Current state of a workspace, determined by inspection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceState {
    /// Workspace identifier.
    pub workspace_id: String,
    /// Branch name.
    pub branch: String,
    /// Base commit the workspace was created from.
    pub base_commit: String,
    /// Files modified in the workspace.
    pub modified_files: Vec<String>,
    /// Whether uncommitted changes exist.
    pub has_uncommitted: bool,
    /// Number of commits ahead of base.
    pub commits_ahead: u32,
    /// Whether verification has passed.
    pub verified: bool,
    /// Whether an active contract exists.
    pub has_contract: bool,
    /// Age of the workspace.
    pub created_at: DateTime<Utc>,
    /// Last activity timestamp.
    pub last_active: DateTime<Utc>,
}

/// Proposed reconciliation action — never destructive without explicit decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReconcileAction {
    /// Promote: merge workspace into target branch.
    Promote {
        target_branch: String,
        strategy: String,
    },
    /// Archive: preserve workspace state but mark as inactive.
    Archive {
        reason: String,
        snapshot_id: Option<String>,
    },
    /// Abandon: discard workspace (after snapshotting).
    Abandon {
        reason: String,
        force_snapshot: bool,
    },
    /// Stale: workspace is old with no recent activity — suggest cleanup.
    SuggestCleanup { age_days: u64, reason: String },
    /// Conflict: workspace conflicts with another workspace.
    ResolveConflict {
        conflicting_workspace: String,
        conflicting_files: Vec<String>,
    },
    /// No action needed — workspace is healthy.
    NoAction,
}

/// Outcome of a reconciliation operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileOutcome {
    pub workspace_id: String,
    pub action: ReconcileAction,
    pub snapshot_taken: Option<String>,
    pub success: bool,
    pub details: String,
    pub timestamp: DateTime<Utc>,
}

/// The workspace reconciler — evaluates workspaces and proposes safe actions.
pub struct WorkspaceReconciler {
    /// Maximum workspace age before suggesting cleanup (days).
    pub stale_threshold_days: u64,
    /// Maximum uncommitted file count before warning.
    pub max_uncommitted_files: usize,
}

impl Default for WorkspaceReconciler {
    fn default() -> Self {
        Self {
            stale_threshold_days: 7,
            max_uncommitted_files: 50,
        }
    }
}

impl WorkspaceReconciler {
    /// Create a new reconciler with default thresholds.
    pub fn new() -> Self {
        Self::default()
    }

    /// Phase 1: Classify a workspace's current state and propose an action.
    /// This is a pure function — no side effects.
    ///
    /// Complexity: O(F) for file inspection, O(1) for classification.
    pub fn classify(&self, state: &WorkspaceState) -> ReconcileAction {
        let age_days = (Utc::now() - state.created_at).num_days().unsigned_abs();

        // Check for staleness
        if age_days > self.stale_threshold_days
            && (Utc::now() - state.last_active).num_days().unsigned_abs()
                > self.stale_threshold_days
        {
            return ReconcileAction::SuggestCleanup {
                age_days,
                reason: format!(
                    "Workspace '{}' is {} days old with no recent activity",
                    state.workspace_id, age_days
                ),
            };
        }

        // If verified and has contract, suggest promotion
        if state.verified && state.has_contract {
            return ReconcileAction::Promote {
                target_branch: "main".to_string(), // Default, overridden by contract
                strategy: "merge".to_string(),
            };
        }

        // If verified but no contract, suggest archive
        if state.verified && !state.has_contract {
            return ReconcileAction::Archive {
                reason: "Verified but no promotion contract".to_string(),
                snapshot_id: None,
            };
        }

        // No action needed for active workspaces
        ReconcileAction::NoAction
    }

    /// Check for file-level conflicts between two workspaces.
    ///
    /// Complexity: O(F₁ + F₂) for file-set intersection.
    pub fn check_conflicts(&self, a: &WorkspaceState, b: &WorkspaceState) -> Vec<String> {
        let files_a: HashSet<&str> = a.modified_files.iter().map(|s| s.as_str()).collect();
        let files_b: HashSet<&str> = b.modified_files.iter().map(|s| s.as_str()).collect();

        files_a
            .intersection(&files_b)
            .map(|s| s.to_string())
            .collect()
    }

    /// Phase 2: Commit to a reconciliation action.
    /// This should only be called after the user/system approves the proposed action.
    pub fn commit_action(&self, workspace_id: &str, action: &ReconcileAction) -> ReconcileOutcome {
        ReconcileOutcome {
            workspace_id: workspace_id.to_string(),
            action: action.clone(),
            snapshot_taken: None, // Filled by the caller after taking snapshot
            success: true,
            details: format!("Reconciliation action committed for {}", workspace_id),
            timestamp: Utc::now(),
        }
    }

    /// Scan all workspaces and return those needing attention.
    pub fn scan(&self, workspaces: &[WorkspaceState]) -> Vec<(String, ReconcileAction)> {
        let mut results = Vec::new();

        // Classify each workspace
        for ws in workspaces {
            let action = self.classify(ws);
            if !matches!(action, ReconcileAction::NoAction) {
                results.push((ws.workspace_id.clone(), action));
            }
        }

        // Check for inter-workspace conflicts
        for i in 0..workspaces.len() {
            for j in (i + 1)..workspaces.len() {
                let conflicts = self.check_conflicts(&workspaces[i], &workspaces[j]);
                if !conflicts.is_empty() {
                    results.push((
                        workspaces[i].workspace_id.clone(),
                        ReconcileAction::ResolveConflict {
                            conflicting_workspace: workspaces[j].workspace_id.clone(),
                            conflicting_files: conflicts,
                        },
                    ));
                }
            }
        }

        results
    }
}
