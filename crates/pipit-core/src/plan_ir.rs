//! Typed Plan IR + Transaction Gate
//!
//! Replaces string-protocol plan markers with a typed intermediate
//! representation. Plans are DAGs of actions with dependencies,
//! capabilities, cost estimates, and verification predicates.
//!
//! State machine: Draft → Reviewed → Approved → Executing → Verified → Done
//! Validation cost: O(|V|+|E|) over the action DAG.

use crate::capability::CapabilitySet;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Plan IR ────────────────────────────────────────────────────────────

/// A typed plan: a DAG of actions with dependencies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanIR {
    /// Unique plan identifier.
    pub id: String,
    /// Human-readable summary of the plan's objective.
    pub objective: String,
    /// Ordered phases. Each phase contains steps that may run in parallel.
    pub phases: Vec<PlanPhase>,
    /// Current state in the transaction lifecycle.
    pub state: PlanState,
    /// Total estimated cost (tokens).
    pub estimated_tokens: u64,
    /// Total estimated time (seconds).
    pub estimated_seconds: u32,
    /// Required capabilities to execute this plan.
    pub required_capabilities: CapabilitySet,
    /// Rollback strategy.
    pub rollback: RollbackStrategy,
    /// Plan provenance.
    pub source: PlanProvenance,
    /// History of state transitions.
    pub transitions: Vec<StateTransition>,
}

/// A phase groups related steps that share a logical boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanPhase {
    pub name: String,
    pub steps: Vec<PlanStep>,
    /// Steps within a phase may declare dependencies on each other.
    pub dependency_edges: Vec<(usize, usize)>,
}

/// An individual action in the plan DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: usize,
    pub action: PlanAction,
    pub description: String,
    /// Capabilities this step requires.
    pub required_capabilities: CapabilitySet,
    /// Estimated token cost for this step.
    pub estimated_tokens: u64,
    /// Verification predicate to check after this step.
    pub verification: Option<VerificationPredicate>,
    /// Current execution status.
    pub status: StepStatus,
}

/// Concrete action types in the plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlanAction {
    /// Read files to understand context.
    ReadFiles { paths: Vec<String> },
    /// Edit a file with a description of the change.
    EditFile { path: String, change_description: String },
    /// Create a new file.
    CreateFile { path: String, purpose: String },
    /// Delete a file.
    DeleteFile { path: String },
    /// Run a shell command.
    RunCommand { command: String, purpose: String },
    /// Run tests.
    RunTests { scope: String },
    /// Run linter or formatter.
    Lint { scope: String },
    /// Delegate to a subagent.
    Delegate { description: String, constraints: String },
    /// Custom action (escape hatch for LLM-generated plans).
    Custom { tool: String, args_description: String },
}

/// How to verify a step succeeded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationPredicate {
    pub kind: VerificationKind,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VerificationKind {
    /// Run a command and check exit code.
    CommandSucceeds { command: String },
    /// Check that a file exists.
    FileExists { path: String },
    /// Check that a file contains a pattern.
    FileContains { path: String, pattern: String },
    /// Check that tests pass.
    TestsPass,
    /// Check that the build succeeds.
    BuildSucceeds,
    /// Manual review by the user.
    ManualReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
}

// ─── Transaction State Machine ──────────────────────────────────────────

/// Plan lifecycle: Draft → Reviewed → Approved → Executing → Verified → Done
///
/// Transitions are monotone: you can only move forward or abort.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanState {
    /// Plan has been generated but not reviewed.
    Draft,
    /// User has reviewed the plan (may have edited steps).
    Reviewed,
    /// User has approved the plan for execution.
    Approved,
    /// Plan is currently being executed.
    Executing,
    /// All steps completed, verification in progress.
    Verifying,
    /// Plan completed successfully.
    Done,
    /// Plan was aborted or rolled back.
    Aborted,
}

/// Record of a state transition for audit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateTransition {
    pub from: PlanState,
    pub to: PlanState,
    pub timestamp_ms: u64,
    pub reason: String,
    /// Who initiated the transition (user, agent, policy).
    pub actor: String,
}

/// How to recover from plan failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RollbackStrategy {
    /// No rollback — trust verification to catch issues.
    None,
    /// Git-based rollback to pre-plan commit.
    GitRevert,
    /// File-level undo via history.
    FileHistory,
    /// Custom rollback command.
    Command { command: String },
}

/// Where the plan came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanProvenance {
    /// Generated by the heuristic planner.
    Heuristic,
    /// Generated by the LLM planner.
    LlmGenerated,
    /// Provided by the user via /plan command.
    UserDefined,
    /// Loaded from a saved plan file.
    Restored,
}

// ─── Plan Operations ────────────────────────────────────────────────────

impl PlanIR {
    /// Create a new draft plan.
    pub fn new(objective: &str, source: PlanProvenance) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            objective: objective.to_string(),
            phases: Vec::new(),
            state: PlanState::Draft,
            estimated_tokens: 0,
            estimated_seconds: 0,
            required_capabilities: CapabilitySet::EMPTY,
            rollback: RollbackStrategy::GitRevert,
            source,
            transitions: Vec::new(),
        }
    }

    /// Add a phase to the plan.
    pub fn add_phase(&mut self, name: &str, steps: Vec<PlanStep>) {
        // Accumulate cost estimates
        for step in &steps {
            self.estimated_tokens += step.estimated_tokens;
            self.required_capabilities = self
                .required_capabilities
                .join(step.required_capabilities);
        }
        self.phases.push(PlanPhase {
            name: name.to_string(),
            steps,
            dependency_edges: Vec::new(),
        });
    }

    /// Transition to a new state. Returns Err if the transition is invalid.
    pub fn transition(&mut self, to: PlanState, reason: &str, actor: &str) -> Result<(), String> {
        if !self.is_valid_transition(to) {
            return Err(format!(
                "invalid plan transition: {:?} → {:?}",
                self.state, to
            ));
        }

        self.transitions.push(StateTransition {
            from: self.state,
            to,
            timestamp_ms: crate::scoped_capability::now_ms(),
            reason: reason.to_string(),
            actor: actor.to_string(),
        });
        self.state = to;
        Ok(())
    }

    fn is_valid_transition(&self, to: PlanState) -> bool {
        matches!(
            (self.state, to),
            (PlanState::Draft, PlanState::Reviewed)
                | (PlanState::Draft, PlanState::Approved)   // auto-approve for simple plans
                | (PlanState::Reviewed, PlanState::Approved)
                | (PlanState::Approved, PlanState::Executing)
                | (PlanState::Executing, PlanState::Verifying)
                | (PlanState::Executing, PlanState::Aborted)
                | (PlanState::Verifying, PlanState::Done)
                | (PlanState::Verifying, PlanState::Aborted)
                | (_, PlanState::Aborted) // Can abort from any state
        )
    }

    /// Validate the plan DAG: check for cycles and missing dependencies.
    /// Cost: O(|V| + |E|) topological sort.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        for (pi, phase) in self.phases.iter().enumerate() {
            let n = phase.steps.len();

            // Check dependency edges reference valid step indices
            for &(from, to) in &phase.dependency_edges {
                if from >= n || to >= n {
                    errors.push(format!(
                        "phase '{}': dependency edge ({}, {}) references invalid step index (max {})",
                        phase.name, from, to, n - 1
                    ));
                }
                if from == to {
                    errors.push(format!(
                        "phase '{}': self-dependency at step {}",
                        phase.name, from
                    ));
                }
            }

            // Simple cycle detection via topological sort
            if n > 0 && !phase.dependency_edges.is_empty() {
                let mut in_degree = vec![0u32; n];
                let mut adj = vec![vec![]; n];
                for &(from, to) in &phase.dependency_edges {
                    if from < n && to < n {
                        adj[from].push(to);
                        in_degree[to] += 1;
                    }
                }
                let mut queue: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
                let mut visited = 0;
                while let Some(node) = queue.pop() {
                    visited += 1;
                    for &next in &adj[node] {
                        in_degree[next] -= 1;
                        if in_degree[next] == 0 {
                            queue.push(next);
                        }
                    }
                }
                if visited < n {
                    errors.push(format!(
                        "phase '{}': dependency cycle detected",
                        phase.name
                    ));
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Get the total number of steps across all phases.
    pub fn step_count(&self) -> usize {
        self.phases.iter().map(|p| p.steps.len()).sum()
    }

    /// Get a summary suitable for displaying to the user.
    pub fn summary(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!("Plan: {} [{:?}]", self.objective, self.state));
        lines.push(format!(
            "  Est. tokens: {} | Steps: {} | Phases: {}",
            self.estimated_tokens,
            self.step_count(),
            self.phases.len()
        ));
        for phase in &self.phases {
            lines.push(format!("  Phase: {}", phase.name));
            for step in &phase.steps {
                let status = match step.status {
                    StepStatus::Pending => "○",
                    StepStatus::Running => "◎",
                    StepStatus::Succeeded => "●",
                    StepStatus::Failed => "✗",
                    StepStatus::Skipped => "−",
                };
                lines.push(format!("    {} {}", status, step.description));
            }
        }
        lines.join("\n")
    }
}

// Expose now_ms for use from scoped_capability
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
