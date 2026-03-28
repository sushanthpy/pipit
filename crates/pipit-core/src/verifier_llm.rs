// ─────────────────────────────────────────────────────────────────────────────
//  verifier_llm.rs — LLM-driven verification with structured verdict parsing
// ─────────────────────────────────────────────────────────────────────────────
//
//  Sends execution evidence to the verifier-role model and parses the
//  structured VerificationReport JSON. Falls back to heuristic verifier
//  on parse failure.
//
// ─────────────────────────────────────────────────────────────────────────────

use crate::pev::{
    ExecutionResult, ModelRole, ModelRouter, PlanSpec, VerificationReport, Verdict,
    verifier_evidence_prompt, verifier_system_prompt,
};
use crate::planner::{VerificationSource, VerifyStrategy};
use crate::proof::{Assumption, ConfidenceReport, EvidenceArtifact, RealizedEdit};
use crate::verifier::Verifier;
use pipit_provider::{CompletionRequest, ContentBlock, ContentEvent, LlmProvider, Message, Role};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// LLM-driven verifier: sends execution results to a verifier-role model
/// and parses the structured VerificationReport JSON.
///
/// # Implementation Tier
/// Tier 2: LLM-driven (structured output from model).
pub struct LlmVerifier {
    provider: Arc<dyn LlmProvider>,
    model_id: String,
    /// Current plan for verification context.
    plan: PlanSpec,
    /// Heuristic fallback for parse failures.
    heuristic: Verifier,
}

impl LlmVerifier {
    pub fn new(router: &ModelRouter, plan: PlanSpec) -> Self {
        let role_provider = router.for_role(ModelRole::Verifier);
        Self {
            provider: role_provider.provider.clone(),
            model_id: role_provider.model_id.clone(),
            plan,
            heuristic: Verifier,
        }
    }

    /// Update the plan context for subsequent verifications.
    pub fn set_plan(&mut self, plan: PlanSpec) {
        self.plan = plan;
    }

    /// Extract JSON from a response that may contain markdown fences.
    fn extract_json(text: &str) -> Option<&str> {
        if let Some(start) = text.find("```json") {
            let after_fence = &text[start + 7..];
            if let Some(end) = after_fence.find("```") {
                return Some(after_fence[..end].trim());
            }
        }
        let first_brace = text.find('{')?;
        let last_brace = text.rfind('}')?;
        if last_brace > first_brace {
            Some(&text[first_brace..=last_brace])
        } else {
            None
        }
    }

    /// Build an ExecutionResult from evidence artifacts for the verifier prompt.
    fn evidence_to_execution_result(
        evidence: &[EvidenceArtifact],
        edits: &[RealizedEdit],
    ) -> ExecutionResult {
        use crate::pev::{CommandOutput, EditSummary};

        let modified_files: Vec<String> = edits.iter().map(|e| e.path.clone()).collect();
        let realized_edits: Vec<EditSummary> = edits
            .iter()
            .map(|e| EditSummary {
                path: e.path.clone(),
                description: e.summary.clone(),
                lines_added: 0,
                lines_removed: 0,
            })
            .collect();
        let command_outputs: Vec<CommandOutput> = evidence
            .iter()
            .filter_map(|artifact| match artifact {
                EvidenceArtifact::CommandResult {
                    command,
                    output,
                    success,
                    ..
                } => Some(CommandOutput {
                    command: command.clone(),
                    stdout: output.clone(),
                    stderr: String::new(),
                    exit_code: if *success { 0 } else { 1 },
                    success: *success,
                }),
                _ => None,
            })
            .collect();

        let has_test_pass = evidence.iter().any(|a| {
            matches!(
                a,
                EvidenceArtifact::CommandResult {
                    success: true,
                    ..
                }
            )
        });

        ExecutionResult {
            modified_files,
            realized_edits,
            command_outputs,
            diff_summary: String::new(),
            turns_used: 0,
            self_reported_complete: has_test_pass,
        }
    }

    /// Synchronous verification call for the trait implementation.
    fn verify_blocking(
        &self,
        evidence: &[EvidenceArtifact],
        edits: &[RealizedEdit],
    ) -> Option<VerificationReport> {
        let handle = match tokio::runtime::Handle::try_current() {
            Ok(h) => h,
            Err(_) => return None,
        };

        let system_prompt = verifier_system_prompt(&self.plan);
        let exec_result = Self::evidence_to_execution_result(evidence, edits);
        let user_prompt = verifier_evidence_prompt(&exec_result, "");

        let request = CompletionRequest {
            system: system_prompt,
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text(user_prompt)],
                metadata: Default::default(),
            }],
            tools: vec![],
            max_tokens: Some(4096),
            temperature: Some(0.2),
            stop_sequences: vec![],
        };

        let provider = self.provider.clone();
        let result = std::thread::spawn(move || {
            handle.block_on(async {
                use futures::StreamExt;
                let cancel = CancellationToken::new();
                let mut stream = provider.complete(request, cancel).await.ok()?;
                let mut text = String::new();
                while let Some(Ok(event)) = stream.next().await {
                    if let ContentEvent::ContentDelta { text: delta } = event {
                        text.push_str(&delta);
                    }
                }
                Some(text)
            })
        })
        .join()
        .ok()
        .flatten()?;

        let json_str = Self::extract_json(&result)?;
        serde_json::from_str::<VerificationReport>(json_str).ok()
    }
}

impl VerifyStrategy for LlmVerifier {
    fn summarize_confidence(
        &self,
        evidence: &[EvidenceArtifact],
        edits: &[RealizedEdit],
    ) -> ConfidenceReport {
        // Try LLM verification first
        match self.verify_blocking(evidence, edits) {
            Some(report) => {
                tracing::info!(
                    "LLM verifier verdict: {} (confidence={:.2})",
                    report.verdict,
                    report.confidence
                );

                // Convert VerificationReport to ConfidenceReport
                let verification_strength = match report.verdict {
                    Verdict::Pass => report.confidence,
                    Verdict::Repairable => report.confidence * 0.6,
                    Verdict::Fail => report.confidence * 0.2,
                    Verdict::Inconclusive => 0.3,
                };

                let findings_pass_rate = if report.findings.is_empty() {
                    0.5
                } else {
                    let passed = report.findings.iter().filter(|f| f.passed).count() as f32;
                    passed / report.findings.len() as f32
                };

                ConfidenceReport {
                    root_cause: if matches!(report.verdict, Verdict::Pass) {
                        0.85
                    } else {
                        0.5
                    },
                    semantic_understanding: findings_pass_rate,
                    side_effect_risk: if report.needs_replan { 0.3 } else { 0.7 },
                    verification_strength,
                    environment_certainty: report.confidence,
                }
            }
            None => {
                tracing::warn!("LLM verifier failed, falling back to heuristic");
                self.heuristic.summarize_confidence(evidence, edits)
            }
        }
    }

    fn unresolved_assumptions(
        &self,
        assumptions: &[Assumption],
        evidence: &[EvidenceArtifact],
    ) -> Vec<Assumption> {
        // Delegate to heuristic — assumption tracking is already rule-based
        self.heuristic.unresolved_assumptions(assumptions, evidence)
    }

    fn source(&self) -> VerificationSource {
        VerificationSource::LlmStructured
    }
}
