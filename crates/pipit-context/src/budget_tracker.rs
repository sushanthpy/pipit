//! Token-Budget Tracker with Continuation & Diminishing-Returns Detection
//!
//! Monitors per-turn token consumption, enforces cost/token ceilings,
//! detects when the model terminates early (under budget), and identifies
//! diminishing-returns cycles to prevent degenerate continuations.
//!
//! Memory: 3 scalars (24 bytes total). O(1) per turn.

use serde::{Deserialize, Serialize};

/// Decision from the budget tracker per turn.
#[derive(Debug, Clone, PartialEq)]
pub enum BudgetDecision {
    /// Continue normally — budget is healthy.
    Continue,
    /// Model stopped early but budget remains — inject continuation nudge.
    ContinueWithNudge {
        remaining_fraction: f64,
        nudge_message: String,
    },
    /// Diminishing returns detected — stop to prevent waste.
    StopDiminishing {
        continuation_count: u32,
        last_delta: u64,
    },
    /// Budget ceiling reached — stop immediately.
    BudgetExceeded {
        metric: BudgetMetric,
        used: f64,
        limit: f64,
    },
}

/// Which budget constraint was exceeded.
#[derive(Debug, Clone, PartialEq)]
pub enum BudgetMetric {
    Cost,
    InputTokens,
    OutputTokens,
    TotalTokens,
}

/// Budget ceiling configuration (for SDK/daemon mode).
#[derive(Debug, Clone, Default)]
pub struct BudgetCeiling {
    /// Maximum total cost in USD. 0 = no limit.
    pub max_cost_usd: f64,
    /// Maximum total tokens (input + output). 0 = no limit.
    pub max_total_tokens: u64,
    /// Maximum output tokens across all turns. 0 = no limit.
    pub max_output_tokens: u64,
}

/// Configuration for the budget tracker.
#[derive(Debug, Clone)]
pub struct BudgetTrackerConfig {
    /// Budget ceiling (optional).
    pub ceiling: BudgetCeiling,
    /// Fraction of budget consumed before triggering continuation nudge.
    pub completion_threshold: f64,
    /// Minimum output tokens per turn to consider "productive".
    pub diminishing_threshold: u64,
    /// Maximum continuation nudges before giving up.
    pub max_continuations: u32,
}

impl Default for BudgetTrackerConfig {
    fn default() -> Self {
        Self {
            ceiling: BudgetCeiling::default(),
            completion_threshold: 0.9,
            diminishing_threshold: 500,
            max_continuations: 5,
        }
    }
}

/// Per-session budget tracker.
///
/// Tracks cumulative token usage and cost, detects premature model stops,
/// and identifies diminishing-returns continuation cycles.
///
/// Memory: 24 bytes of tracking state (3 scalars).
pub struct BudgetTracker {
    config: BudgetTrackerConfig,
    /// Number of continuation nudges sent in current sequence.
    continuation_count: u32,
    /// Output tokens produced in the previous turn (for delta analysis).
    last_delta_tokens: u64,
    /// Total output tokens across all turns.
    total_output_tokens: u64,
    /// Total input tokens across all turns.
    total_input_tokens: u64,
    /// Total cost accumulated.
    total_cost: f64,
}

impl BudgetTracker {
    pub fn new(config: BudgetTrackerConfig) -> Self {
        Self {
            config,
            continuation_count: 0,
            last_delta_tokens: 0,
            total_output_tokens: 0,
            total_input_tokens: 0,
            total_cost: 0.0,
        }
    }

    /// Record a completed turn and return the budget decision.
    ///
    /// - `output_tokens`: tokens produced by the model this turn.
    /// - `input_tokens`: tokens consumed as input this turn.
    /// - `cost`: cost of this turn in USD.
    /// - `model_stopped_naturally`: true if the model chose to stop (EndTurn),
    ///   false if it hit max_tokens.
    pub fn record_turn(
        &mut self,
        output_tokens: u64,
        input_tokens: u64,
        cost: f64,
        model_stopped_naturally: bool,
    ) -> BudgetDecision {
        self.total_output_tokens += output_tokens;
        self.total_input_tokens += input_tokens;
        self.total_cost += cost;

        // Check ceilings first
        if let Some(exceeded) = self.check_ceiling() {
            return exceeded;
        }

        // If model stopped naturally, check if we should nudge continuation
        if model_stopped_naturally {
            return self.evaluate_continuation(output_tokens);
        }

        // Model hit max_tokens — reset continuation tracking
        self.continuation_count = 0;
        self.last_delta_tokens = output_tokens;
        BudgetDecision::Continue
    }

    /// Check if any budget ceiling has been exceeded.
    fn check_ceiling(&self) -> Option<BudgetDecision> {
        let c = &self.config.ceiling;

        if c.max_cost_usd > 0.0 && self.total_cost >= c.max_cost_usd {
            return Some(BudgetDecision::BudgetExceeded {
                metric: BudgetMetric::Cost,
                used: self.total_cost,
                limit: c.max_cost_usd,
            });
        }

        if c.max_output_tokens > 0 && self.total_output_tokens >= c.max_output_tokens {
            return Some(BudgetDecision::BudgetExceeded {
                metric: BudgetMetric::OutputTokens,
                used: self.total_output_tokens as f64,
                limit: c.max_output_tokens as f64,
            });
        }

        let total = self.total_input_tokens + self.total_output_tokens;
        if c.max_total_tokens > 0 && total >= c.max_total_tokens {
            return Some(BudgetDecision::BudgetExceeded {
                metric: BudgetMetric::TotalTokens,
                used: total as f64,
                limit: c.max_total_tokens as f64,
            });
        }

        None
    }

    /// Evaluate whether to continue after the model stopped naturally.
    fn evaluate_continuation(&mut self, current_delta: u64) -> BudgetDecision {
        let budget_total = if self.config.ceiling.max_output_tokens > 0 {
            self.config.ceiling.max_output_tokens
        } else {
            // No ceiling — don't nudge continuation
            self.continuation_count = 0;
            self.last_delta_tokens = current_delta;
            return BudgetDecision::Continue;
        };

        let fraction = self.total_output_tokens as f64 / budget_total as f64;

        // Diminishing returns: last two deltas both below threshold
        let is_diminishing = self.continuation_count >= 3
            && current_delta < self.config.diminishing_threshold
            && self.last_delta_tokens < self.config.diminishing_threshold;

        if is_diminishing {
            let decision = BudgetDecision::StopDiminishing {
                continuation_count: self.continuation_count,
                last_delta: current_delta,
            };
            self.continuation_count = 0;
            self.last_delta_tokens = current_delta;
            return decision;
        }

        // Under budget — nudge continuation
        if fraction < self.config.completion_threshold
            && self.continuation_count < self.config.max_continuations
        {
            self.continuation_count += 1;
            self.last_delta_tokens = current_delta;

            let remaining = 1.0 - fraction;
            return BudgetDecision::ContinueWithNudge {
                remaining_fraction: remaining,
                nudge_message: format!(
                    "[System: You have {:.0}% of your output budget remaining. \
                     Please continue with the task.]",
                    remaining * 100.0
                ),
            };
        }

        self.continuation_count = 0;
        self.last_delta_tokens = current_delta;
        BudgetDecision::Continue
    }

    /// Get a summary of current budget state.
    pub fn summary(&self) -> BudgetTrackerSummary {
        let total_tokens = self.total_input_tokens + self.total_output_tokens;
        let ceiling = &self.config.ceiling;
        let budget_fraction = if ceiling.max_total_tokens > 0 {
            total_tokens as f64 / ceiling.max_total_tokens as f64
        } else if ceiling.max_output_tokens > 0 {
            self.total_output_tokens as f64 / ceiling.max_output_tokens as f64
        } else {
            0.0
        };

        BudgetTrackerSummary {
            total_input_tokens: self.total_input_tokens,
            total_output_tokens: self.total_output_tokens,
            total_cost_usd: self.total_cost,
            budget_fraction_used: budget_fraction,
            continuation_count: self.continuation_count,
        }
    }

    /// Reset continuation state (e.g., on new user message).
    pub fn reset_continuation(&mut self) {
        self.continuation_count = 0;
        self.last_delta_tokens = 0;
    }
}

/// Summary of budget tracker state for telemetry/SDK output.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BudgetTrackerSummary {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost_usd: f64,
    pub budget_fraction_used: f64,
    pub continuation_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_ceiling_continues() {
        let mut tracker = BudgetTracker::new(BudgetTrackerConfig::default());
        let decision = tracker.record_turn(1000, 500, 0.01, true);
        assert_eq!(decision, BudgetDecision::Continue);
    }

    #[test]
    fn test_cost_ceiling_exceeded() {
        let mut tracker = BudgetTracker::new(BudgetTrackerConfig {
            ceiling: BudgetCeiling {
                max_cost_usd: 0.05,
                ..Default::default()
            },
            ..Default::default()
        });
        tracker.record_turn(1000, 500, 0.03, false);
        let decision = tracker.record_turn(1000, 500, 0.03, false);
        assert!(matches!(decision, BudgetDecision::BudgetExceeded { metric: BudgetMetric::Cost, .. }));
    }

    #[test]
    fn test_continuation_nudge() {
        let mut tracker = BudgetTracker::new(BudgetTrackerConfig {
            ceiling: BudgetCeiling {
                max_output_tokens: 10000,
                ..Default::default()
            },
            completion_threshold: 0.9,
            ..Default::default()
        });

        // First turn: 2000 tokens = 20% of budget → should nudge
        let decision = tracker.record_turn(2000, 500, 0.01, true);
        assert!(matches!(decision, BudgetDecision::ContinueWithNudge { .. }));
    }

    #[test]
    fn test_diminishing_returns() {
        let mut tracker = BudgetTracker::new(BudgetTrackerConfig {
            ceiling: BudgetCeiling {
                max_output_tokens: 10000,
                ..Default::default()
            },
            diminishing_threshold: 500,
            ..Default::default()
        });

        // Burn through continuations with small deltas
        for _ in 0..3 {
            tracker.record_turn(100, 500, 0.001, true);
        }
        // 4th small delta should trigger diminishing returns
        let decision = tracker.record_turn(100, 500, 0.001, true);
        assert!(matches!(decision, BudgetDecision::StopDiminishing { .. }));
    }
}
