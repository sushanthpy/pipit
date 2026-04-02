//! Adaptive Deliberation Scheduler
//!
//! Replaces coarse "thinking mode" toggles with an automatic elasticity
//! engine. Decides when to spend more compute by measuring uncertainty,
//! tool risk, retrieval entropy, and plan branching factor.
//!
//! Optimization: d* = argmax_d [E[value(d)] - λ·cost(d)]
//! Continue deliberation while Δp·L_error > c.

use serde::{Deserialize, Serialize};

/// Deliberation depth level — how much thinking the model should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DeliberationDepth {
    /// Minimal thinking — fast responses for simple queries.
    Shallow,
    /// Standard thinking — normal agentic reasoning.
    Standard,
    /// Deep thinking — extended reasoning for complex problems.
    Deep,
    /// Maximum thinking — exhaustive analysis with multiple passes.
    Exhaustive,
}

impl DeliberationDepth {
    /// Approximate token multiplier for this depth level.
    pub fn token_multiplier(&self) -> f64 {
        match self {
            Self::Shallow => 0.5,
            Self::Standard => 1.0,
            Self::Deep => 2.5,
            Self::Exhaustive => 5.0,
        }
    }

    /// Whether this depth enables extended thinking features.
    pub fn enables_extended_thinking(&self) -> bool {
        matches!(self, Self::Deep | Self::Exhaustive)
    }
}

/// Signals used to determine deliberation depth.
#[derive(Debug, Clone, Default)]
pub struct DeliberationSignals {
    /// Answer uncertainty score (0.0 = certain, 1.0 = very uncertain).
    pub uncertainty: f64,
    /// Tool side-effect risk (0.0 = pure read, 1.0 = destructive).
    pub tool_risk: f64,
    /// Retrieval disagreement (0.0 = consistent, 1.0 = contradictory sources).
    pub retrieval_entropy: f64,
    /// Plan branching factor — how many viable approaches exist.
    pub branching_factor: f64,
    /// Number of failed verification attempts so far.
    pub failed_verifications: u32,
    /// Whether the task involves multiple files.
    pub multi_file: bool,
    /// Whether the task involves architectural changes.
    pub architectural: bool,
    /// Expected rework cost if the answer is wrong (token estimate).
    pub rework_cost: u64,
    /// Token budget remaining.
    pub budget_remaining: u64,
    /// User explicitly requested deeper thinking.
    pub user_override: Option<DeliberationDepth>,
}

/// Configuration for the deliberation scheduler.
#[derive(Debug, Clone)]
pub struct DeliberationConfig {
    /// Cost per extra thinking pass (in tokens).
    pub pass_cost: u64,
    /// Risk multiplier: λ in the cost function.
    pub risk_lambda: f64,
    /// Minimum uncertainty to trigger deeper thinking.
    pub uncertainty_threshold: f64,
    /// Minimum tool risk to trigger deeper thinking.
    pub risk_threshold: f64,
    /// Maximum passes before stopping.
    pub max_passes: u32,
    /// Whether adaptive scheduling is enabled.
    pub enabled: bool,
}

impl Default for DeliberationConfig {
    fn default() -> Self {
        Self {
            pass_cost: 2000,
            risk_lambda: 0.3,
            uncertainty_threshold: 0.5,
            risk_threshold: 0.6,
            max_passes: 4,
            enabled: true,
        }
    }
}

/// The deliberation scheduler — decides how deep to think.
pub struct DeliberationScheduler {
    config: DeliberationConfig,
}

impl DeliberationScheduler {
    pub fn new(config: DeliberationConfig) -> Self {
        Self { config }
    }

    /// Determine the optimal deliberation depth for the current turn.
    ///
    /// Uses optimal stopping: continue while Δp·L_error > c.
    /// Returns the recommended depth and the computed priority score.
    pub fn schedule(&self, signals: &DeliberationSignals) -> (DeliberationDepth, f64) {
        // User override takes priority
        if let Some(override_depth) = signals.user_override {
            return (override_depth, 1.0);
        }

        if !self.config.enabled {
            return (DeliberationDepth::Standard, 0.5);
        }

        // Compute priority score: weighted combination of signals
        let priority = self.compute_priority(signals);

        // Map priority to depth level using cost-benefit analysis
        let depth = self.optimal_depth(priority, signals);

        (depth, priority)
    }

    /// Compute a priority score from deliberation signals.
    ///
    /// p(e) = w_uncertainty·uncertainty + w_risk·tool_risk
    ///      + w_entropy·retrieval_entropy + w_branch·branching
    ///      + w_rework·rework_factor + w_failure·failure_boost
    fn compute_priority(&self, signals: &DeliberationSignals) -> f64 {
        let mut score = 0.0;

        // Uncertainty contributes most — uncertain answers need more thought
        score += 0.30 * signals.uncertainty;

        // Tool risk — destructive operations deserve more planning
        score += 0.25 * signals.tool_risk;

        // Retrieval entropy — contradictory context needs reconciliation
        score += 0.15 * signals.retrieval_entropy;

        // Branching factor — multiple approaches need comparison
        score += 0.10 * (signals.branching_factor / 5.0).min(1.0);

        // Failed verifications — repeated failures indicate need for deeper analysis
        score += 0.10 * (signals.failed_verifications as f64 / 3.0).min(1.0);

        // Structural complexity indicators
        if signals.multi_file {
            score += 0.05;
        }
        if signals.architectural {
            score += 0.05;
        }

        score.clamp(0.0, 1.0)
    }

    /// Choose optimal depth using cost-benefit analysis.
    ///
    /// For each depth level d, compute:
    ///   E[value(d)] = priority · quality_gain(d)
    ///   cost(d) = λ · token_multiplier(d) · pass_cost
    /// Choose d* = argmax_d [E[value] - cost]
    fn optimal_depth(
        &self,
        priority: f64,
        signals: &DeliberationSignals,
    ) -> DeliberationDepth {
        let candidates = [
            DeliberationDepth::Shallow,
            DeliberationDepth::Standard,
            DeliberationDepth::Deep,
            DeliberationDepth::Exhaustive,
        ];

        let mut best_depth = DeliberationDepth::Standard;
        let mut best_utility = f64::NEG_INFINITY;

        let rework_cost = signals.rework_cost as f64;

        for &depth in &candidates {
            let multiplier = depth.token_multiplier();
            let token_cost = multiplier * self.config.pass_cost as f64;

            // Don't exceed budget
            if token_cost as u64 > signals.budget_remaining / 2 {
                continue;
            }

            // Quality gain: diminishing returns — deeper thinking helps more
            // when priority is high, less when it's low
            let quality_gain = match depth {
                DeliberationDepth::Shallow => 0.3,
                DeliberationDepth::Standard => 0.6,
                DeliberationDepth::Deep => 0.85,
                DeliberationDepth::Exhaustive => 0.95,
            };

            // Expected value: how much rework cost we avoid
            let expected_value = priority * quality_gain * rework_cost.max(1000.0);

            // Cost: tokens * lambda
            let cost = self.config.risk_lambda * token_cost;

            let utility = expected_value - cost;

            if utility > best_utility {
                best_utility = utility;
                best_depth = depth;
            }
        }

        best_depth
    }

    /// Decide whether to continue deliberation after a pass.
    /// Returns true if another pass is cost-effective.
    ///
    /// Optimal stopping: continue while Δp·L_error > c
    pub fn should_continue(
        &self,
        current_pass: u32,
        error_reduction: f64, // Δp: how much error probability dropped this pass
        error_cost: f64,      // L_error: cost of being wrong
    ) -> bool {
        if current_pass >= self.config.max_passes {
            return false;
        }
        let marginal_benefit = error_reduction * error_cost;
        let marginal_cost = self.config.pass_cost as f64;
        marginal_benefit > marginal_cost
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_priority_gets_standard() {
        let scheduler = DeliberationScheduler::new(DeliberationConfig::default());
        let signals = DeliberationSignals {
            uncertainty: 0.1,
            tool_risk: 0.1,
            budget_remaining: 100_000,
            rework_cost: 1000,
            ..Default::default()
        };
        let (depth, _) = scheduler.schedule(&signals);
        assert!(depth <= DeliberationDepth::Standard);
    }

    #[test]
    fn high_risk_gets_deep() {
        let scheduler = DeliberationScheduler::new(DeliberationConfig::default());
        let signals = DeliberationSignals {
            uncertainty: 0.8,
            tool_risk: 0.9,
            budget_remaining: 100_000,
            rework_cost: 50_000,
            failed_verifications: 2,
            ..Default::default()
        };
        let (depth, _) = scheduler.schedule(&signals);
        assert!(depth >= DeliberationDepth::Deep);
    }

    #[test]
    fn user_override_respected() {
        let scheduler = DeliberationScheduler::new(DeliberationConfig::default());
        let signals = DeliberationSignals {
            user_override: Some(DeliberationDepth::Exhaustive),
            ..Default::default()
        };
        let (depth, _) = scheduler.schedule(&signals);
        assert_eq!(depth, DeliberationDepth::Exhaustive);
    }
}
