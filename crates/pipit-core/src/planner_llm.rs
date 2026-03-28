// ─────────────────────────────────────────────────────────────────────────────
//  planner_llm.rs — LLM-driven plan generation via structured output
// ─────────────────────────────────────────────────────────────────────────────
//
//  Sends the planner system prompt to the planner-role model, parses
//  the structured PlanSpec JSON, and converts it to a CandidatePlan.
//  Falls back to the heuristic planner on parse failure.
//
// ─────────────────────────────────────────────────────────────────────────────

use crate::pev::{ModelRouter, ModelRole, PlanSpec, planner_system_prompt};
use crate::planner::{
    CandidatePlan, PlanSource, PlanStrategy, Planner, StrategyKind,
};
use crate::proof::{ConfidenceReport, EvidenceArtifact, Objective, VerificationStep};
use pipit_provider::{CompletionRequest, ContentBlock, ContentEvent, LlmProvider, Message, Role};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// LLM-driven planner: sends structured prompts to a planner-role model
/// and parses the returned PlanSpec JSON.
///
/// # Implementation Tier
/// Tier 2: LLM-driven (structured output from model).
pub struct LlmPlanner {
    provider: Arc<dyn LlmProvider>,
    model_id: String,
    repo_summary: String,
    /// Heuristic fallback for parse failures and as a baseline portfolio.
    heuristic: Planner,
}

impl LlmPlanner {
    pub fn new(router: &ModelRouter, repo_summary: String) -> Self {
        let role_provider = router.for_role(ModelRole::Planner);
        Self {
            provider: role_provider.provider.clone(),
            model_id: role_provider.model_id.clone(),
            repo_summary,
            heuristic: Planner,
        }
    }

    /// Extract JSON from a response that may contain markdown fences or preamble.
    fn extract_json(text: &str) -> Option<&str> {
        // Try to find JSON within ```json ... ``` fences first
        if let Some(start) = text.find("```json") {
            let after_fence = &text[start + 7..];
            if let Some(end) = after_fence.find("```") {
                return Some(after_fence[..end].trim());
            }
        }
        // Try to find outermost braces
        let first_brace = text.find('{')?;
        let last_brace = text.rfind('}')?;
        if last_brace > first_brace {
            Some(&text[first_brace..=last_brace])
        } else {
            None
        }
    }

    /// Convert a PlanSpec into a CandidatePlan with LLM-derived expected value.
    fn plan_spec_to_candidate(spec: &PlanSpec) -> CandidatePlan {
        // EV derived from plan specificity:
        // 0.5 base + 0.1*(files_to_read/5) + 0.1*(invariants/5) + 0.1*(verification_steps/5)
        let files_score = (spec.files_to_read.len().min(5) as f32) / 5.0;
        let invariants_score = (spec.invariants.len().min(5) as f32) / 5.0;
        let verification_score = (spec.verification_steps.len().min(5) as f32) / 5.0;
        let expected_value = 0.5 + 0.1 * files_score + 0.1 * invariants_score + 0.1 * verification_score;

        // Strategy mapping from LLM strategy description
        let strategy = classify_strategy(&spec.strategy);

        CandidatePlan {
            strategy,
            rationale: if spec.rationale.is_empty() {
                spec.strategy.clone()
            } else {
                spec.rationale.clone()
            },
            expected_value,
            estimated_cost: 0.5, // LLM plans cost more but are more targeted
            verification_plan: spec
                .verification_steps
                .iter()
                .map(|s| VerificationStep {
                    description: s.clone(),
                })
                .collect(),
            plan_source: PlanSource::LlmStructured,
        }
    }

    /// Synchronous plan generation for the trait implementation.
    /// Since the trait is not async, we use tokio::runtime::Handle to block.
    fn generate_plan_blocking(
        &self,
        objective: &Objective,
    ) -> Option<CandidatePlan> {
        let handle = match tokio::runtime::Handle::try_current() {
            Ok(h) => h,
            Err(_) => return None,
        };

        let system_prompt = planner_system_prompt(&self.repo_summary);
        let request = CompletionRequest {
            system: system_prompt,
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text(objective.statement.clone())],
                metadata: Default::default(),
            }],
            tools: vec![],
            max_tokens: Some(4096),
            temperature: Some(0.3),
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
        let spec: PlanSpec = serde_json::from_str(json_str).ok()?;
        Some(Self::plan_spec_to_candidate(&spec))
    }
}

fn classify_strategy(description: &str) -> StrategyKind {
    let lower = description.to_ascii_lowercase();
    if lower.contains("minimal") || lower.contains("patch") || lower.contains("surgical") {
        StrategyKind::MinimalPatch
    } else if lower.contains("root cause") || lower.contains("debug") || lower.contains("investigate") {
        StrategyKind::RootCauseRepair
    } else if lower.contains("architectural") || lower.contains("refactor") || lower.contains("restructur") {
        StrategyKind::ArchitecturalRepair
    } else if lower.contains("diagnostic") || lower.contains("analyze") || lower.contains("read-only") {
        StrategyKind::DiagnosticOnly
    } else if lower.contains("characteriz") || lower.contains("test first") || lower.contains("baseline") {
        StrategyKind::CharacterizationFirst
    } else {
        StrategyKind::RootCauseRepair // Default for LLM plans
    }
}

impl PlanStrategy for LlmPlanner {
    fn candidate_plans(
        &self,
        objective: &Objective,
        confidence: &ConfidenceReport,
        evidence: &[EvidenceArtifact],
    ) -> Vec<CandidatePlan> {
        // Start with heuristic baseline
        let mut plans = self.heuristic.candidate_plans_with_evidence(objective, confidence, evidence);

        // Try LLM-generated plan
        match self.generate_plan_blocking(objective) {
            Some(llm_plan) => {
                tracing::info!(
                    "LLM planner produced plan: {:?} (EV={:.2})",
                    llm_plan.strategy,
                    llm_plan.expected_value
                );
                // Insert at front — LLM plans take priority
                plans.insert(0, llm_plan);
            }
            None => {
                tracing::warn!("LLM planner failed to produce a valid plan, using heuristic fallback");
            }
        }

        // Re-sort by net score
        plans.sort_by(|a, b| {
            let a_score = a.expected_value - a.estimated_cost;
            let b_score = b.expected_value - b.estimated_cost;
            b_score
                .partial_cmp(&a_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        plans
    }

    fn source(&self) -> PlanSource {
        PlanSource::LlmStructured
    }
}
