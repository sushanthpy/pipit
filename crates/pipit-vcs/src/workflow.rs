//! # VCS Workflow Finite-State Machine
//!
//! Typed state machine for all repository lifecycle operations.
//! Every mutation flows through explicit transitions with O(1) dispatch.
//! Validation cost is O(F + C + G) where F = files, C = checkpoint lineage, G = policies.

use crate::firewall::GitFirewall;
use crate::ledger::RepositoryLedger;
use crate::snapshot::SnapshotGraph;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

// ═══════════════════════════════════════════════════════════════
//  WORKFLOW PHASES — the core FSM states
// ═══════════════════════════════════════════════════════════════

/// The lifecycle phase of a workspace / branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkflowPhase {
    /// Initial state: workspace created but no mutations yet.
    Idle,
    /// Active editing: files are being modified.
    Editing,
    /// Snapshot taken: immutable checkpoint exists.
    Snapshotted,
    /// Verification running: build/lint/test in progress.
    Verifying,
    /// Verification passed: ready for promotion decision.
    Verified,
    /// Promotion proposed: contract predicates being evaluated.
    PendingPromotion,
    /// Promoted: changes merged into target branch.
    Promoted,
    /// Frozen: workspace is read-only, pending reconciliation.
    Frozen,
    /// Archived: workspace preserved but inactive.
    Archived,
    /// Abandoned: workspace explicitly discarded.
    Abandoned,
    /// Error: unrecoverable state requiring manual intervention.
    Error(String),
}

impl WorkflowPhase {
    /// Whether this phase is a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Promoted | Self::Archived | Self::Abandoned | Self::Error(_)
        )
    }

    /// Whether further edits are allowed in this phase.
    pub fn allows_edits(&self) -> bool {
        matches!(self, Self::Idle | Self::Editing)
    }
}

// ═══════════════════════════════════════════════════════════════
//  WORKFLOW OPERATIONS — typed commands
// ═══════════════════════════════════════════════════════════════

/// Every repository mutation is one of these typed operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkflowOp {
    /// Create a new workspace (worktree + branch).
    CreateWorkspace {
        name: String,
        base_branch: String,
        objective: Option<String>,
    },
    /// Record a snapshot of current state.
    Snapshot { message: String },
    /// Begin verification pipeline.
    Verify {
        checks: Vec<String>,
    },
    /// Record verification result.
    RecordVerification {
        check: String,
        passed: bool,
        evidence: String,
    },
    /// Propose promotion to target branch.
    ProposePromotion {
        target: String,
    },
    /// Execute promotion (merge).
    Promote {
        target: String,
        strategy: MergeStrategy,
    },
    /// Freeze workspace for reconciliation.
    Freeze,
    /// Archive workspace (preserve but deactivate).
    Archive { reason: String },
    /// Abandon workspace (explicit discard).
    Abandon { reason: String },
    /// Garbage-collect orphaned resources.
    GarbageCollect,
}

/// Merge strategy for promotions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MergeStrategy {
    /// Standard merge commit.
    Merge,
    /// Rebase onto target.
    Rebase,
    /// Squash all commits.
    Squash,
    /// Fast-forward only (fail if diverged).
    FastForward,
}

// ═══════════════════════════════════════════════════════════════
//  WORKFLOW TRANSITIONS — validated state changes
// ═══════════════════════════════════════════════════════════════

/// Result of a workflow transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowTransition {
    pub from: WorkflowPhase,
    pub to: WorkflowPhase,
    pub op: WorkflowOp,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub workspace_id: String,
}

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("Invalid transition from {from:?} via {op}")]
    InvalidTransition { from: WorkflowPhase, op: String },
    #[error("Firewall rejected operation: {0}")]
    FirewallRejected(String),
    #[error("Contract predicate failed: {0}")]
    ContractFailed(String),
    #[error("Git operation failed: {0}")]
    GitError(String),
    #[error("IO error: {0}")]
    IoError(String),
}

// ═══════════════════════════════════════════════════════════════
//  VCS KERNEL — the single authoritative entry point
// ═══════════════════════════════════════════════════════════════

/// The VCS Workflow Kernel. All repository mutations flow through here.
///
/// This is the **single source of truth** for workspace state.
/// It coordinates the firewall, snapshot graph, ledger, and workflow FSM.
pub struct VcsKernel {
    /// Project root directory.
    project_root: PathBuf,
    /// Semantic git firewall for security validation.
    pub firewall: GitFirewall,
    /// Immutable snapshot graph.
    pub snapshots: SnapshotGraph,
    /// Append-only repository ledger.
    pub ledger: RepositoryLedger,
    /// Active workspace states keyed by workspace ID.
    workspaces: std::collections::HashMap<String, WorkflowPhase>,
}

impl VcsKernel {
    /// Create a new VCS kernel rooted at the given project directory.
    pub fn new(project_root: PathBuf) -> Self {
        let ledger_path = project_root.join(".pipit").join("ledger.jsonl");
        let snapshot_path = project_root.join(".pipit").join("snapshots");
        Self {
            firewall: GitFirewall::new(),
            snapshots: SnapshotGraph::new(snapshot_path),
            ledger: RepositoryLedger::new(ledger_path),
            workspaces: std::collections::HashMap::new(),
            project_root,
        }
    }

    /// Load kernel state from disk (replay ledger).
    pub fn load(project_root: PathBuf) -> Result<Self, WorkflowError> {
        let mut kernel = Self::new(project_root);
        kernel.ledger.replay(&mut kernel.workspaces, &mut kernel.snapshots)
            .map_err(|e| WorkflowError::IoError(e.to_string()))?;
        Ok(kernel)
    }

    /// Execute a workflow operation. This is the **only** entry point for
    /// repository mutations. Returns the resulting transition.
    pub fn execute(
        &mut self,
        workspace_id: &str,
        op: WorkflowOp,
    ) -> Result<WorkflowTransition, WorkflowError> {
        let current = self
            .workspaces
            .get(workspace_id)
            .cloned()
            .unwrap_or(WorkflowPhase::Idle);

        // 1. Validate transition is legal
        let next = self.validate_transition(&current, &op)?;

        // 2. Run firewall checks
        self.firewall_check(&op)?;

        // 3. Build transition record
        let transition = WorkflowTransition {
            from: current.clone(),
            to: next.clone(),
            op: op.clone(),
            timestamp: chrono::Utc::now(),
            workspace_id: workspace_id.to_string(),
        };

        // 4. Append to ledger (durable write)
        self.ledger
            .append(&transition)
            .map_err(|e| WorkflowError::IoError(e.to_string()))?;

        // 5. Update in-memory state
        self.workspaces.insert(workspace_id.to_string(), next);

        tracing::info!(
            workspace = workspace_id,
            from = ?transition.from,
            to = ?transition.to,
            "VCS transition"
        );

        Ok(transition)
    }

    /// Validate that a transition from `current` via `op` is legal.
    /// This is O(1) — pure pattern matching.
    fn validate_transition(
        &self,
        current: &WorkflowPhase,
        op: &WorkflowOp,
    ) -> Result<WorkflowPhase, WorkflowError> {
        use WorkflowOp::*;
        use WorkflowPhase::*;

        match (current, op) {
            // Creating workspace: only from Idle
            (Idle, CreateWorkspace { .. }) => Ok(Editing),

            // Snapshotting: from Editing or Snapshotted (re-snapshot)
            (Editing, Snapshot { .. }) | (Snapshotted, Snapshot { .. }) => Ok(Snapshotted),

            // Begin verification: from Snapshotted or Editing
            (Snapshotted, Verify { .. }) | (Editing, Verify { .. }) => Ok(Verifying),

            // Record verification result: during Verifying
            (Verifying, RecordVerification { passed, .. }) => {
                if *passed {
                    Ok(Verified)
                } else {
                    Ok(Editing) // Failed verification returns to editing
                }
            }

            // Propose promotion: from Verified
            (Verified, ProposePromotion { .. }) => Ok(PendingPromotion),

            // Execute promotion: from PendingPromotion
            (PendingPromotion, Promote { .. }) => Ok(Promoted),

            // Freeze: from most non-terminal states
            (Editing | Snapshotted | Verified, Freeze) => Ok(Frozen),

            // Archive: from Frozen
            (Frozen, Archive { .. }) => Ok(Archived),

            // Abandon: from most non-terminal states
            (Idle | Editing | Snapshotted | Verified | Frozen, Abandon { .. }) => Ok(Abandoned),

            // GC: allowed from any state (operates on global resources)
            (_, GarbageCollect) => Ok(current.clone()),

            _ => Err(WorkflowError::InvalidTransition {
                from: current.clone(),
                op: format!("{:?}", op),
            }),
        }
    }

    /// Run firewall checks against the operation.
    fn firewall_check(&self, op: &WorkflowOp) -> Result<(), WorkflowError> {
        match op {
            WorkflowOp::Promote { target, .. } => {
                // Validate target branch isn't protected in dangerous ways
                if let Some(threat) = self.firewall.check_branch_mutation(target) {
                    return Err(WorkflowError::FirewallRejected(format!(
                        "Branch '{}' is protected: {:?}",
                        target, threat
                    )));
                }
            }
            WorkflowOp::CreateWorkspace { name, .. } => {
                if let Some(threat) = self.firewall.check_workspace_name(name) {
                    return Err(WorkflowError::FirewallRejected(format!(
                        "Workspace name '{}' rejected: {:?}",
                        name, threat
                    )));
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Get the current phase of a workspace.
    pub fn phase(&self, workspace_id: &str) -> WorkflowPhase {
        self.workspaces
            .get(workspace_id)
            .cloned()
            .unwrap_or(WorkflowPhase::Idle)
    }

    /// List all active (non-terminal) workspaces.
    pub fn active_workspaces(&self) -> Vec<(&str, &WorkflowPhase)> {
        self.workspaces
            .iter()
            .filter(|(_, phase)| !phase.is_terminal())
            .map(|(id, phase)| (id.as_str(), phase))
            .collect()
    }

    /// Project root path.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }
}
