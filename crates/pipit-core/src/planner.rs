use crate::proof::{
    ConfidenceReport, EvidenceArtifact, Objective, VerificationKind, VerificationStep,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StrategyKind {
    MinimalPatch,
    RootCauseRepair,
    ArchitecturalRepair,
    DiagnosticOnly,
    CharacterizationFirst,
}

/// Provenance of a plan — distinguishes heuristic from LLM-generated plans.
///
/// # Implementation Tier
/// Tier 2: type-level encoding of plan provenance for proof packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanSource {
    /// Keyword heuristic over objective text + evidence pattern matching.
    Heuristic,
    /// Structured JSON response from an LLM planner role.
    LlmStructured,
    /// User-provided plan (from /plan command or conventions).
    UserSpecified,
}

/// Provenance of a verification verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationSource {
    /// Weighted average of evidence artifact pass rates.
    Heuristic,
    /// Structured JSON verdict from an LLM verifier role.
    LlmStructured,
    /// No verification performed (Fast mode).
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidatePlan {
    pub strategy: StrategyKind,
    pub rationale: String,
    pub expected_value: f32,
    pub estimated_cost: f32,
    pub verification_plan: Vec<VerificationStep>,
    #[serde(default = "default_plan_source")]
    pub plan_source: PlanSource,
}

fn default_plan_source() -> PlanSource {
    PlanSource::Heuristic
}

// ═══════════════════════════════════════════════════════════════════════════
//  Strategy traits — polymorphic planner / verifier dispatch
// ═══════════════════════════════════════════════════════════════════════════

/// Trait for plan generation strategy. Implemented by heuristic, LLM, and null planners.
pub trait PlanStrategy: Send + Sync {
    fn candidate_plans(
        &self,
        objective: &Objective,
        confidence: &ConfidenceReport,
        evidence: &[EvidenceArtifact],
    ) -> Vec<CandidatePlan>;

    fn select_plan(
        &self,
        objective: &Objective,
        confidence: &ConfidenceReport,
        evidence: &[EvidenceArtifact],
    ) -> CandidatePlan {
        self.candidate_plans(objective, confidence, evidence)
            .into_iter()
            .next()
            .unwrap_or(CandidatePlan {
                strategy: StrategyKind::MinimalPatch,
                rationale: "Fallback plan.".to_string(),
                expected_value: 0.5,
                estimated_cost: 0.5,
                verification_plan: Vec::new(),
                plan_source: PlanSource::Heuristic,
            })
    }

    fn source(&self) -> PlanSource;
}

/// Trait for verification strategy. Implemented by heuristic, LLM, and null verifiers.
pub trait VerifyStrategy: Send + Sync {
    fn summarize_confidence(
        &self,
        evidence: &[EvidenceArtifact],
        edits: &[crate::proof::RealizedEdit],
    ) -> ConfidenceReport;

    fn unresolved_assumptions(
        &self,
        assumptions: &[crate::proof::Assumption],
        evidence: &[EvidenceArtifact],
    ) -> Vec<crate::proof::Assumption>;

    fn source(&self) -> VerificationSource;
}

// ═══════════════════════════════════════════════════════════════════════════
//  NullPlanner — Fast mode: returns MinimalPatch immediately, zero overhead
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Default)]
pub struct NullPlanner;

impl PlanStrategy for NullPlanner {
    fn candidate_plans(
        &self,
        _objective: &Objective,
        _confidence: &ConfidenceReport,
        _evidence: &[EvidenceArtifact],
    ) -> Vec<CandidatePlan> {
        vec![CandidatePlan {
            strategy: StrategyKind::MinimalPatch,
            rationale: "Fast mode — direct execution.".to_string(),
            expected_value: 0.5,
            estimated_cost: 0.1,
            verification_plan: Vec::new(),
            plan_source: PlanSource::Heuristic,
        }]
    }

    fn source(&self) -> PlanSource {
        PlanSource::Heuristic
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  HeuristicPlanner — Balanced mode: keyword-driven strategy selection
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Default)]
pub struct Planner;

impl Planner {
    pub fn candidate_plans(
        &self,
        objective: &Objective,
        confidence: &ConfidenceReport,
    ) -> Vec<CandidatePlan> {
        self.candidate_plans_with_evidence(objective, confidence, &[])
    }

    pub fn candidate_plans_with_evidence(
        &self,
        objective: &Objective,
        confidence: &ConfidenceReport,
        evidence: &[EvidenceArtifact],
    ) -> Vec<CandidatePlan> {
        let statement = objective.statement.to_ascii_lowercase();
        let mentions_refactor = statement.contains("refactor") || statement.contains("architecture");
        let mentions_verify = statement.contains("verify") || statement.contains("test");
        let mentions_fix = statement.contains("fix") || statement.contains("bug") || statement.contains("correct");
        let verification_heavy = mentions_verify
            || statement.contains("both documented cases")
            || statement.contains("documented behavior")
            || statement.contains("example")
            || statement.contains("regression");
        let failed_verification_streak = repeated_failed_verifications(evidence);
        let pivot_to_characterization = failed_verification_streak >= 2;

        let mut plans = Vec::new();

        plans.push(CandidatePlan {
            strategy: StrategyKind::MinimalPatch,
            rationale: if pivot_to_characterization {
                format!(
                    "Start small, but repeated failed verifications ({}) make blind patching less reliable now.",
                    failed_verification_streak
                )
            } else {
                "Prefer the smallest change that could satisfy the objective with minimal blast radius.".to_string()
            },
            expected_value: if pivot_to_characterization {
                0.44
            } else if mentions_fix && verification_heavy {
                // When both fix AND verification keywords present,
                // reduce MinimalPatch slightly so CharacterizationFirst wins.
                // Tasks like "fix the regression" or "fix the tests" benefit
                // from running tests first, not blind patching.
                0.72
            } else if mentions_fix {
                0.82
            } else {
                0.55
            },
            estimated_cost: 0.25,
            verification_plan: vec![VerificationStep {
                description: "Run the narrowest command or test that directly checks the requested behavior.".to_string(),
            }],
            plan_source: PlanSource::Heuristic,
        });

        plans.push(CandidatePlan {
            strategy: StrategyKind::RootCauseRepair,
            rationale: "Spend additional effort to understand the underlying failure mode before editing.".to_string(),
            // Only boost RootCauseRepair when there IS evidence of a problem
            // (failed tests, errors, etc.) but confidence remains low.
            // At the start of a task, zero confidence is normal.
            expected_value: if confidence.overall() < 0.45 && !evidence.is_empty() {
                0.84
            } else {
                0.68
            },
            estimated_cost: 0.45,
            verification_plan: vec![
                VerificationStep {
                    description: "Read the relevant implementation and documentation before mutating.".to_string(),
                },
                VerificationStep {
                    description: "Verify behavior with a runtime command or focused test after the change.".to_string(),
                },
            ],
            plan_source: PlanSource::Heuristic,
        });

        plans.push(CandidatePlan {
            strategy: StrategyKind::CharacterizationFirst,
            rationale: if pivot_to_characterization {
                format!(
                    "Repeated failed verifications ({}) suggest the task needs characterization and expected-vs-actual comparison before more edits.",
                    failed_verification_streak
                )
            } else {
                "Stabilize expected behavior with explicit checks before trusting a repair.".to_string()
            },
            expected_value: if pivot_to_characterization {
                0.99
            } else if verification_heavy {
                0.95
            } else {
                0.64
            },
            estimated_cost: if pivot_to_characterization || verification_heavy {
                0.4
            } else {
                0.55
            },
            verification_plan: vec![
                VerificationStep {
                    description: "Run the documented examples or existing targeted checks first.".to_string(),
                },
                VerificationStep {
                    description: "Re-run those checks after the change and compare outputs.".to_string(),
                },
            ],
            plan_source: PlanSource::Heuristic,
        });

        if mentions_refactor {
            plans.push(CandidatePlan {
                strategy: StrategyKind::ArchitecturalRepair,
                rationale: "The objective appears structural, so a broader but cleaner repair may be justified.".to_string(),
                expected_value: if mentions_verify { 0.80 } else { 0.72 },
                estimated_cost: if mentions_verify { 0.45 } else { 0.35 },
                verification_plan: vec![VerificationStep {
                    description: "Confirm public behavior and boundaries still hold after the structural change.".to_string(),
                }],
                plan_source: PlanSource::Heuristic,
            });
        }

        plans.push(CandidatePlan {
            strategy: StrategyKind::DiagnosticOnly,
            rationale: "If confidence stays too low, stop mutation and return evidence plus options.".to_string(),
            // DiagnosticOnly should score high only when the agent has been running
            // (evidence exists) but confidence is still low. At the start of a task
            // (no evidence), zero confidence is normal and shouldn't trigger diagnostic mode.
            expected_value: if !mentions_fix
                && confidence.overall() < 0.25
                && !pivot_to_characterization
                && !evidence.is_empty()
            {
                0.9
            } else {
                0.35
            },
            estimated_cost: 0.15,
            verification_plan: vec![VerificationStep {
                description: "Collect evidence without mutating if the state remains too uncertain.".to_string(),
            }],
            plan_source: PlanSource::Heuristic,
        });

        plans.sort_by(|a, b| {
            let a_score = a.expected_value - a.estimated_cost;
            let b_score = b.expected_value - b.estimated_cost;
            b_score
                .partial_cmp(&a_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        plans
    }

    pub fn select_plan(
        &self,
        objective: &Objective,
        confidence: &ConfidenceReport,
    ) -> CandidatePlan {
        self.select_plan_with_evidence(objective, confidence, &[])
    }

    pub fn select_plan_with_evidence(
        &self,
        objective: &Objective,
        confidence: &ConfidenceReport,
        evidence: &[EvidenceArtifact],
    ) -> CandidatePlan {
        self.candidate_plans_with_evidence(objective, confidence, evidence)
            .into_iter()
            .next()
            .unwrap_or(CandidatePlan {
                strategy: StrategyKind::MinimalPatch,
                rationale: "Fallback plan.".to_string(),
                expected_value: 0.5,
                estimated_cost: 0.5,
                verification_plan: Vec::new(),
                plan_source: PlanSource::Heuristic,
            })
    }
}

impl PlanStrategy for Planner {
    fn candidate_plans(
        &self,
        objective: &Objective,
        confidence: &ConfidenceReport,
        evidence: &[EvidenceArtifact],
    ) -> Vec<CandidatePlan> {
        self.candidate_plans_with_evidence(objective, confidence, evidence)
    }

    fn source(&self) -> PlanSource {
        PlanSource::Heuristic
    }
}

fn repeated_failed_verifications(evidence: &[EvidenceArtifact]) -> usize {
    evidence
        .iter()
        .rev()
        .filter_map(|artifact| match artifact {
            EvidenceArtifact::CommandResult {
                kind: VerificationKind::Test | VerificationKind::Build | VerificationKind::RuntimeCheck,
                success,
                ..
            } => Some(*success),
            _ => None,
        })
        .take(6)
        .filter(|success| !success)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn objective() -> Objective {
        Objective::from_prompt("Fix the bug and verify with tests and examples")
    }

    #[test]
    fn pivots_to_characterization_after_repeated_failed_verifications() {
        let planner = Planner;
        let confidence = ConfidenceReport::default();
        let evidence = vec![
            EvidenceArtifact::CommandResult {
                kind: VerificationKind::Test,
                command: "pytest".to_string(),
                output: "1 failed".to_string(),
                success: false,
            },
            EvidenceArtifact::CommandResult {
                kind: VerificationKind::RuntimeCheck,
                command: "python app.py".to_string(),
                output: "wrong output".to_string(),
                success: false,
            },
        ];

        let selected = planner.select_plan_with_evidence(&objective(), &confidence, &evidence);

        assert_eq!(selected.strategy, StrategyKind::CharacterizationFirst);
        assert!(selected.rationale.contains("Repeated failed verifications"));
    }

    #[test]
    fn pivots_with_interleaved_successes_in_recent_window() {
        let planner = Planner;
        let confidence = ConfidenceReport::default();
        let evidence = vec![
            EvidenceArtifact::CommandResult {
                kind: VerificationKind::RuntimeCheck,
                command: "python app.py --example-1".to_string(),
                output: "wrong output".to_string(),
                success: false,
            },
            EvidenceArtifact::CommandResult {
                kind: VerificationKind::RuntimeCheck,
                command: "python app.py --example-2".to_string(),
                output: "good output".to_string(),
                success: true,
            },
            EvidenceArtifact::CommandResult {
                kind: VerificationKind::Test,
                command: "python3 -m unittest".to_string(),
                output: "1 failed".to_string(),
                success: false,
            },
        ];

        let selected = planner.select_plan_with_evidence(&objective(), &confidence, &evidence);

        assert_eq!(selected.strategy, StrategyKind::CharacterizationFirst);
    }

    #[test]
    fn keeps_characterization_first_with_verify_keywords() {
        let planner = Planner;
        let confidence = ConfidenceReport::default();

        // objective() includes "verify" and "test" keywords, so
        // CharacterizationFirst should win over MinimalPatch
        let selected = planner.select_plan_with_evidence(&objective(), &confidence, &[]);

        assert_eq!(selected.strategy, StrategyKind::CharacterizationFirst);
    }

    #[test]
    fn selects_minimal_patch_for_pure_fix() {
        let planner = Planner;
        let confidence = ConfidenceReport::default();

        // Pure fix without verify/test keywords → MinimalPatch wins
        let objective = Objective::from_prompt("Fix the null pointer bug in main.py");
        let selected = planner.select_plan_with_evidence(&objective, &confidence, &[]);

        assert_eq!(selected.strategy, StrategyKind::MinimalPatch);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Adaptive Strategy Scoring — Thompson Sampling
// ═══════════════════════════════════════════════════════════════════════════

/// Bayesian multi-armed bandit for adaptive strategy scoring.
/// Each strategy maintains a Beta(α, β) distribution updated from outcomes.
/// Expected value: E[s] = α / (α + β). Update O(1) per task completion.
/// Prior: Beta(1, 1) (uniform) — learns from scratch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptiveScorer {
    /// Per-strategy Beta distribution parameters: (successes, failures)
    strategies: std::collections::HashMap<String, (f64, f64)>,
}

impl Default for AdaptiveScorer {
    fn default() -> Self {
        Self::new()
    }
}

impl AdaptiveScorer {
    pub fn new() -> Self {
        let mut strategies = std::collections::HashMap::new();
        // Initialize with Beta(1,1) uniform prior for each strategy
        for kind in &["MinimalPatch", "RootCauseRepair", "ArchitecturalRepair",
                       "DiagnosticOnly", "CharacterizationFirst"] {
            strategies.insert(kind.to_string(), (1.0, 1.0));
        }
        Self { strategies }
    }

    /// Record a task outcome. O(1).
    pub fn record_outcome(&mut self, strategy: &StrategyKind, success: bool) {
        let key = format!("{:?}", strategy);
        let entry = self.strategies.entry(key).or_insert((1.0, 1.0));
        if success {
            entry.0 += 1.0; // α += 1
        } else {
            entry.1 += 1.0; // β += 1
        }
    }

    /// Get the posterior expected value for a strategy: E[s] = α / (α + β).
    pub fn expected_value(&self, strategy: &StrategyKind) -> f64 {
        let key = format!("{:?}", strategy);
        if let Some(&(alpha, beta)) = self.strategies.get(&key) {
            alpha / (alpha + beta)
        } else {
            0.5 // Uniform prior
        }
    }

    /// Total observations for a strategy.
    pub fn sample_count(&self, strategy: &StrategyKind) -> u32 {
        let key = format!("{:?}", strategy);
        if let Some(&(alpha, beta)) = self.strategies.get(&key) {
            (alpha + beta - 2.0).max(0.0) as u32 // Subtract prior
        } else {
            0
        }
    }

    /// Override the heuristic expected_value with the learned value
    /// once we have enough samples (>= min_samples).
    pub fn adjust_plan_score(&self, plan: &mut CandidatePlan, min_samples: u32) {
        if self.sample_count(&plan.strategy) >= min_samples {
            plan.expected_value = self.expected_value(&plan.strategy) as f32;
        }
    }
}

/// Generate an autonomous remediation task from health metrics.
/// Called by the health monitor when H(t) < threshold.
pub fn generate_remediation_plan(
    health_prompt: &str,
    scorer: &AdaptiveScorer,
) -> CandidatePlan {
    let objective = Objective::from_prompt(health_prompt);
    let planner = Planner;
    let confidence = ConfidenceReport::default();

    let mut plan = planner.select_plan_with_evidence(&objective, &confidence, &[]);

    // Apply adaptive scoring if enough data
    scorer.adjust_plan_score(&mut plan, 10);

    plan
}

// ═══════════════════════════════════════════════════════════════════════════
//  Q&A classifier
// ═══════════════════════════════════════════════════════════════════════════

/// Quick classifier: is this prompt a question/information request
/// rather than a coding task?
///
/// Used to short-circuit the planning system for Q&A. This avoids injecting
/// "Strategy: DiagnosticOnly" into the system prompt when the user asks
/// "what is this code" or "explain this function", which causes the model
/// to wander through file-discovery instead of answering directly.
///
/// Errs on the side of NOT classifying as Q&A — if in doubt, returns false.
pub fn is_question_task(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    let trimmed = lower.trim();

    if trimmed.is_empty() {
        return false;
    }

    // Ends with a question mark — strong signal
    if trimmed.ends_with('?') {
        return true;
    }

    // Starts with a question word
    const QUESTION_STARTERS: &[&str] = &[
        "what ", "what's ", "whats ",
        "how ", "how's ",
        "why ", "where ", "when ", "which ",
        "who ", "whom ",
        "can you explain", "could you explain",
        "explain ", "describe ",
        "tell me", "show me",
        "is there", "are there",
        "does ", "do you know",
        "what is", "what are",
        "how do ", "how does ",
        "how can ", "how should ",
        "what do ", "what does ",
    ];

    if QUESTION_STARTERS.iter().any(|q| trimmed.starts_with(q)) {
        // Check for action verbs that override the question form
        // e.g., "can you fix the bug" is a task, not a question
        const TASK_VERBS_IN_QUESTIONS: &[&str] = &[
            "fix", "add", "create", "write", "edit", "change",
            "update", "refactor", "implement", "build", "delete",
            "remove", "modify", "replace", "move", "rename",
            "install", "deploy", "configure", "set up",
        ];

        if TASK_VERBS_IN_QUESTIONS.iter().any(|v| trimmed.contains(v)) {
            return false;
        }

        return true;
    }

    // Short prompts (≤8 words) without action verbs are likely Q&A
    let word_count = trimmed.split_whitespace().count();
    if word_count <= 8 {
        const ACTION_WORDS: &[&str] = &[
            "fix", "add", "create", "write", "edit", "change",
            "update", "refactor", "implement", "build", "delete",
            "remove", "modify", "replace", "move", "rename",
            "install", "deploy", "run", "execute", "test",
            "debug", "optimize", "migrate", "convert",
        ];

        if !ACTION_WORDS.iter().any(|w| {
            trimmed.split_whitespace().any(|token| token == *w)
        }) {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod question_task_tests {
    use super::is_question_task;

    #[test]
    fn question_mark_is_question() {
        assert!(is_question_task("what files are in this project?"));
        assert!(is_question_task("how does the auth system work?"));
    }

    #[test]
    fn question_starters_are_questions() {
        assert!(is_question_task("what is this code"));
        assert!(is_question_task("explain the architecture"));
        assert!(is_question_task("how does the agent loop work"));
        assert!(is_question_task("where is the main function defined"));
        assert!(is_question_task("show me the config file"));
        assert!(is_question_task("describe the project structure"));
    }

    #[test]
    fn short_prompts_without_actions_are_questions() {
        assert!(is_question_task("current directory"));
        assert!(is_question_task("list of dependencies"));
        assert!(is_question_task("project overview"));
        assert!(is_question_task("status"));
    }

    #[test]
    fn task_verbs_override_question_form() {
        assert!(!is_question_task("can you fix the bug in main.rs"));
        assert!(!is_question_task("how should I implement the cache"));
        assert!(!is_question_task("explain and then fix the failing test"));
    }

    #[test]
    fn action_prompts_are_not_questions() {
        assert!(!is_question_task("fix the panic on line 42"));
        assert!(!is_question_task("add a new endpoint for /api/users"));
        assert!(!is_question_task("refactor the database module"));
        assert!(!is_question_task("create a migration for the users table"));
        assert!(!is_question_task("run the test suite and fix failures"));
        assert!(!is_question_task("implement retry logic for the HTTP client"));
    }

    #[test]
    fn empty_prompt_is_not_question() {
        assert!(!is_question_task(""));
        assert!(!is_question_task("   "));
    }

    #[test]
    fn longer_action_prompts_are_not_questions() {
        assert!(!is_question_task(
            "update the config parser to support nested TOML tables with array values"
        ));
        assert!(!is_question_task(
            "write a comprehensive test suite for the authentication middleware"
        ));
    }
}