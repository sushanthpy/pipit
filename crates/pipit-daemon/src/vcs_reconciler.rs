//! # Daemon VCS Reconciliation
//!
//! Background reconciliation for workspace health. Periodically scans for:
//! - Stale worktrees with no recent activity
//! - Expired contracts
//! - Orphaned snapshots
//! - File-level conflicts between active workspaces
//! - Promotion-ready branches
//!
//! Complexity: O(W + E) where W = active workspaces, E = outstanding events.
//! Conflict detection: O(ΣF) with hash-indexed paths.

use pipit_vcs::{
    ReconcileAction, RepositoryLedger, WorkspaceReconciler, WorkspaceState,
    VcsKernel, WorkflowPhase,
    ledger::LedgerEvent,
};
use std::path::PathBuf;
use tracing::{info, warn};

/// Configuration for the background reconciler.
pub struct ReconcilerConfig {
    /// How often to run reconciliation (seconds).
    pub interval_secs: u64,
    /// Maximum workspace age before suggesting cleanup (days).
    pub stale_threshold_days: u64,
    /// Whether to auto-archive stale workspaces.
    pub auto_archive_stale: bool,
    /// Whether to log found issues without taking action.
    pub dry_run: bool,
}

impl Default for ReconcilerConfig {
    fn default() -> Self {
        Self {
            interval_secs: 300, // 5 minutes
            stale_threshold_days: 7,
            auto_archive_stale: false,
            dry_run: true,
        }
    }
}

/// Run a single reconciliation pass over the project workspace.
///
/// Returns the number of issues found.
pub fn reconcile_pass(
    project_root: &PathBuf,
    config: &ReconcilerConfig,
) -> usize {
    let kernel = match VcsKernel::load(project_root.clone()) {
        Ok(k) => k,
        Err(e) => {
            warn!("Failed to load VCS kernel for reconciliation: {}", e);
            return 0;
        }
    };

    let reconciler = WorkspaceReconciler {
        stale_threshold_days: config.stale_threshold_days,
        ..Default::default()
    };

    // Gather workspace states
    let workspaces: Vec<WorkspaceState> = kernel
        .active_workspaces()
        .iter()
        .map(|(id, phase)| WorkspaceState {
            workspace_id: id.to_string(),
            branch: format!("pipit/{}", id),
            base_commit: String::new(), // Would need git query
            modified_files: Vec::new(), // Would need git status
            has_uncommitted: false,
            commits_ahead: 0,
            verified: matches!(phase, WorkflowPhase::Verified),
            has_contract: false, // Would check contract registry
            created_at: chrono::Utc::now(), // Would need ledger lookup
            last_active: chrono::Utc::now(),
        })
        .collect();

    let issues = reconciler.scan(&workspaces);

    for (workspace_id, action) in &issues {
        match action {
            ReconcileAction::SuggestCleanup { age_days, reason } => {
                info!(
                    workspace = workspace_id,
                    age_days = age_days,
                    "Stale workspace detected: {}",
                    reason
                );
            }
            ReconcileAction::ResolveConflict {
                conflicting_workspace,
                conflicting_files,
            } => {
                warn!(
                    workspace = workspace_id,
                    other = conflicting_workspace.as_str(),
                    files = conflicting_files.len(),
                    "Inter-workspace conflict detected"
                );
            }
            ReconcileAction::Promote { target_branch, .. } => {
                info!(
                    workspace = workspace_id,
                    target = target_branch.as_str(),
                    "Workspace ready for promotion"
                );
            }
            ReconcileAction::Archive { reason, .. } => {
                info!(
                    workspace = workspace_id,
                    "Workspace candidate for archive: {}", reason
                );
            }
            _ => {}
        }
    }

    if !issues.is_empty() && !config.dry_run {
        // Write issues to ledger
        let ledger_path = project_root.join(".pipit").join("ledger.jsonl");
        let mut ledger = RepositoryLedger::new(ledger_path);
        ledger.set_actor("daemon-reconciler");

        for (workspace_id, action) in &issues {
            let _ = ledger.append_event(LedgerEvent::Note {
                workspace_id: Some(workspace_id.clone()),
                message: format!("Reconciler action: {:?}", action),
            });
        }
    }

    issues.len()
}
