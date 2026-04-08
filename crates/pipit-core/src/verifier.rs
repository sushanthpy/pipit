use crate::planner::{VerificationSource, VerifyStrategy};
use crate::proof::{Assumption, ConfidenceReport, EvidenceArtifact, RealizedEdit};

// ═══════════════════════════════════════════════════════════════════════════
//  NullVerifier — Fast mode: always passes, zero overhead
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Default)]
pub struct NullVerifier;

impl VerifyStrategy for NullVerifier {
    fn summarize_confidence(
        &self,
        _evidence: &[EvidenceArtifact],
        _edits: &[RealizedEdit],
    ) -> ConfidenceReport {
        ConfidenceReport::default()
    }

    fn unresolved_assumptions(
        &self,
        _assumptions: &[Assumption],
        _evidence: &[EvidenceArtifact],
    ) -> Vec<Assumption> {
        Vec::new()
    }

    fn source(&self) -> VerificationSource {
        VerificationSource::None
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  HeuristicVerifier — Balanced mode: confidence from evidence pass rates
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Default)]
pub struct Verifier;

impl Verifier {
    pub fn summarize_confidence(
        &self,
        evidence: &[EvidenceArtifact],
        edits: &[RealizedEdit],
    ) -> ConfidenceReport {
        let total_evidence = evidence.len().max(1) as f32;
        let semantic_understanding = (evidence
            .iter()
            .filter(|artifact| matches!(artifact, EvidenceArtifact::FileRead { .. }))
            .count() as f32
            / total_evidence)
            .clamp(0.0, 1.0);

        let mut verification_points: f32 = 0.0;
        let mut verification_cap: f32 = 0.0;

        for artifact in evidence {
            match artifact {
                EvidenceArtifact::CommandResult { kind, success, .. } => {
                    let weight: f32 = match kind {
                        crate::proof::VerificationKind::Test => 1.0,
                        crate::proof::VerificationKind::Build => 0.8,
                        crate::proof::VerificationKind::Benchmark => 0.9,
                        crate::proof::VerificationKind::RuntimeCheck => 0.7,
                        crate::proof::VerificationKind::Shell => 0.4,
                    };
                    verification_cap += weight;
                    if *success {
                        verification_points += weight;
                    }
                }
                EvidenceArtifact::ToolExecution { success, .. } => {
                    verification_cap += 0.2;
                    if *success {
                        verification_points += 0.2;
                    }
                }
                _ => {}
            }
        }

        let verification_strength = if verification_cap > 0.0 {
            (verification_points / verification_cap).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let successful_runtime_checks = evidence
            .iter()
            .filter(|artifact| {
                matches!(
                    artifact,
                    EvidenceArtifact::CommandResult {
                        kind: crate::proof::VerificationKind::RuntimeCheck,
                        success: true,
                        ..
                    }
                )
            })
            .count() as f32;

        let successful_test_or_build = evidence
            .iter()
            .filter(|artifact| {
                matches!(
                    artifact,
                    EvidenceArtifact::CommandResult {
                        kind: crate::proof::VerificationKind::Test,
                        success: true,
                        ..
                    } | EvidenceArtifact::CommandResult {
                        kind: crate::proof::VerificationKind::Build,
                        success: true,
                        ..
                    }
                )
            })
            .count() as f32;

        let post_tool_policy_violations = evidence
            .iter()
            .filter(|artifact| {
                matches!(
                    artifact,
                    EvidenceArtifact::PolicyViolation {
                        stage: crate::proof::PolicyStage::PostToolUse,
                        ..
                    }
                )
            })
            .count() as f32;

        let post_tool_policy_with_mutation = evidence
            .iter()
            .filter(|artifact| {
                matches!(
                    artifact,
                    EvidenceArtifact::PolicyViolation {
                        stage: crate::proof::PolicyStage::PostToolUse,
                        mutation_applied: true,
                        ..
                    }
                )
            })
            .count() as f32;

        let side_effect_risk = if edits.is_empty() {
            0.4
        } else if post_tool_policy_with_mutation > 0.0 {
            0.3
        } else if successful_test_or_build > 0.0 {
            0.68
        } else {
            0.55
        };

        let environment_certainty = if successful_test_or_build > 0.0 {
            0.9
        } else if post_tool_policy_violations > 0.0 {
            0.35
        } else if successful_runtime_checks > 0.0 {
            0.8
        } else {
            0.5
        };

        ConfidenceReport {
            root_cause: if edits.is_empty() {
                0.35
            } else if successful_test_or_build > 0.0 || successful_runtime_checks > 1.0 {
                0.72
            } else {
                0.6
            },
            semantic_understanding,
            side_effect_risk,
            verification_strength,
            environment_certainty,
        }
    }

    pub fn unresolved_assumptions(
        &self,
        assumptions: &[Assumption],
        evidence: &[EvidenceArtifact],
    ) -> Vec<Assumption> {
        let has_runtime_evidence = evidence.iter().any(|artifact| {
            matches!(
                artifact,
                EvidenceArtifact::CommandResult { success: true, .. }
            )
        });

        assumptions
            .iter()
            .cloned()
            .map(|mut assumption| {
                if has_runtime_evidence {
                    assumption.verified = true;
                }
                assumption
            })
            .filter(|assumption| !assumption.verified)
            .collect()
    }
}

impl VerifyStrategy for Verifier {
    fn summarize_confidence(
        &self,
        evidence: &[EvidenceArtifact],
        edits: &[RealizedEdit],
    ) -> ConfidenceReport {
        Verifier::summarize_confidence(self, evidence, edits)
    }

    fn unresolved_assumptions(
        &self,
        assumptions: &[Assumption],
        evidence: &[EvidenceArtifact],
    ) -> Vec<Assumption> {
        Verifier::unresolved_assumptions(self, assumptions, evidence)
    }

    fn source(&self) -> VerificationSource {
        VerificationSource::Heuristic
    }
}
