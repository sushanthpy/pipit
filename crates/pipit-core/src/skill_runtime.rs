//! Skill Runtime — bridges discovery, activation, budgeting, and kernel
//! validation into a single governed invocation path.
//!
//! This module ensures skills are typed control objects with budgets,
//! capability envelopes, and explicit runtime states — not loose prompt
//! content or ad-hoc hooks.

use crate::skill_activation::{ActivationRule, ActivationScope, SkillActivationIndex};
use crate::skill_budget::{
    allocate_skill_budget, create_budget_variants, InclusionLevel, SkillBudgetCandidate,
};
use crate::skill_kernel::{
    CompiledSkill, ExecutionMode, SkillKernel, SkillSandbox, SkillTokenBudget,
    SkillTrigger, SkillTrustTier, SkillValidation,
};
use crate::capability::CapabilitySet;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The skill runtime: single entry point for skill activation, budgeting,
/// and injection into the agent's context.
pub struct SkillRuntime {
    /// The validation + dispatch kernel.
    kernel: SkillKernel,
    /// Path-activation index (trie-based).
    activation_index: SkillActivationIndex,
    /// Token budget for the current turn's skill content.
    skill_token_budget: u64,
    /// Skill content cache: skill_id → (full_text, truncated_text).
    content_cache: HashMap<String, SkillContent>,
}

/// Cached skill content for prompt injection.
#[derive(Debug, Clone)]
pub struct SkillContent {
    pub full_text: String,
    pub truncated_text: String,
    pub name_only: String,
    pub token_estimate: u64,
    pub relevance: f32,
}

/// The result of activating skills for a turn: sized prompt segments.
#[derive(Debug, Clone)]
pub struct SkillInjection {
    /// Skill content to inject into the system prompt.
    pub prompt_segments: Vec<SkillSegment>,
    /// Total tokens consumed by skill content.
    pub total_tokens: u64,
    /// Skills that were excluded due to budget constraints.
    pub excluded: Vec<String>,
    /// Skills that were downgraded to name-only.
    pub downgraded: Vec<String>,
}

/// A single skill's contribution to the prompt.
#[derive(Debug, Clone)]
pub struct SkillSegment {
    pub skill_id: String,
    pub skill_name: String,
    pub content: String,
    pub inclusion_level: InclusionLevel,
    pub tokens: u64,
}

impl SkillRuntime {
    pub fn new(minimum_trust: SkillTrustTier, skill_token_budget: u64) -> Self {
        Self {
            kernel: SkillKernel::new(minimum_trust),
            activation_index: SkillActivationIndex::new(),
            skill_token_budget,
            content_cache: HashMap::new(),
        }
    }

    /// Register a compiled skill into both the kernel and the activation index.
    pub fn register_skill(
        &mut self,
        skill: CompiledSkill,
        content: SkillContent,
        scope: ActivationScope,
    ) {
        // Index for path-based activation
        let rule = ActivationRule {
            skill_id: skill.id.clone(),
            path_patterns: extract_path_patterns(&skill.trigger),
            language_patterns: extract_language_patterns(&skill.trigger),
            scope,
            defined_at: skill.source.clone(),
        };
        self.activation_index.add_rule(rule);

        // Cache content for budgeted injection
        self.content_cache.insert(skill.id.clone(), content);

        // Register in the kernel for validation
        self.kernel.register(skill);
    }

    /// Activate skills for a given context (files, languages) under budget control.
    /// Returns prompt segments sized to fit within the token budget.
    pub fn activate_for_context(
        &self,
        active_files: &[&str],
        languages: &[&str],
    ) -> SkillInjection {
        // 1. Find matching skills via the kernel (trigger-based).
        let kernel_matches = self.kernel.find_matching_skills(active_files, languages);

        // 2. Also find matches via the activation index (path-prefix trie).
        let mut matched_ids: Vec<String> = kernel_matches
            .iter()
            .map(|s| s.id.clone())
            .collect();

        let activated = self.activation_index.activate(active_files, languages);
        for id in activated {
            if !matched_ids.contains(&id) {
                matched_ids.push(id);
            }
        }

        // 3. Build budget candidates from matched skills.
        let mut candidates: Vec<SkillBudgetCandidate> = Vec::new();
        for id in &matched_ids {
            if let Some(content) = self.content_cache.get(id) {
                candidates.push(SkillBudgetCandidate {
                    skill_id: id.clone(),
                    full_text: content.full_text.clone(),
                    full_tokens: content.token_estimate,
                    truncated_text: content.truncated_text.clone(),
                    truncated_tokens: content.token_estimate / 2,
                    name_only: content.name_only.clone(),
                    name_tokens: 10,
                    relevance: content.relevance as f64,
                    prior_utility: 1.0,
                });
            }
        }

        // 4. Run knapsack allocation under budget.
        let allocation = allocate_skill_budget(&candidates, self.skill_token_budget);

        // 5. Build injection result.
        let mut segments = Vec::new();
        let mut excluded = Vec::new();
        let mut downgraded = Vec::new();
        let mut total_tokens = 0u64;

        // Build a map from skill_id to inclusion level
        let level_map: HashMap<String, InclusionLevel> = allocation.allocations
            .iter()
            .cloned()
            .collect();

        for candidate in &candidates {
            let level = level_map.get(&candidate.skill_id).copied().unwrap_or(InclusionLevel::Excluded);
            match level {
                InclusionLevel::Excluded => {
                    excluded.push(candidate.skill_id.clone());
                }
                InclusionLevel::NameOnly => {
                    downgraded.push(candidate.skill_id.clone());
                    let name = self.kernel.get(&candidate.skill_id)
                        .map(|s| s.name.clone())
                        .unwrap_or_else(|| candidate.skill_id.clone());
                    segments.push(SkillSegment {
                        skill_id: candidate.skill_id.clone(),
                        skill_name: name,
                        content: candidate.name_only.clone(),
                        inclusion_level: InclusionLevel::NameOnly,
                        tokens: candidate.name_tokens,
                    });
                    total_tokens += candidate.name_tokens;
                }
                InclusionLevel::Truncated => {
                    let name = self.kernel.get(&candidate.skill_id)
                        .map(|s| s.name.clone())
                        .unwrap_or_else(|| candidate.skill_id.clone());
                    segments.push(SkillSegment {
                        skill_id: candidate.skill_id.clone(),
                        skill_name: name,
                        content: candidate.truncated_text.clone(),
                        inclusion_level: InclusionLevel::Truncated,
                        tokens: candidate.truncated_tokens,
                    });
                    total_tokens += candidate.truncated_tokens;
                }
                InclusionLevel::Full => {
                    let name = self.kernel.get(&candidate.skill_id)
                        .map(|s| s.name.clone())
                        .unwrap_or_else(|| candidate.skill_id.clone());
                    segments.push(SkillSegment {
                        skill_id: candidate.skill_id.clone(),
                        skill_name: name,
                        content: candidate.full_text.clone(),
                        inclusion_level: InclusionLevel::Full,
                        tokens: candidate.full_tokens,
                    });
                    total_tokens += candidate.full_tokens;
                }
            }
        }

        SkillInjection {
            prompt_segments: segments,
            total_tokens,
            excluded,
            downgraded,
        }
    }

    /// Validate a skill before execution (checks trust, budget, schema).
    pub fn validate_skill(
        &self,
        skill_id: &str,
        params: &serde_json::Value,
        available_budget: u64,
    ) -> SkillValidation {
        self.kernel.validate(skill_id, params, available_budget)
    }

    /// Get a compiled skill by ID.
    pub fn get_skill(&self, id: &str) -> Option<&CompiledSkill> {
        self.kernel.get(id)
    }

    /// Number of registered skills.
    pub fn skill_count(&self) -> usize {
        self.kernel.count()
    }

    /// Format activated skill injection as a prompt string.
    pub fn format_injection(injection: &SkillInjection) -> String {
        if injection.prompt_segments.is_empty() {
            return String::new();
        }

        let mut prompt = String::from("\n## Active Skills\n\n");
        for segment in &injection.prompt_segments {
            match segment.inclusion_level {
                InclusionLevel::Full | InclusionLevel::Truncated => {
                    prompt.push_str(&format!(
                        "### {} ({})\n{}\n\n",
                        segment.skill_name,
                        if matches!(segment.inclusion_level, InclusionLevel::Truncated) {
                            "summary"
                        } else {
                            "full"
                        },
                        segment.content
                    ));
                }
                InclusionLevel::NameOnly => {
                    prompt.push_str(&format!("- {} (available on request)\n", segment.skill_name));
                }
                InclusionLevel::Excluded => {}
            }
        }
        prompt
    }
}

/// Extract path patterns from a skill trigger for activation indexing.
fn extract_path_patterns(trigger: &SkillTrigger) -> Vec<String> {
    match trigger {
        SkillTrigger::PathPattern { patterns } => patterns.clone(),
        SkillTrigger::All { triggers } | SkillTrigger::Any { triggers } => {
            triggers.iter().flat_map(|t| extract_path_patterns(t)).collect()
        }
        _ => vec![],
    }
}

/// Extract language patterns from a skill trigger.
fn extract_language_patterns(trigger: &SkillTrigger) -> Vec<String> {
    match trigger {
        SkillTrigger::Language { languages } => languages.clone(),
        SkillTrigger::All { triggers } | SkillTrigger::Any { triggers } => {
            triggers.iter().flat_map(|t| extract_language_patterns(t)).collect()
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill_kernel::SkillSandbox;

    fn test_skill(id: &str, name: &str, trigger: SkillTrigger) -> CompiledSkill {
        CompiledSkill {
            id: id.to_string(),
            name: name.to_string(),
            description: format!("Test skill: {}", name),
            source: PathBuf::from(format!(".pipit/skills/{}.md", id)),
            trigger,
            template: format!("Instructions for {}", name),
            input_schema: None,
            output_schema: None,
            sandbox: SkillSandbox::default(),
            execution_mode: ExecutionMode::Inline,
            priority: 0,
            trust_tier: SkillTrustTier::ProjectLocal,
        }
    }

    fn test_content(name: &str) -> SkillContent {
        SkillContent {
            full_text: format!("Full skill content for {}", name),
            truncated_text: format!("Summary of {}", name),
            name_only: name.to_string(),
            token_estimate: 100,
            relevance: 0.8,
        }
    }

    #[test]
    fn skill_runtime_activation() {
        let mut runtime = SkillRuntime::new(SkillTrustTier::Community, 10_000);

        // Register an always-active skill
        let always = test_skill("fmt", "Formatting", SkillTrigger::Always);
        runtime.register_skill(always, test_content("Formatting"), ActivationScope::Project);

        // Register a Rust-specific skill
        let rust_skill = test_skill(
            "rust-testing",
            "Rust Testing",
            SkillTrigger::Language { languages: vec!["rust".into()] },
        );
        runtime.register_skill(rust_skill, test_content("Rust Testing"), ActivationScope::Project);

        assert_eq!(runtime.skill_count(), 2);

        // Activate for Rust context
        let injection = runtime.activate_for_context(&["src/main.rs"], &["rust"]);
        assert!(!injection.prompt_segments.is_empty());
        // At minimum the always-active skill should be present
        let ids: Vec<&str> = injection.prompt_segments.iter()
            .map(|s| s.skill_id.as_str())
            .collect();
        assert!(ids.contains(&"fmt"));
    }

    #[test]
    fn skill_budget_constrains_injection() {
        let mut runtime = SkillRuntime::new(SkillTrustTier::Community, 50); // very tight budget

        for i in 0..5 {
            let skill = test_skill(
                &format!("skill-{}", i),
                &format!("Skill {}", i),
                SkillTrigger::Always,
            );
            let mut content = test_content(&format!("Skill {}", i));
            content.token_estimate = 30; // each skill costs 30 tokens
            runtime.register_skill(skill, content, ActivationScope::Project);
        }

        let injection = runtime.activate_for_context(&[], &[]);
        // With budget=50 and each skill=30 tokens, not all can be Full
        assert!(injection.total_tokens <= 60); // some may be downgraded
    }

    #[test]
    fn format_injection_produces_valid_markdown() {
        let injection = SkillInjection {
            prompt_segments: vec![
                SkillSegment {
                    skill_id: "test".into(),
                    skill_name: "Testing".into(),
                    content: "Always run tests".into(),
                    inclusion_level: InclusionLevel::Full,
                    tokens: 10,
                },
            ],
            total_tokens: 10,
            excluded: vec![],
            downgraded: vec![],
        };
        let output = SkillRuntime::format_injection(&injection);
        assert!(output.contains("## Active Skills"));
        assert!(output.contains("### Testing (full)"));
    }
}
