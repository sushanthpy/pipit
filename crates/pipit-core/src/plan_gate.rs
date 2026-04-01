//! Plan Gate — First-Class Plan / Execute / Verify UX
//!
//! Makes planning and verification explicit user-visible phases.
//! For non-trivial tasks (multi-file, large changes, ambiguous scope),
//! the agent presents a plan and waits for approval before executing.
//!
//! Expected cost minimization: if ambiguity probability is p and rework
//! cost is R, planning is worthwhile when C_plan < pR. For multi-file
//! changes, R grows superlinearly (rollback + retest + context drift).

use serde::{Deserialize, Serialize};

/// Whether a task should require an explicit plan gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanGateDecision {
    /// Skip planning — task is simple enough for direct execution.
    SkipPlan,
    /// Require plan review — task is complex or ambiguous.
    RequirePlan,
    /// Suggest plan but auto-approve if user doesn't intervene.
    SuggestPlan,
}

/// Complexity signals used to decide whether to gate on a plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskComplexitySignals {
    /// Estimated number of files to modify.
    pub estimated_files: u32,
    /// Whether the task involves multiple directories.
    pub multi_directory: bool,
    /// Whether the task involves test changes.
    pub involves_tests: bool,
    /// Whether the task is ambiguous (multiple interpretations).
    pub ambiguity_score: f32,
    /// Whether the task involves config or schema changes.
    pub config_change: bool,
    /// Whether external dependencies might change.
    pub dependency_change: bool,
    /// Word count of the user's request (proxy for complexity).
    pub request_word_count: u32,
}

impl TaskComplexitySignals {
    /// Analyze a user prompt to extract complexity signals.
    pub fn from_prompt(prompt: &str) -> Self {
        let words: Vec<&str> = prompt.split_whitespace().collect();
        let lower = prompt.to_lowercase();

        Self {
            estimated_files: estimate_file_count(&lower),
            multi_directory: lower.contains("across") || lower.contains("multiple")
                || lower.contains("all files") || lower.contains("refactor"),
            involves_tests: lower.contains("test") || lower.contains("spec"),
            ambiguity_score: estimate_ambiguity(&lower, words.len()),
            config_change: lower.contains("config") || lower.contains("schema")
                || lower.contains("migration") || lower.contains("dependency"),
            dependency_change: lower.contains("upgrade") || lower.contains("dependency")
                || lower.contains("package") || lower.contains("install"),
            request_word_count: words.len() as u32,
        }
    }

    /// Compute a complexity score (0.0–1.0).
    pub fn complexity_score(&self) -> f32 {
        let mut score = 0.0f32;

        // File count contributes most
        score += match self.estimated_files {
            0..=1 => 0.0,
            2..=3 => 0.2,
            4..=5 => 0.4,
            _ => 0.6,
        };

        if self.multi_directory { score += 0.15; }
        if self.involves_tests { score += 0.05; }
        if self.config_change { score += 0.1; }
        if self.dependency_change { score += 0.1; }
        score += self.ambiguity_score * 0.2;

        score.min(1.0)
    }
}

/// Decide whether to require a plan gate based on complexity signals.
pub fn decide_plan_gate(
    signals: &TaskComplexitySignals,
    plan_cost_tokens: u64,
    user_preference: Option<PlanGateDecision>,
) -> PlanGateDecision {
    // User override takes precedence
    if let Some(pref) = user_preference {
        return pref;
    }

    let complexity = signals.complexity_score();

    // Expected cost model: plan when C_plan < p * R
    // p = complexity score, R = rework cost (increases with file count)
    let estimated_rework = (signals.estimated_files as f64) * 500.0; // tokens per file rework
    let expected_penalty = complexity as f64 * estimated_rework;
    let plan_cost = plan_cost_tokens as f64;

    if complexity >= 0.5 || expected_penalty > plan_cost * 3.0 {
        PlanGateDecision::RequirePlan
    } else if complexity >= 0.25 || signals.multi_directory {
        PlanGateDecision::SuggestPlan
    } else {
        PlanGateDecision::SkipPlan
    }
}

/// A structured plan presented to the user for review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    /// High-level summary of what the agent will do.
    pub summary: String,
    /// Ordered steps the agent plans to take.
    pub steps: Vec<PlanStep>,
    /// Files the agent expects to modify.
    pub target_files: Vec<String>,
    /// Verification strategy (how success will be confirmed).
    pub verification: String,
    /// Estimated token cost for execution.
    pub estimated_cost: u64,
    /// Confidence in the plan (0.0–1.0).
    pub confidence: f32,
}

/// A single step in the execution plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub number: u32,
    pub description: String,
    pub tool_hint: Option<String>,
    pub target_file: Option<String>,
}

/// User response to a plan gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanResponse {
    /// Approve the plan as-is.
    Approve,
    /// Approve with modifications/guidance.
    ApproveWithGuidance(String),
    /// Reject the plan — agent should try a different approach.
    Reject,
    /// Cancel the task entirely.
    Cancel,
}

fn estimate_file_count(text: &str) -> u32 {
    let mut count = 0u32;
    if text.contains("all files") || text.contains("across the project") { count += 5; }
    if text.contains("refactor") { count += 3; }
    if text.contains("rename") { count += 2; }
    // Count explicit file references
    count += text.matches(".rs").count() as u32;
    count += text.matches(".py").count() as u32;
    count += text.matches(".ts").count() as u32;
    count += text.matches(".js").count() as u32;
    count += text.matches(".toml").count() as u32;
    count.max(1) // at least 1
}

fn estimate_ambiguity(text: &str, word_count: usize) -> f32 {
    let mut score = 0.0f32;
    // Short requests might be too vague
    if word_count < 5 { score += 0.3; }
    // Questions embedded in task
    if text.contains('?') { score += 0.2; }
    // "maybe", "or", "either" signal uncertainty
    if text.contains("maybe") || text.contains("or ") || text.contains("either") { score += 0.2; }
    // Very long requests may be ambiguous
    if word_count > 100 { score += 0.1; }
    score.min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_task_skips_plan() {
        let signals = TaskComplexitySignals::from_prompt("fix the typo in main.rs");
        let decision = decide_plan_gate(&signals, 500, None);
        assert_eq!(decision, PlanGateDecision::SkipPlan);
    }

    #[test]
    fn complex_task_requires_plan() {
        let signals = TaskComplexitySignals::from_prompt(
            "refactor all files across the project to use the new API pattern"
        );
        let decision = decide_plan_gate(&signals, 500, None);
        assert!(matches!(decision, PlanGateDecision::RequirePlan | PlanGateDecision::SuggestPlan));
    }

    #[test]
    fn user_override_takes_precedence() {
        let signals = TaskComplexitySignals::from_prompt("complex multi-file refactor");
        let decision = decide_plan_gate(&signals, 500, Some(PlanGateDecision::SkipPlan));
        assert_eq!(decision, PlanGateDecision::SkipPlan);
    }

    #[test]
    fn complexity_scoring() {
        let simple = TaskComplexitySignals::from_prompt("fix bug in foo.rs");
        let complex = TaskComplexitySignals::from_prompt(
            "refactor all config files across multiple directories and update tests"
        );
        assert!(complex.complexity_score() > simple.complexity_score());
    }
}
