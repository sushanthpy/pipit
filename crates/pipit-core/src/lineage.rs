//! Subagent Lineage, Budgets, and Merge Contracts (Architecture Task 5)
//!
//! Makes subagents into first-class execution branches with:
//! - Lineage DAG (parent/child relationships)
//! - Capability inheritance (lattice meet)
//! - Token and wall-clock budgets
//! - Structured merge contracts (not textual summaries)

use crate::capability::CapabilitySet;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Unique task identifier for lineage tracking.
pub type TaskId = String;

/// A subagent execution branch with full lineage metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionBranch {
    /// Unique task ID.
    pub task_id: TaskId,
    /// Parent task ID (None for root agent).
    pub parent_id: Option<TaskId>,
    /// Depth in the delegation tree (0 = root).
    pub depth: u32,
    /// Inherited capability set (lattice meet of parent grant ∩ request).
    pub capabilities: CapabilitySet,
    /// Token budget for this branch.
    pub token_budget: TokenBudget,
    /// Wall-clock budget.
    pub wall_clock_budget: Duration,
    /// Task description.
    pub task: String,
    /// Context inherited from parent.
    pub inherited_context: String,
    /// Evidence inherited from parent (subset).
    pub inherited_evidence_ids: Vec<String>,
    /// Allowed tools (empty = all tools within capability set).
    pub allowed_tools: Vec<String>,
    /// Whether to use worktree isolation.
    pub isolated: bool,
    /// Status of this branch.
    pub status: BranchStatus,
    /// Merge contract (set when branch completes).
    pub merge_contract: Option<MergeContract>,
    /// Timestamps.
    pub created_at: u64,
    pub completed_at: Option<u64>,
}

/// Token budget for a subagent branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBudget {
    /// Maximum input tokens.
    pub max_input: u64,
    /// Maximum output tokens.
    pub max_output: u64,
    /// Tokens used so far.
    pub used_input: u64,
    pub used_output: u64,
}

impl TokenBudget {
    pub fn new(max_input: u64, max_output: u64) -> Self {
        Self {
            max_input,
            max_output,
            used_input: 0,
            used_output: 0,
        }
    }

    pub fn remaining_input(&self) -> u64 {
        self.max_input.saturating_sub(self.used_input)
    }

    pub fn remaining_output(&self) -> u64 {
        self.max_output.saturating_sub(self.used_output)
    }

    pub fn is_exhausted(&self) -> bool {
        self.used_input >= self.max_input || self.used_output >= self.max_output
    }

    pub fn consume(&mut self, input: u64, output: u64) {
        self.used_input += input;
        self.used_output += output;
    }
}

/// Branch execution status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BranchStatus {
    /// Waiting to start.
    Pending,
    /// Currently executing.
    Running,
    /// Completed successfully.
    Completed,
    /// Failed.
    Failed { reason: String },
    /// Cancelled (by parent or budget exhaustion).
    Cancelled { reason: String },
    /// Merged into parent.
    Merged,
}

/// A structured merge contract — machine-actionable, not prose.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeContract {
    /// Files changed by this branch.
    pub changed_files: Vec<ChangedFile>,
    /// Semantic intent of the changes.
    pub intent: String,
    /// Verification obligations that must be met before merge.
    /// Each obligation is a typed predicate, not a free-form string.
    pub verification_obligations: Vec<VerificationObligation>,
    /// Rollback point (git ref or checkpoint ID).
    pub rollback_point: String,
    /// Whether the branch self-reports as complete.
    pub self_reported_complete: bool,
    /// Confidence in the changes (0.0–1.0).
    pub confidence: f32,
    /// Diff summary (unified diff).
    pub diff_summary: String,
    /// Worktree or branch name for isolated branches.
    pub branch_name: Option<String>,
}

/// A typed verification obligation — machine-checkable predicate.
/// Satisfaction requires presenting matching `ObligationEvidence`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VerificationObligation {
    /// A command must exit zero (tests, lint, build).
    CommandMustPass {
        /// The command to run (e.g. "cargo test").
        command: String,
        /// Human-readable label (e.g. "unit tests").
        label: String,
    },
    /// A specific file must not have been modified.
    FileUnmodified { path: String },
    /// Manual review required.
    ManualReview { description: String },
    /// Custom predicate with an ID for matching.
    Custom { id: String, description: String },
}

/// Evidence that satisfies a verification obligation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ObligationEvidence {
    /// A command was run and exited with the given code.
    CommandResult {
        command: String,
        exit_code: i32,
        stdout_hash: Option<String>,
    },
    /// A file's content hash at verification time.
    FileHash { path: String, hash: String },
    /// Manual approval recorded.
    ManualApproval { reviewer: String, timestamp: u64 },
    /// Custom evidence.
    CustomEvidence { id: String, data: String },
}

impl VerificationObligation {
    /// Check if a piece of evidence satisfies this obligation.
    pub fn satisfied_by(&self, evidence: &ObligationEvidence) -> bool {
        match (self, evidence) {
            (
                VerificationObligation::CommandMustPass { command, .. },
                ObligationEvidence::CommandResult {
                    command: ev_cmd,
                    exit_code,
                    ..
                },
            ) => command == ev_cmd && *exit_code == 0,
            (
                VerificationObligation::FileUnmodified { path },
                ObligationEvidence::FileHash { path: ev_path, .. },
            ) => path == ev_path,
            (
                VerificationObligation::ManualReview { .. },
                ObligationEvidence::ManualApproval { .. },
            ) => true,
            (
                VerificationObligation::Custom { id, .. },
                ObligationEvidence::CustomEvidence { id: ev_id, .. },
            ) => id == ev_id,
            _ => false,
        }
    }
}

/// A file changed by the subagent branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub change_type: FileChangeType,
    pub lines_added: u32,
    pub lines_removed: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FileChangeType {
    Created,
    Modified,
    Deleted,
    Renamed { from: String },
}

// ─── Lineage DAG ────────────────────────────────────────────────────────

/// The subagent lineage DAG — tracks all branches and their relationships.
pub struct LineageDAG {
    /// All branches, keyed by task ID.
    branches: HashMap<TaskId, ExecutionBranch>,
    /// Parent → children mapping.
    children: HashMap<TaskId, Vec<TaskId>>,
    /// Root task ID.
    root_id: Option<TaskId>,
}

impl LineageDAG {
    pub fn new() -> Self {
        Self {
            branches: HashMap::new(),
            children: HashMap::new(),
            root_id: None,
        }
    }

    /// Register the root task.
    pub fn set_root(&mut self, task_id: TaskId, task: &str) {
        let branch = ExecutionBranch {
            task_id: task_id.clone(),
            parent_id: None,
            depth: 0,
            capabilities: CapabilitySet::ALL,
            token_budget: TokenBudget::new(u64::MAX, u64::MAX),
            wall_clock_budget: Duration::from_secs(3600),
            task: task.to_string(),
            inherited_context: String::new(),
            inherited_evidence_ids: vec![],
            allowed_tools: vec![],
            isolated: false,
            status: BranchStatus::Running,
            merge_contract: None,
            created_at: current_timestamp(),
            completed_at: None,
        };
        self.branches.insert(task_id.clone(), branch);
        self.root_id = Some(task_id);
    }

    /// Spawn a child branch. Returns the child's task ID.
    pub fn spawn(
        &mut self,
        parent_id: &str,
        child_id: TaskId,
        task: &str,
        capabilities: CapabilitySet,
        token_budget: TokenBudget,
        wall_clock_budget: Duration,
        allowed_tools: Vec<String>,
        isolated: bool,
    ) -> Result<&ExecutionBranch, String> {
        let parent = self
            .branches
            .get(parent_id)
            .ok_or_else(|| format!("Parent task {} not found", parent_id))?;

        let parent_caps = parent.capabilities;
        let child_caps = parent_caps.meet(capabilities);

        let depth = parent.depth + 1;
        if depth > 5 {
            return Err("Maximum delegation depth (5) exceeded".to_string());
        }

        let branch = ExecutionBranch {
            task_id: child_id.clone(),
            parent_id: Some(parent_id.to_string()),
            depth,
            capabilities: child_caps,
            token_budget,
            wall_clock_budget,
            task: task.to_string(),
            inherited_context: String::new(),
            inherited_evidence_ids: vec![],
            allowed_tools,
            isolated,
            status: BranchStatus::Pending,
            merge_contract: None,
            created_at: current_timestamp(),
            completed_at: None,
        };

        self.branches.insert(child_id.clone(), branch);
        self.children
            .entry(parent_id.to_string())
            .or_default()
            .push(child_id.clone());

        Ok(&self.branches[&child_id])
    }

    /// Update branch status.
    pub fn set_status(&mut self, task_id: &str, status: BranchStatus) {
        if let Some(branch) = self.branches.get_mut(task_id) {
            if matches!(
                status,
                BranchStatus::Completed
                    | BranchStatus::Failed { .. }
                    | BranchStatus::Cancelled { .. }
            ) {
                branch.completed_at = Some(current_timestamp());
            }
            branch.status = status;
        }
    }

    /// Set the merge contract for a completed branch.
    pub fn set_merge_contract(&mut self, task_id: &str, contract: MergeContract) {
        if let Some(branch) = self.branches.get_mut(task_id) {
            branch.merge_contract = Some(contract);
        }
    }

    /// Validate a merge contract: check changed-file manifests for conflicts.
    /// Uses sorted sets for O(Δ log Δ) validation.
    pub fn validate_merge(&self, task_id: &str) -> Result<MergeValidation, String> {
        let branch = self.branches.get(task_id).ok_or("Branch not found")?;

        let contract = branch.merge_contract.as_ref().ok_or("No merge contract")?;

        // Collect all files changed by sibling branches
        let siblings = branch
            .parent_id
            .as_ref()
            .and_then(|pid| self.children.get(pid))
            .map(|children| children.as_slice())
            .unwrap_or(&[]);

        let mut sibling_files: HashSet<String> = HashSet::new();
        for sibling_id in siblings {
            if sibling_id == task_id {
                continue;
            }
            if let Some(sib) = self.branches.get(sibling_id) {
                if let Some(ref sib_contract) = sib.merge_contract {
                    for file in &sib_contract.changed_files {
                        sibling_files.insert(file.path.clone());
                    }
                }
            }
        }

        // Check for conflicts
        let mut conflicting_files = Vec::new();
        for file in &contract.changed_files {
            if sibling_files.contains(&file.path) {
                conflicting_files.push(file.path.clone());
            }
        }

        Ok(MergeValidation {
            can_merge: conflicting_files.is_empty(),
            conflicting_files,
            verification_pending: contract.verification_obligations.clone(),
        })
    }

    /// Get a branch by task ID.
    pub fn get(&self, task_id: &str) -> Option<&ExecutionBranch> {
        self.branches.get(task_id)
    }

    /// Get all children of a task.
    pub fn children_of(&self, task_id: &str) -> Vec<&ExecutionBranch> {
        self.children
            .get(task_id)
            .map(|ids| ids.iter().filter_map(|id| self.branches.get(id)).collect())
            .unwrap_or_default()
    }

    /// Satisfy a verification obligation by presenting evidence.
    /// Removes the obligation from the contract if the evidence matches.
    /// Returns the number of obligations satisfied.
    pub fn satisfy_obligation(
        &mut self,
        task_id: &str,
        evidence: &ObligationEvidence,
    ) -> Result<usize, String> {
        let branch = self.branches.get_mut(task_id).ok_or("Branch not found")?;
        let contract = branch.merge_contract.as_mut().ok_or("No merge contract")?;
        let before = contract.verification_obligations.len();
        contract
            .verification_obligations
            .retain(|o| !o.satisfied_by(evidence));
        Ok(before - contract.verification_obligations.len())
    }

    /// Hard gate: determine if a branch may merge.
    /// merge_allowed = complete(contract) ∧ obligations_passed ∧ budget_ok ∧ lineage_consistent
    /// This is a predicate over contract completeness, not a UI hint.
    pub fn merge_allowed(&self, task_id: &str) -> Result<MergeDecision, String> {
        let branch = self.branches.get(task_id).ok_or("Branch not found")?;

        // 1. Branch must be Completed
        if !matches!(branch.status, BranchStatus::Completed) {
            return Ok(MergeDecision::Denied {
                reason: format!(
                    "Branch status is {:?}, must be Completed to merge",
                    branch.status
                ),
            });
        }

        // 2. Must have a merge contract
        let contract = match &branch.merge_contract {
            Some(c) => c,
            None => {
                return Ok(MergeDecision::Denied {
                    reason: "No merge contract set. Branch must produce a structured contract."
                        .into(),
                });
            }
        };

        // 3. Contract must self-report complete
        if !contract.self_reported_complete {
            return Ok(MergeDecision::Denied {
                reason: "Merge contract is not marked complete by the subagent.".into(),
            });
        }

        // 4. Verification obligations must be satisfied
        //    With typed obligations, an empty list means no obligations.
        //    Non-empty obligations are pending until evidence is presented.
        if !contract.verification_obligations.is_empty() {
            let pending: Vec<String> = contract
                .verification_obligations
                .iter()
                .map(|o| match o {
                    VerificationObligation::CommandMustPass { label, .. } => label.clone(),
                    VerificationObligation::FileUnmodified { path } => {
                        format!("file unchanged: {}", path)
                    }
                    VerificationObligation::ManualReview { description } => {
                        format!("review: {}", description)
                    }
                    VerificationObligation::Custom { description, .. } => description.clone(),
                })
                .collect();
            return Ok(MergeDecision::Denied {
                reason: format!("Verification obligations not met: {}", pending.join(", ")),
            });
        }

        // 5. Token budget not exhausted (if exhausted, output may be truncated)
        if branch.token_budget.is_exhausted() {
            return Ok(MergeDecision::Denied {
                reason: "Token budget exhausted — output may be incomplete.".into(),
            });
        }

        // 6. No sibling file conflicts
        let validation = self.validate_merge(task_id)?;
        if !validation.can_merge {
            return Ok(MergeDecision::Denied {
                reason: format!(
                    "File conflicts with sibling branches: {}",
                    validation.conflicting_files.join(", ")
                ),
            });
        }

        // 7. Rollback point must be present
        if contract.rollback_point.is_empty() {
            return Ok(MergeDecision::Denied {
                reason: "No rollback point specified.".into(),
            });
        }

        Ok(MergeDecision::Allowed {
            changed_files: contract.changed_files.len(),
            confidence: contract.confidence,
        })
    }

    /// Total number of branches.
    pub fn branch_count(&self) -> usize {
        self.branches.len()
    }
}

impl Default for LineageDAG {
    fn default() -> Self {
        Self::new()
    }
}

use std::collections::HashSet;

/// Result of merge validation.
#[derive(Debug, Clone)]
pub struct MergeValidation {
    pub can_merge: bool,
    pub conflicting_files: Vec<String>,
    pub verification_pending: Vec<VerificationObligation>,
}

/// Hard gate decision for merge.
#[derive(Debug, Clone)]
pub enum MergeDecision {
    /// Merge is allowed — all predicates pass.
    Allowed {
        changed_files: usize,
        confidence: f32,
    },
    /// Merge is denied — at least one predicate failed.
    Denied { reason: String },
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
    fn lineage_spawn_and_depth() {
        let mut dag = LineageDAG::new();
        dag.set_root("root".to_string(), "main task");

        dag.spawn(
            "root",
            "child-1".to_string(),
            "subtask 1",
            CapabilitySet::READ_ONLY,
            TokenBudget::new(10000, 5000),
            Duration::from_secs(60),
            vec!["read_file".to_string()],
            false,
        )
        .unwrap();

        let child = dag.get("child-1").unwrap();
        assert_eq!(child.depth, 1);
        assert_eq!(child.parent_id.as_deref(), Some("root"));
    }

    #[test]
    fn capability_inheritance_is_lattice_meet() {
        let mut dag = LineageDAG::new();
        dag.set_root("root".to_string(), "main");

        // Root has EDIT capabilities
        dag.branches.get_mut("root").unwrap().capabilities = CapabilitySet::EDIT;

        // Child requests FULL_AUTO — gets meet (intersection)
        dag.spawn(
            "root",
            "child".to_string(),
            "subtask",
            CapabilitySet::FULL_AUTO,
            TokenBudget::new(10000, 5000),
            Duration::from_secs(60),
            vec![],
            false,
        )
        .unwrap();

        let child = dag.get("child").unwrap();
        let child_caps = child.capabilities;
        // Should have FsRead and FsWrite (from EDIT) but not ProcessExecMutating (not in EDIT)
        assert!(child_caps.has(crate::capability::Capability::FsRead));
        assert!(child_caps.has(crate::capability::Capability::FsWrite));
        assert!(!child_caps.has(crate::capability::Capability::ProcessExecMutating));
    }

    #[test]
    fn merge_conflict_detection() {
        let mut dag = LineageDAG::new();
        dag.set_root("root".to_string(), "main");

        dag.spawn(
            "root",
            "a".to_string(),
            "task a",
            CapabilitySet::EDIT,
            TokenBudget::new(10000, 5000),
            Duration::from_secs(60),
            vec![],
            true,
        )
        .unwrap();
        dag.spawn(
            "root",
            "b".to_string(),
            "task b",
            CapabilitySet::EDIT,
            TokenBudget::new(10000, 5000),
            Duration::from_secs(60),
            vec![],
            true,
        )
        .unwrap();

        // Both change the same file
        dag.set_merge_contract(
            "a",
            MergeContract {
                changed_files: vec![ChangedFile {
                    path: "src/lib.rs".to_string(),
                    change_type: FileChangeType::Modified,
                    lines_added: 5,
                    lines_removed: 2,
                }],
                intent: "fix bug".to_string(),
                verification_obligations: vec![],
                rollback_point: "abc123".to_string(),
                self_reported_complete: true,
                confidence: 0.9,
                diff_summary: String::new(),
                branch_name: None,
            },
        );
        dag.set_merge_contract(
            "b",
            MergeContract {
                changed_files: vec![ChangedFile {
                    path: "src/lib.rs".to_string(),
                    change_type: FileChangeType::Modified,
                    lines_added: 3,
                    lines_removed: 1,
                }],
                intent: "add feature".to_string(),
                verification_obligations: vec![],
                rollback_point: "def456".to_string(),
                self_reported_complete: true,
                confidence: 0.8,
                diff_summary: String::new(),
                branch_name: None,
            },
        );

        let validation = dag.validate_merge("b").unwrap();
        assert!(!validation.can_merge);
        assert!(
            validation
                .conflicting_files
                .contains(&"src/lib.rs".to_string())
        );
    }
}
