//! # Speculative Branched Execution (B4)
//!
//! Fork-and-join speculative execution for the planning layer.
//! When the planner identifies multiple candidate strategies, this module
//! executes them speculatively on filesystem branches (using git worktrees
//! or copy-on-write snapshots) and picks the winner based on verification scores.
//!
//! ## Architecture
//!
//! ```text
//! PlanCandidates [A, B, C]
//!   → SpeculativeExecutor::fork()
//!     → Branch A (worktree-a/) → execute → score
//!     → Branch B (worktree-b/) → execute → score
//!     → Branch C (worktree-c/) → execute → score
//!   → join() → pick max(score) → merge winner
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A speculative execution branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecBranch {
    pub id: String,
    pub strategy: String,
    pub status: BranchStatus,
    pub score: f64,
    pub workspace: Option<PathBuf>,
    pub artifacts: Vec<String>,
    pub turn_count: u32,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BranchStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Configuration for speculative execution.
#[derive(Debug, Clone)]
pub struct SpecConfig {
    /// Maximum number of concurrent branches.
    pub max_branches: usize,
    /// Maximum turns per branch before cancellation.
    pub max_turns_per_branch: u32,
    /// Maximum total cost (USD) across all branches.
    pub max_total_cost: f64,
    /// If true, cancel all other branches once one completes with score > threshold.
    pub early_termination: bool,
    /// Score threshold for early termination.
    pub early_termination_threshold: f64,
}

impl Default for SpecConfig {
    fn default() -> Self {
        Self {
            max_branches: 3,
            max_turns_per_branch: 10,
            max_total_cost: 1.0,
            early_termination: true,
            early_termination_threshold: 0.8,
        }
    }
}

/// Result of a speculative execution round.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecResult {
    pub winner: Option<SpecBranch>,
    pub branches: Vec<SpecBranch>,
    pub total_cost: f64,
    pub total_turns: u32,
}

/// The speculative executor.
pub struct SpeculativeExecutor {
    config: SpecConfig,
    branches: Vec<SpecBranch>,
    next_id: u32,
}

impl SpeculativeExecutor {
    pub fn new(config: SpecConfig) -> Self {
        Self {
            config,
            branches: Vec::new(),
            next_id: 0,
        }
    }

    /// Register a candidate strategy for speculative execution.
    pub fn register_candidate(&mut self, strategy: &str) -> Result<String, String> {
        if self.branches.len() >= self.config.max_branches {
            return Err(format!(
                "max branches ({}) reached",
                self.config.max_branches
            ));
        }

        self.next_id += 1;
        let id = format!("spec-{}", self.next_id);
        self.branches.push(SpecBranch {
            id: id.clone(),
            strategy: strategy.to_string(),
            status: BranchStatus::Pending,
            score: 0.0,
            workspace: None,
            artifacts: Vec::new(),
            turn_count: 0,
            cost_usd: 0.0,
        });

        Ok(id)
    }

    /// Mark a branch as running with its workspace path.
    pub fn start_branch(&mut self, id: &str, workspace: PathBuf) -> Result<(), String> {
        let branch = self
            .branches
            .iter_mut()
            .find(|b| b.id == id)
            .ok_or_else(|| format!("branch {} not found", id))?;
        branch.status = BranchStatus::Running;
        branch.workspace = Some(workspace);
        Ok(())
    }

    /// Record a turn completion for a branch and update its score.
    pub fn record_turn(
        &mut self,
        id: &str,
        score: f64,
        cost: f64,
    ) -> Result<TurnAction, String> {
        let idx = self
            .branches
            .iter()
            .position(|b| b.id == id)
            .ok_or_else(|| format!("branch {} not found", id))?;

        self.branches[idx].turn_count += 1;
        self.branches[idx].score = score;
        self.branches[idx].cost_usd += cost;

        // Check turn budget
        if self.branches[idx].turn_count >= self.config.max_turns_per_branch {
            self.branches[idx].status = BranchStatus::Completed;
            return Ok(TurnAction::Complete);
        }

        // Check cost budget
        let total_cost: f64 = self.branches.iter().map(|b| b.cost_usd).sum();
        if total_cost >= self.config.max_total_cost {
            self.branches[idx].status = BranchStatus::Completed;
            return Ok(TurnAction::BudgetExhausted);
        }

        // Check early termination
        if self.config.early_termination
            && score >= self.config.early_termination_threshold
        {
            self.branches[idx].status = BranchStatus::Completed;
            return Ok(TurnAction::EarlyWin);
        }

        Ok(TurnAction::Continue)
    }

    /// Complete a branch (either success or failure).
    pub fn complete_branch(&mut self, id: &str, success: bool, score: f64) {
        if let Some(branch) = self.branches.iter_mut().find(|b| b.id == id) {
            branch.status = if success {
                BranchStatus::Completed
            } else {
                BranchStatus::Failed
            };
            branch.score = score;
        }
    }

    /// Cancel remaining branches (e.g., after early win).
    pub fn cancel_remaining(&mut self) {
        for branch in &mut self.branches {
            if branch.status == BranchStatus::Pending || branch.status == BranchStatus::Running {
                branch.status = BranchStatus::Cancelled;
            }
        }
    }

    /// Pick the winner (highest-scoring completed branch).
    pub fn pick_winner(&self) -> Option<&SpecBranch> {
        self.branches
            .iter()
            .filter(|b| b.status == BranchStatus::Completed)
            .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal))
    }

    /// Produce the final result.
    pub fn result(&self) -> SpecResult {
        SpecResult {
            winner: self.pick_winner().cloned(),
            branches: self.branches.clone(),
            total_cost: self.branches.iter().map(|b| b.cost_usd).sum(),
            total_turns: self.branches.iter().map(|b| b.turn_count).sum(),
        }
    }

    /// Check if all branches are done.
    pub fn all_done(&self) -> bool {
        self.branches.iter().all(|b| {
            matches!(
                b.status,
                BranchStatus::Completed | BranchStatus::Failed | BranchStatus::Cancelled
            )
        })
    }

    /// Get branch count.
    pub fn branch_count(&self) -> usize {
        self.branches.len()
    }
}

/// Action for the turn executor after recording a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnAction {
    Continue,
    Complete,
    EarlyWin,
    BudgetExhausted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_run_branches() {
        let mut exec = SpeculativeExecutor::new(SpecConfig::default());
        let id1 = exec.register_candidate("minimal_patch").unwrap();
        let id2 = exec.register_candidate("root_cause_repair").unwrap();

        exec.start_branch(&id1, PathBuf::from("/tmp/spec-1")).unwrap();
        exec.start_branch(&id2, PathBuf::from("/tmp/spec-2")).unwrap();

        assert_eq!(exec.branch_count(), 2);
    }

    #[test]
    fn max_branches_enforced() {
        let config = SpecConfig {
            max_branches: 2,
            ..Default::default()
        };
        let mut exec = SpeculativeExecutor::new(config);
        exec.register_candidate("a").unwrap();
        exec.register_candidate("b").unwrap();
        assert!(exec.register_candidate("c").is_err());
    }

    #[test]
    fn early_termination_on_high_score() {
        let mut exec = SpeculativeExecutor::new(SpecConfig {
            early_termination: true,
            early_termination_threshold: 0.8,
            ..Default::default()
        });

        let id = exec.register_candidate("strategy_a").unwrap();
        exec.start_branch(&id, PathBuf::from("/tmp/a")).unwrap();

        let action = exec.record_turn(&id, 0.9, 0.01).unwrap();
        assert_eq!(action, TurnAction::EarlyWin);
    }

    #[test]
    fn pick_highest_scoring_winner() {
        let mut exec = SpeculativeExecutor::new(SpecConfig::default());

        let id1 = exec.register_candidate("a").unwrap();
        let id2 = exec.register_candidate("b").unwrap();

        exec.start_branch(&id1, PathBuf::from("/tmp/a")).unwrap();
        exec.start_branch(&id2, PathBuf::from("/tmp/b")).unwrap();

        exec.complete_branch(&id1, true, 0.6);
        exec.complete_branch(&id2, true, 0.85);

        let winner = exec.pick_winner().unwrap();
        assert_eq!(winner.id, id2);
        assert_eq!(winner.score, 0.85);
    }

    #[test]
    fn cancel_remaining_after_early_win() {
        let mut exec = SpeculativeExecutor::new(SpecConfig::default());
        let id1 = exec.register_candidate("a").unwrap();
        let id2 = exec.register_candidate("b").unwrap();

        exec.start_branch(&id1, PathBuf::from("/tmp/a")).unwrap();
        exec.complete_branch(&id1, true, 0.95);
        exec.cancel_remaining();

        let result = exec.result();
        assert_eq!(
            result.branches.iter().filter(|b| b.status == BranchStatus::Cancelled).count(),
            1
        );
    }

    #[test]
    fn budget_exhaustion() {
        let mut exec = SpeculativeExecutor::new(SpecConfig {
            max_total_cost: 0.05,
            early_termination: false,
            ..Default::default()
        });

        let id = exec.register_candidate("expensive").unwrap();
        exec.start_branch(&id, PathBuf::from("/tmp/e")).unwrap();

        exec.record_turn(&id, 0.3, 0.02).unwrap();
        exec.record_turn(&id, 0.4, 0.02).unwrap();
        let action = exec.record_turn(&id, 0.5, 0.02).unwrap();
        assert_eq!(action, TurnAction::BudgetExhausted);
    }

    #[test]
    fn all_done_check() {
        let mut exec = SpeculativeExecutor::new(SpecConfig::default());
        let id = exec.register_candidate("a").unwrap();
        assert!(!exec.all_done());

        exec.complete_branch(&id, true, 1.0);
        assert!(exec.all_done());
    }
}
