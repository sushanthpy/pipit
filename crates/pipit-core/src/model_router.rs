//! # Cost-Optimal Model Routing (C2)
//!
//! Routes tasks to the cheapest model that can handle them with acceptable quality.
//! Uses a multi-armed bandit approach (Thompson sampling) to learn model capabilities
//! online from session feedback.
//!
//! ## Routing Strategy
//!
//! ```text
//! Task → classify(complexity) → ModelTier → select_model(tier, budget)
//!   Simple (grep, read)     → Tier1 (fast/cheap: Haiku, GPT-4o-mini)
//!   Standard (edit, test)   → Tier2 (balanced: Sonnet, GPT-4o)
//!   Complex (architect, debug) → Tier3 (frontier: Opus, o1)
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Task complexity classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskComplexity {
    /// Simple: read file, grep, list directory
    Simple,
    /// Standard: edit file, run tests, write code
    Standard,
    /// Complex: architectural decisions, multi-file refactors, debugging
    Complex,
    /// Critical: security review, production deployment
    Critical,
}

/// Model tier for routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelTier {
    /// Fast/cheap — for simple tasks
    Tier1,
    /// Balanced — for standard tasks
    Tier2,
    /// Frontier — for complex tasks
    Tier3,
}

/// A model available for routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutableModel {
    pub name: String,
    pub tier: ModelTier,
    pub cost_per_1m_input: f64,
    pub cost_per_1m_output: f64,
    pub max_context_tokens: u32,
    pub avg_latency_ms: u64,
    pub available: bool,
}

/// Thompson sampling statistics for a model.
#[derive(Debug, Clone)]
struct ModelStats {
    /// Beta distribution parameters (successes, failures).
    alpha: f64,
    beta: f64,
    /// Total tasks routed to this model.
    total_tasks: u32,
    /// Average cost per task.
    avg_cost: f64,
}

impl ModelStats {
    fn new() -> Self {
        Self {
            alpha: 1.0, // Prior: uniform
            beta: 1.0,
            total_tasks: 0,
            avg_cost: 0.0,
        }
    }

    /// Update with task outcome (success=true/false) and cost.
    fn update(&mut self, success: bool, cost: f64) {
        if success {
            self.alpha += 1.0;
        } else {
            self.beta += 1.0;
        }
        self.total_tasks += 1;
        let n = self.total_tasks as f64;
        self.avg_cost = self.avg_cost * (n - 1.0) / n + cost / n;
    }

    /// Sample from the posterior Beta(alpha, beta).
    /// Uses a simple approximation: mean + noise proportional to variance.
    fn sample(&self) -> f64 {
        let mean = self.alpha / (self.alpha + self.beta);
        let variance = (self.alpha * self.beta)
            / ((self.alpha + self.beta).powi(2) * (self.alpha + self.beta + 1.0));
        // Deterministic approximation for testing (real impl would use rand)
        mean + variance.sqrt() * 0.1
    }

    /// Expected success rate.
    fn expected_rate(&self) -> f64 {
        self.alpha / (self.alpha + self.beta)
    }
}

/// The cost-optimal model router.
pub struct ModelRouter {
    models: Vec<RoutableModel>,
    stats: HashMap<String, ModelStats>,
    /// Budget remaining in USD for this session.
    budget_remaining: f64,
    /// Task → tier mapping overrides.
    tier_overrides: HashMap<String, ModelTier>,
}

/// Routing decision.
#[derive(Debug, Clone)]
pub struct RoutingDecision {
    pub model: String,
    pub tier: ModelTier,
    pub estimated_cost: f64,
    pub confidence: f64,
    pub reason: String,
}

impl ModelRouter {
    pub fn new(models: Vec<RoutableModel>, budget: f64) -> Self {
        let stats: HashMap<String, ModelStats> = models
            .iter()
            .map(|m| (m.name.clone(), ModelStats::new()))
            .collect();
        Self {
            models,
            stats,
            budget_remaining: budget,
            tier_overrides: HashMap::new(),
        }
    }

    /// Classify task complexity based on the tool being called.
    pub fn classify_task(&self, tool_name: &str, context_tokens: u32) -> TaskComplexity {
        // Check overrides
        if let Some(tier) = self.tier_overrides.get(tool_name) {
            return match tier {
                ModelTier::Tier1 => TaskComplexity::Simple,
                ModelTier::Tier2 => TaskComplexity::Standard,
                ModelTier::Tier3 => TaskComplexity::Complex,
            };
        }

        match tool_name {
            "read_file" | "list_directory" | "grep" | "glob" => TaskComplexity::Simple,
            "edit_file" | "write_file" | "multi_edit" | "bash" => {
                if context_tokens > 50_000 {
                    TaskComplexity::Complex
                } else {
                    TaskComplexity::Standard
                }
            }
            "subagent" | "architect" => TaskComplexity::Complex,
            _ => TaskComplexity::Standard,
        }
    }

    /// Map task complexity to model tier.
    pub fn complexity_to_tier(&self, complexity: TaskComplexity) -> ModelTier {
        match complexity {
            TaskComplexity::Simple => ModelTier::Tier1,
            TaskComplexity::Standard => ModelTier::Tier2,
            TaskComplexity::Complex | TaskComplexity::Critical => ModelTier::Tier3,
        }
    }

    /// Route a task to the optimal model.
    pub fn route(
        &self,
        tool_name: &str,
        estimated_tokens: u32,
    ) -> Option<RoutingDecision> {
        let complexity = self.classify_task(tool_name, estimated_tokens);
        let tier = self.complexity_to_tier(complexity);

        let candidates: Vec<&RoutableModel> = self
            .models
            .iter()
            .filter(|m| m.available && m.tier == tier && m.max_context_tokens >= estimated_tokens)
            .collect();

        if candidates.is_empty() {
            // Fall back to any available model in a higher tier
            return self.fallback_route(estimated_tokens);
        }

        // Thompson sampling: pick the model with highest sampled success rate,
        // weighted by inverse cost.
        let best = candidates
            .iter()
            .map(|m| {
                let stats = self.stats.get(&m.name).unwrap();
                let quality_score = stats.sample();
                let cost_per_token = (m.cost_per_1m_input + m.cost_per_1m_output) / 2_000_000.0;
                let estimated_cost = cost_per_token * estimated_tokens as f64;
                let score = quality_score / (estimated_cost.max(0.0001));
                (m, score, estimated_cost, quality_score)
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        best.map(|(model, _, cost, confidence)| RoutingDecision {
            model: model.name.clone(),
            tier,
            estimated_cost: cost,
            confidence,
            reason: format!("complexity={:?}, tier={:?}", complexity, tier),
        })
    }

    fn fallback_route(&self, estimated_tokens: u32) -> Option<RoutingDecision> {
        self.models
            .iter()
            .filter(|m| m.available && m.max_context_tokens >= estimated_tokens)
            .next()
            .map(|m| RoutingDecision {
                model: m.name.clone(),
                tier: m.tier,
                estimated_cost: 0.0,
                confidence: 0.5,
                reason: "fallback: no model available in preferred tier".into(),
            })
    }

    /// Record task outcome for online learning.
    pub fn record_outcome(&mut self, model: &str, success: bool, cost: f64) {
        if let Some(stats) = self.stats.get_mut(model) {
            stats.update(success, cost);
        }
        self.budget_remaining -= cost;
    }

    /// Get remaining budget.
    pub fn budget_remaining(&self) -> f64 {
        self.budget_remaining
    }

    /// Get model statistics.
    pub fn model_stats(&self, model: &str) -> Option<(f64, u32)> {
        self.stats
            .get(model)
            .map(|s| (s.expected_rate(), s.total_tasks))
    }

    /// Override tier for a specific tool.
    pub fn set_tier_override(&mut self, tool: &str, tier: ModelTier) {
        self.tier_overrides.insert(tool.to_string(), tier);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_models() -> Vec<RoutableModel> {
        vec![
            RoutableModel {
                name: "haiku".into(),
                tier: ModelTier::Tier1,
                cost_per_1m_input: 0.25,
                cost_per_1m_output: 1.25,
                max_context_tokens: 200_000,
                avg_latency_ms: 500,
                available: true,
            },
            RoutableModel {
                name: "sonnet".into(),
                tier: ModelTier::Tier2,
                cost_per_1m_input: 3.0,
                cost_per_1m_output: 15.0,
                max_context_tokens: 200_000,
                avg_latency_ms: 2000,
                available: true,
            },
            RoutableModel {
                name: "opus".into(),
                tier: ModelTier::Tier3,
                cost_per_1m_input: 15.0,
                cost_per_1m_output: 75.0,
                max_context_tokens: 200_000,
                avg_latency_ms: 5000,
                available: true,
            },
        ]
    }

    #[test]
    fn classify_simple_tasks() {
        let router = ModelRouter::new(test_models(), 10.0);
        assert_eq!(router.classify_task("read_file", 1000), TaskComplexity::Simple);
        assert_eq!(router.classify_task("grep", 1000), TaskComplexity::Simple);
    }

    #[test]
    fn classify_complex_tasks() {
        let router = ModelRouter::new(test_models(), 10.0);
        assert_eq!(router.classify_task("subagent", 1000), TaskComplexity::Complex);
    }

    #[test]
    fn route_simple_to_tier1() {
        let router = ModelRouter::new(test_models(), 10.0);
        let decision = router.route("read_file", 1000).unwrap();
        assert_eq!(decision.tier, ModelTier::Tier1);
        assert_eq!(decision.model, "haiku");
    }

    #[test]
    fn route_complex_to_tier3() {
        let router = ModelRouter::new(test_models(), 10.0);
        let decision = router.route("subagent", 5000).unwrap();
        assert_eq!(decision.tier, ModelTier::Tier3);
        assert_eq!(decision.model, "opus");
    }

    #[test]
    fn record_outcome_updates_stats() {
        let mut router = ModelRouter::new(test_models(), 10.0);
        router.record_outcome("haiku", true, 0.001);
        router.record_outcome("haiku", true, 0.002);
        router.record_outcome("haiku", false, 0.001);

        let (rate, tasks) = router.model_stats("haiku").unwrap();
        assert_eq!(tasks, 3);
        assert!(rate > 0.5); // 2 successes, 1 failure + priors
    }

    #[test]
    fn budget_tracking() {
        let mut router = ModelRouter::new(test_models(), 1.0);
        router.record_outcome("haiku", true, 0.3);
        assert!((router.budget_remaining() - 0.7).abs() < 1e-10);
    }

    #[test]
    fn tier_override() {
        let mut router = ModelRouter::new(test_models(), 10.0);
        router.set_tier_override("bash", ModelTier::Tier3);
        assert_eq!(router.classify_task("bash", 100), TaskComplexity::Complex);
    }
}
