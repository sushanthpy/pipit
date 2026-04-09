//! # Candidate Action Scoring (Architecture Task 4)
//!
//! Enumerates legal actions for the current state and attaches mechanical
//! axis scores. The LLM does the final ranking — the engine reduces entropy
//! without embedding a shadow planner.
//!
//! Candidate enumeration: O(A) in legal actions.
//! Axis scoring: O(A·K) where K = scoring axes.

use serde::{Deserialize, Serialize};

// ═══════════════════════════════════════════════════════════════
//  CANDIDATE ACTIONS
// ═══════════════════════════════════════════════════════════════

/// A candidate action the LLM may choose.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateAction {
    /// Unique identifier for this candidate.
    pub id: String,
    /// Human-readable description.
    pub description: String,
    /// The action category.
    pub kind: ActionKind,
    /// Mechanical axis scores (engine-computed, not LLM-ranked).
    pub scores: AxisScores,
    /// Whether all preconditions are satisfied.
    pub preconditions_met: bool,
    /// Preconditions that are NOT met (if any).
    pub unmet_preconditions: Vec<String>,
}

/// Categories of actions the agent can take.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActionKind {
    /// Read a file for context.
    ReadFile { path: String },
    /// Edit a file.
    EditFile { path: String },
    /// Execute a shell command.
    Execute { command: String },
    /// Run verification (build/lint/test).
    Verify { checks: Vec<String> },
    /// Commit changes.
    Commit { message: String },
    /// Delegate to a subagent.
    Delegate { task: String },
    /// Compress context.
    CompactContext,
    /// Request plan review.
    ReviewPlan,
    /// Propose promotion.
    Promote { target: String },
    /// Custom/tool-specific action.
    Custom {
        tool_name: String,
        description: String,
    },
}

/// Mechanical axis scores — computed by the engine, not ranked by it.
/// Each score is in [0.0, 1.0].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AxisScores {
    /// Are all preconditions met? 1.0 = yes, 0.0 = missing critical preconditions.
    pub precondition_satisfaction: f32,
    /// How reversible is this action? 1.0 = fully reversible, 0.0 = irreversible.
    pub reversibility: f32,
    /// How many files/systems are affected? 1.0 = minimal blast radius, 0.0 = wide.
    pub blast_radius: f32,
    /// Expected latency. 1.0 = instant, 0.0 = very slow.
    pub latency: f32,
    /// Policy risk. 1.0 = no policy concerns, 0.0 = likely to be blocked.
    pub policy_safety: f32,
    /// How much evidence supports this action? 1.0 = strong evidence, 0.0 = speculative.
    pub evidence_coverage: f32,
}

impl Default for AxisScores {
    fn default() -> Self {
        Self {
            precondition_satisfaction: 1.0,
            reversibility: 1.0,
            blast_radius: 1.0,
            latency: 1.0,
            policy_safety: 1.0,
            evidence_coverage: 0.5,
        }
    }
}

impl AxisScores {
    /// Compute a weighted composite score (for sorting/display, NOT for ranking).
    pub fn composite(&self) -> f32 {
        // Weights reflect relative importance for safety
        self.precondition_satisfaction * 0.25
            + self.reversibility * 0.15
            + self.blast_radius * 0.15
            + self.latency * 0.10
            + self.policy_safety * 0.20
            + self.evidence_coverage * 0.15
    }
}

// ═══════════════════════════════════════════════════════════════
//  SCORING FUNCTIONS
// ═══════════════════════════════════════════════════════════════

/// Score a read action.
pub fn score_read(path: &str) -> AxisScores {
    let _ = path;
    AxisScores {
        precondition_satisfaction: 1.0,
        reversibility: 1.0,     // Reads are fully reversible
        blast_radius: 1.0,      // No side effects
        latency: 0.9,           // Usually fast
        policy_safety: 1.0,     // Reads are always safe
        evidence_coverage: 0.3, // Gathering evidence, not acting on it
    }
}

/// Score an edit action.
pub fn score_edit(path: &str, files_modified_so_far: usize) -> AxisScores {
    let _ = path;
    let blast = if files_modified_so_far < 3 { 0.9 } else { 0.6 };
    AxisScores {
        precondition_satisfaction: 1.0,
        reversibility: 0.9, // Edits can be undone via VCS
        blast_radius: blast,
        latency: 0.85,
        policy_safety: 0.9, // May need approval
        evidence_coverage: 0.5,
    }
}

/// Score a shell command execution.
pub fn score_execute(command: &str) -> AxisScores {
    let is_readonly = command.starts_with("cat ")
        || command.starts_with("ls ")
        || command.starts_with("grep ")
        || command.starts_with("find ");
    let is_build = command.starts_with("cargo build")
        || command.starts_with("npm run")
        || command.starts_with("make");

    AxisScores {
        precondition_satisfaction: 1.0,
        reversibility: if is_readonly { 1.0 } else { 0.4 },
        blast_radius: if is_readonly {
            1.0
        } else if is_build {
            0.7
        } else {
            0.5
        },
        latency: if is_build { 0.3 } else { 0.7 },
        policy_safety: if is_readonly { 1.0 } else { 0.6 },
        evidence_coverage: if is_build { 0.7 } else { 0.4 },
    }
}

/// Score a verification action.
pub fn score_verify() -> AxisScores {
    AxisScores {
        precondition_satisfaction: 1.0,
        reversibility: 1.0, // Verification is read-only
        blast_radius: 1.0,
        latency: 0.4, // Tests can be slow
        policy_safety: 1.0,
        evidence_coverage: 0.9, // High evidence value
    }
}

/// Score a delegation action.
pub fn score_delegate() -> AxisScores {
    AxisScores {
        precondition_satisfaction: 0.8, // Needs capability matching
        reversibility: 0.7,             // Subagent work can be rolled back
        blast_radius: 0.6,              // Subagent may touch many files
        latency: 0.2,                   // Spawning and running is slow
        policy_safety: 0.8,
        evidence_coverage: 0.6,
    }
}

/// Enumerate candidate actions for the current state.
/// The engine provides scored candidates; the LLM chooses.
pub fn enumerate_candidates(
    modified_files: &[String],
    has_uncommitted: bool,
    has_active_subagents: bool,
    verification_pending: bool,
) -> Vec<CandidateAction> {
    let mut candidates = Vec::new();

    // Always available: read file
    candidates.push(CandidateAction {
        id: "read".into(),
        description: "Read a file for additional context".into(),
        kind: ActionKind::ReadFile {
            path: String::new(),
        },
        scores: score_read(""),
        preconditions_met: true,
        unmet_preconditions: Vec::new(),
    });

    // Edit file
    candidates.push(CandidateAction {
        id: "edit".into(),
        description: "Edit a file to implement changes".into(),
        kind: ActionKind::EditFile {
            path: String::new(),
        },
        scores: score_edit("", modified_files.len()),
        preconditions_met: true,
        unmet_preconditions: Vec::new(),
    });

    // Verify if there are modifications
    if !modified_files.is_empty() || verification_pending {
        candidates.push(CandidateAction {
            id: "verify".into(),
            description: "Run verification (build/lint/test)".into(),
            kind: ActionKind::Verify {
                checks: vec!["build".into(), "test".into()],
            },
            scores: score_verify(),
            preconditions_met: true,
            unmet_preconditions: Vec::new(),
        });
    }

    // Commit if there are uncommitted changes
    if has_uncommitted {
        candidates.push(CandidateAction {
            id: "commit".into(),
            description: "Commit current changes".into(),
            kind: ActionKind::Commit {
                message: String::new(),
            },
            scores: AxisScores {
                precondition_satisfaction: 1.0,
                reversibility: 0.8,
                blast_radius: 0.9,
                latency: 0.9,
                policy_safety: 0.9,
                evidence_coverage: 0.7,
            },
            preconditions_met: true,
            unmet_preconditions: Vec::new(),
        });
    }

    // Delegate (if no active subagents)
    if !has_active_subagents {
        candidates.push(CandidateAction {
            id: "delegate".into(),
            description: "Delegate a sub-task to a specialist agent".into(),
            kind: ActionKind::Delegate {
                task: String::new(),
            },
            scores: score_delegate(),
            preconditions_met: true,
            unmet_preconditions: Vec::new(),
        });
    }

    candidates
}

/// Format candidates for LLM prompt injection.
pub fn format_candidates_for_prompt(candidates: &[CandidateAction]) -> String {
    let mut out = String::from("Available actions (with mechanical scores):\n");
    for c in candidates {
        out.push_str(&format!(
            "- {} [composite={:.2}, precond={:.1}, reversible={:.1}, blast={:.1}, policy={:.1}]",
            c.description,
            c.scores.composite(),
            c.scores.precondition_satisfaction,
            c.scores.reversibility,
            c.scores.blast_radius,
            c.scores.policy_safety,
        ));
        if !c.preconditions_met {
            out.push_str(&format!(" ⚠ unmet: {}", c.unmet_preconditions.join(", ")));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_scores_composite_in_range() {
        let scores = AxisScores::default();
        let c = scores.composite();
        assert!(c >= 0.0 && c <= 1.0);
    }

    #[test]
    fn enumerate_includes_verify_when_modified() {
        let candidates = enumerate_candidates(&["src/main.rs".into()], true, false, false);
        assert!(candidates.iter().any(|c| c.id == "verify"));
        assert!(candidates.iter().any(|c| c.id == "commit"));
    }

    #[test]
    fn read_scores_higher_safety_than_execute() {
        let read = score_read("file.rs");
        let exec = score_execute("rm -rf /tmp/test");
        assert!(read.policy_safety > exec.policy_safety);
        assert!(read.reversibility > exec.reversibility);
    }
}
