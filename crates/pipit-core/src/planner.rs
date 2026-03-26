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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidatePlan {
    pub strategy: StrategyKind,
    pub rationale: String,
    pub expected_value: f32,
    pub estimated_cost: f32,
    pub verification_plan: Vec<VerificationStep>,
}

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
            } else if mentions_fix {
                0.82
            } else {
                0.55
            },
            estimated_cost: 0.25,
            verification_plan: vec![VerificationStep {
                description: "Run the narrowest command or test that directly checks the requested behavior.".to_string(),
            }],
        });

        plans.push(CandidatePlan {
            strategy: StrategyKind::RootCauseRepair,
            rationale: "Spend additional effort to understand the underlying failure mode before editing.".to_string(),
            expected_value: if confidence.overall() < 0.45 { 0.84 } else { 0.68 },
            estimated_cost: 0.45,
            verification_plan: vec![
                VerificationStep {
                    description: "Read the relevant implementation and documentation before mutating.".to_string(),
                },
                VerificationStep {
                    description: "Verify behavior with a runtime command or focused test after the change.".to_string(),
                },
            ],
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
        });

        if mentions_refactor {
            plans.push(CandidatePlan {
                strategy: StrategyKind::ArchitecturalRepair,
                rationale: "The objective appears structural, so a broader but cleaner repair may be justified.".to_string(),
                expected_value: 0.62,
                estimated_cost: 0.75,
                verification_plan: vec![VerificationStep {
                    description: "Confirm public behavior and boundaries still hold after the structural change.".to_string(),
                }],
            });
        }

        plans.push(CandidatePlan {
            strategy: StrategyKind::DiagnosticOnly,
            rationale: "If confidence stays too low, stop mutation and return evidence plus options.".to_string(),
            expected_value: if !mentions_fix && confidence.overall() < 0.25 && !pivot_to_characterization {
                0.9
            } else {
                0.35
            },
            estimated_cost: 0.15,
            verification_plan: vec![VerificationStep {
                description: "Collect evidence without mutating if the state remains too uncertain.".to_string(),
            }],
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
            })
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
    fn keeps_minimal_patch_without_failed_verification_streak() {
        let planner = Planner;
        let confidence = ConfidenceReport::default();

        let selected = planner.select_plan_with_evidence(&objective(), &confidence, &[]);

        assert_eq!(selected.strategy, StrategyKind::MinimalPatch);
    }
}