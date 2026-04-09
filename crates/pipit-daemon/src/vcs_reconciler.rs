//! # Daemon VCS Reconciliation
//!
//! Background reconciliation for workspace health. Periodically scans for:
//! - Stale worktrees with no recent activity
//! - Expired contracts
//! - Orphaned snapshots
//! - File-level conflicts between active workspaces
//! - Promotion-ready branches
//!
//! Complexity: O(W · (F + C + E)) where W = active workspaces, F = changed files,
//! C = commits inspected, E = relevant ledger entries.
//! Conflict detection: O(ΣF) with hash-indexed paths.

use pipit_vcs::{
    ReconcileAction, RepositoryLedger, VcsGateway, VcsKernel, WorkflowPhase, WorkspaceReconciler,
    WorkspaceState,
    ledger::{LedgerEntry, LedgerEvent},
};
use std::path::PathBuf;
use tracing::{debug, info, warn};

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
    /// Base branch to compare workspaces against (default: "main").
    pub base_branch: String,
}

impl Default for ReconcilerConfig {
    fn default() -> Self {
        Self {
            interval_secs: 300, // 5 minutes
            stale_threshold_days: 7,
            auto_archive_stale: false,
            dry_run: true,
            base_branch: "main".to_string(),
        }
    }
}

/// Build a truth-sourced `WorkspaceState` from git queries and ledger history.
///
/// Hydration cost: O(F + C + E) where F = changed files, C = commits,
/// E = relevant ledger entries for this workspace.
fn hydrate_workspace_state(
    gateway: &VcsGateway,
    workspace_id: &str,
    branch: &str,
    phase: &WorkflowPhase,
    ledger_entries: &[LedgerEntry],
    base_branch: &str,
) -> WorkspaceState {
    // Query git for modified files
    let modified_files = gateway
        .workspace_modified_files(branch, base_branch)
        .unwrap_or_else(|e| {
            debug!(
                workspace = workspace_id,
                "failed to get modified files: {}", e
            );
            Vec::new()
        });

    // Query git for commits ahead
    let (commits_ahead, _behind) = gateway
        .commits_ahead_behind(branch, base_branch)
        .unwrap_or_else(|e| {
            debug!(
                workspace = workspace_id,
                "failed to get commit counts: {}", e
            );
            (0, 0)
        });

    // Query git for base commit
    let base_commit = gateway
        .branch_base_commit(branch, base_branch)
        .unwrap_or_default();

    // Check for uncommitted changes via worktree path
    let has_uncommitted = gateway
        .parse_worktrees()
        .ok()
        .and_then(|wts| {
            wts.iter()
                .find(|(_, b)| b == branch)
                .map(|(path, _)| gateway.branch_has_uncommitted(path).unwrap_or(false))
        })
        .unwrap_or(false);

    // Extract timestamps from ledger history for this workspace
    let (created_at, last_active) = extract_workspace_timestamps(workspace_id, ledger_entries);

    // Check for contract in ledger events
    let has_contract = ledger_entries.iter().any(|entry| {
        matches!(
            &entry.event,
            LedgerEvent::ContractCreated { workspace_id: id, .. } if id == workspace_id
        )
    });

    WorkspaceState {
        workspace_id: workspace_id.to_string(),
        branch: branch.to_string(),
        base_commit,
        modified_files,
        has_uncommitted,
        commits_ahead,
        verified: matches!(phase, WorkflowPhase::Verified),
        has_contract,
        created_at,
        last_active,
    }
}

/// Extract created_at and last_active timestamps from ledger entries.
///
/// Scans relevant entries for this workspace: created_at is the timestamp
/// of the first WorkspaceCreated event; last_active is the latest timestamp
/// of any event referencing this workspace.
fn extract_workspace_timestamps(
    workspace_id: &str,
    entries: &[LedgerEntry],
) -> (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) {
    let now = chrono::Utc::now();
    let mut created_at = now;
    let mut last_active = now;
    let mut found_create = false;

    for entry in entries {
        let matches_workspace = match &entry.event {
            LedgerEvent::WorkspaceCreated {
                workspace_id: id, ..
            } => {
                if id == workspace_id && !found_create {
                    created_at = entry.timestamp;
                    found_create = true;
                }
                id == workspace_id
            }
            LedgerEvent::PhaseTransition {
                workspace_id: id, ..
            }
            | LedgerEvent::SnapshotCreated {
                workspace_id: id, ..
            }
            | LedgerEvent::VerificationCompleted {
                workspace_id: id, ..
            }
            | LedgerEvent::ContractCreated {
                workspace_id: id, ..
            }
            | LedgerEvent::GateEvaluated {
                workspace_id: id, ..
            }
            | LedgerEvent::PromotionExecuted {
                workspace_id: id, ..
            }
            | LedgerEvent::WorkspaceReconciled {
                workspace_id: id, ..
            }
            | LedgerEvent::FirewallBlocked {
                workspace_id: id, ..
            } => id == workspace_id,
            LedgerEvent::Note {
                workspace_id: Some(id),
                ..
            } => id == workspace_id,
            LedgerEvent::ConflictDetected {
                workspace_a,
                workspace_b,
                ..
            } => workspace_a == workspace_id || workspace_b == workspace_id,
            _ => false,
        };

        if matches_workspace && entry.timestamp > last_active {
            last_active = entry.timestamp;
        }
    }

    // If no create event found, use earliest matching entry or now
    if !found_create {
        // Use last_active as created_at fallback (better than now)
        created_at = last_active;
    }

    (created_at, last_active)
}

/// Run a single reconciliation pass over the project workspace.
///
/// Queries git for real workspace state (modified files, commits ahead,
/// uncommitted changes) and the ledger for timestamps and contracts.
///
/// Returns the number of issues found.
pub fn reconcile_pass(project_root: &PathBuf, config: &ReconcilerConfig) -> usize {
    let kernel = match VcsKernel::load(project_root.clone()) {
        Ok(k) => k,
        Err(e) => {
            warn!("Failed to load VCS kernel for reconciliation: {}", e);
            return 0;
        }
    };

    let gateway = VcsGateway::new(project_root.clone());

    // Read all ledger entries once for timestamp and contract lookups
    let ledger_path = project_root.join(".pipit").join("ledger.jsonl");
    let ledger = RepositoryLedger::new(ledger_path);
    let all_entries = ledger.read_all().unwrap_or_else(|e| {
        warn!("Failed to read ledger for reconciliation: {}", e);
        Vec::new()
    });

    let reconciler = WorkspaceReconciler {
        stale_threshold_days: config.stale_threshold_days,
        ..Default::default()
    };

    // Gather truth-sourced workspace states
    let workspaces: Vec<WorkspaceState> = kernel
        .active_workspaces()
        .iter()
        .map(|(id, phase)| {
            let branch = format!("pipit/{}", id);
            hydrate_workspace_state(
                &gateway,
                id,
                &branch,
                phase,
                &all_entries,
                &config.base_branch,
            )
        })
        .collect();

    debug!(
        workspace_count = workspaces.len(),
        "reconciliation: hydrated workspace states"
    );

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

/// Build a causal snapshot of the current workspace state.
/// Used by daemon APIs and monitoring to expose consistent cross-store state.
pub fn build_causal_snapshot(
    project_root: &PathBuf,
) -> pipit_core::causal_snapshot::CausalSnapshot {
    use pipit_core::causal_snapshot::{CausalSnapshotBuilder, WorkspaceInfo};

    let mut builder = CausalSnapshotBuilder::new();

    // VCS state
    match VcsKernel::load(project_root.clone()) {
        Ok(kernel) => {
            let active: Vec<WorkspaceInfo> = kernel
                .active_workspaces()
                .iter()
                .map(|(id, phase)| WorkspaceInfo {
                    workspace_id: id.to_string(),
                    phase: format!("{:?}", phase),
                    has_contract: kernel.contracts.has_contract(id),
                    modified_file_count: 0, // Would need git query
                })
                .collect();
            let promotable = kernel.contracts.promotable().len();
            builder = builder.with_vcs_kernel(
                active,
                promotable,
                0, // conflict count from last reconcile
                kernel.ledger.len(),
            );
        }
        Err(e) => {
            builder = builder.without_vcs(&format!("kernel load failed: {}", e));
        }
    }

    builder.build()
}
