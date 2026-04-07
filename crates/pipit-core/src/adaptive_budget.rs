//! Adaptive Turn Budget — predictive completion system.
//!
//! Instead of a hard turn ceiling, this module provides intelligent
//! turn budget decisions based on task progress signals:
//!
//! 1. **Progress velocity:** Are we making forward progress (file mutations)?
//! 2. **Task completion estimate:** Based on evidence, how close are we?
//! 3. **Diminishing returns detector:** Is each turn producing less value?
//! 4. **LLM judge (optional):** Ask a cheap model "is this task done?"
//!
//! The adaptive budget replaces the dumb counter with a decision function:
//!   extend(turn, evidence, mutations) → Continue(n) | WindDown | Stop

use serde::{Deserialize, Serialize};

/// Decision from the adaptive turn budget.
#[derive(Debug, Clone)]
pub enum TurnBudgetDecision {
    /// Continue normally — plenty of budget remaining.
    Continue,
    /// Approaching limit — warn the model to wrap up within N turns.
    WindDown { turns_remaining: u32 },
    /// Grant extension — task is making progress and appears incomplete.
    Extend {
        extra_turns: u32,
        reason: String,
    },
    /// Stop — either completed or no progress.
    Stop { reason: String },
}

/// Signals collected per turn for budget decisions.
#[derive(Debug, Clone, Default)]
pub struct TurnSignals {
    /// Number of files mutated this turn.
    pub files_mutated: u32,
    /// Number of tool calls this turn.
    pub tool_calls: u32,
    /// Whether any tool error occurred.
    pub had_error: bool,
    /// Number of files mutated across all turns.
    pub total_files_mutated: u32,
    /// Number of unique files modified in the session.
    pub unique_files_modified: u32,
    /// Consecutive turns with no file mutation.
    pub idle_turns: u32,
    /// Whether the model's response contained a "done" signal.
    pub model_signaled_done: bool,
    /// Whether verification (test/lint) passed this turn.
    pub verification_passed: bool,
}

/// Adaptive turn budget controller.
///
/// Replaces the fixed `max_turns + GRACE_TURNS` ceiling with a
/// dynamic budget that extends based on task progress.
#[derive(Debug, Clone)]
pub struct AdaptiveTurnBudget {
    /// The user-configured base turn limit.
    pub base_limit: u32,
    /// Maximum total turns (hard safety ceiling, even with extensions).
    pub hard_ceiling: u32,
    /// Current approved budget (starts at base_limit, can grow).
    pub approved_budget: u32,
    /// Total extensions granted so far.
    pub extensions_granted: u32,
    /// Maximum extensions before requiring LLM judge.
    pub max_auto_extensions: u32,
    /// Turn history for velocity analysis.
    pub turn_history: Vec<TurnSignals>,
    /// Whether the wind-down warning has been sent.
    pub winddown_warned: bool,
}

impl AdaptiveTurnBudget {
    pub fn new(base_limit: u32) -> Self {
        // Hard ceiling: 5x the base limit, capped at 200
        let hard_ceiling = (base_limit * 5).min(200);

        Self {
            base_limit,
            hard_ceiling,
            approved_budget: base_limit,
            extensions_granted: 0,
            max_auto_extensions: 3,
            turn_history: Vec::new(),
            winddown_warned: false,
        }
    }

    /// Record signals from the completed turn.
    pub fn record_turn(&mut self, signals: TurnSignals) {
        self.turn_history.push(signals);
    }

    /// Evaluate the turn budget at the current turn.
    /// Returns a decision about whether to continue, extend, or stop.
    pub fn evaluate(&mut self, current_turn: u32) -> TurnBudgetDecision {
        let signals = self.aggregate_signals();

        // Well under budget — continue normally
        if current_turn < self.approved_budget.saturating_sub(5) {
            return TurnBudgetDecision::Continue;
        }

        // Approaching budget — wind-down warning
        if current_turn >= self.approved_budget.saturating_sub(5)
            && current_turn < self.approved_budget.saturating_sub(2)
            && !self.winddown_warned
        {
            self.winddown_warned = true;
            let remaining = self.approved_budget.saturating_sub(current_turn);
            return TurnBudgetDecision::WindDown { turns_remaining: remaining };
        }

        // At or past budget — decide whether to extend
        if current_turn >= self.approved_budget {
            return self.decide_extension(current_turn, &signals);
        }

        // Near budget but not yet there
        TurnBudgetDecision::Continue
    }

    /// Decide whether to grant a budget extension.
    fn decide_extension(&mut self, current_turn: u32, signals: &AggregateSignals) -> TurnBudgetDecision {
        // Hard ceiling — never exceed
        if current_turn >= self.hard_ceiling {
            return TurnBudgetDecision::Stop {
                reason: format!(
                    "Hard ceiling reached ({} turns). Task used {}x the base budget.",
                    self.hard_ceiling,
                    self.hard_ceiling / self.base_limit.max(1)
                ),
            };
        }

        // No progress in last 5 turns — stop (model is stuck)
        if signals.idle_streak >= 5 {
            return TurnBudgetDecision::Stop {
                reason: format!(
                    "No file mutations in {} consecutive turns. Model appears stuck.",
                    signals.idle_streak
                ),
            };
        }

        // Model explicitly signaled completion
        if signals.last_turn_done_signal {
            return TurnBudgetDecision::Stop {
                reason: "Model indicated task completion.".into(),
            };
        }

        // Verification passed and no more pending work
        if signals.last_verification_passed && signals.idle_streak >= 2 {
            return TurnBudgetDecision::Stop {
                reason: "Verification passed with no pending mutations.".into(),
            };
        }

        // Active progress — calculate extension size based on velocity
        let velocity = signals.mutation_velocity;
        let estimated_remaining = self.estimate_remaining_turns(signals);

        if velocity > 0.0 && estimated_remaining > 0 {
            let extension = estimated_remaining.min(10).max(3);
            if self.extensions_granted < self.max_auto_extensions {
                self.extensions_granted += 1;
                self.approved_budget = current_turn + extension;
                self.winddown_warned = false;

                return TurnBudgetDecision::Extend {
                    extra_turns: extension,
                    reason: format!(
                        "Active progress: {:.1} mutations/turn, ~{} turns remaining. Extension {}/{}.",
                        velocity,
                        estimated_remaining,
                        self.extensions_granted,
                        self.max_auto_extensions,
                    ),
                };
            }
        }

        // Auto-extensions exhausted but still making progress —
        // this is where the LLM judge would be called
        if velocity > 0.3 && self.extensions_granted >= self.max_auto_extensions {
            // One final extension with strong wind-down
            self.approved_budget = current_turn + 5;
            return TurnBudgetDecision::Extend {
                extra_turns: 5,
                reason: format!(
                    "Final extension: still making progress ({:.1} mutations/turn) \
                     but auto-extension budget exhausted. Wrapping up.",
                    velocity,
                ),
            };
        }

        // No progress and no extensions — stop
        TurnBudgetDecision::Stop {
            reason: format!(
                "Turn limit reached ({} turns, {} extensions). \
                 Velocity: {:.1} mutations/turn.",
                current_turn, self.extensions_granted, velocity,
            ),
        }
    }

    /// Estimate how many more turns are needed based on task trajectory.
    fn estimate_remaining_turns(&self, signals: &AggregateSignals) -> u32 {
        if signals.mutation_velocity <= 0.0 {
            return 0;
        }

        // Heuristic: if the model is still creating files at a steady rate,
        // estimate remaining turns proportional to recent velocity.
        // Assume tasks that create many files need proportionally more turns.
        let files_so_far = signals.total_unique_files;
        let recent_rate = signals.mutation_velocity;

        if recent_rate > 0.5 {
            // High velocity — probably still in the middle of creation
            // Estimate: ~50% more time proportional to what's been used
            let used_turns = self.turn_history.len() as u32;
            (used_turns / 3).max(3).min(15)
        } else if recent_rate > 0.2 {
            // Moderate velocity — winding down
            5
        } else {
            // Low velocity — nearly done
            3
        }
    }

    /// Aggregate signals from turn history for decision-making.
    fn aggregate_signals(&self) -> AggregateSignals {
        let window = 5; // Look at last 5 turns
        let recent = &self.turn_history[self.turn_history.len().saturating_sub(window)..];

        let mutations_in_window: u32 = recent.iter().map(|s| s.files_mutated).sum();
        let mutation_velocity = if recent.is_empty() {
            0.0
        } else {
            mutations_in_window as f64 / recent.len() as f64
        };

        let idle_streak = self.turn_history.iter().rev()
            .take_while(|s| s.files_mutated == 0)
            .count() as u32;

        let last_turn = self.turn_history.last();
        let last_turn_done_signal = last_turn.map(|s| s.model_signaled_done).unwrap_or(false);
        let last_verification_passed = last_turn.map(|s| s.verification_passed).unwrap_or(false);

        let total_unique_files: u32 = last_turn.map(|s| s.unique_files_modified).unwrap_or(0);

        AggregateSignals {
            mutation_velocity,
            idle_streak,
            last_turn_done_signal,
            last_verification_passed,
            total_unique_files,
        }
    }

    /// Build the system message for the wind-down warning.
    pub fn wind_down_message(turns_remaining: u32) -> String {
        format!(
            "[SYSTEM] You have approximately {} turns remaining. \
             Start wrapping up: finish current edits, run verification if needed, \
             and prepare to conclude. If the task requires more work, prioritize \
             the most critical remaining items.",
            turns_remaining,
        )
    }

    /// Build the system message for a budget extension.
    pub fn extension_message(extra_turns: u32, reason: &str) -> String {
        format!(
            "[SYSTEM] Turn budget extended by {} turns. Reason: {} \
             Continue working, but focus on completing the most important \
             remaining items. You will be warned again as the new limit approaches.",
            extra_turns, reason,
        )
    }

    /// Build the system message for the final extension (LLM judge equivalent).
    pub fn final_extension_message(extra_turns: u32) -> String {
        format!(
            "[SYSTEM] FINAL extension: {} bonus turns granted based on progress analysis. \
             This is your last extension. You MUST complete or checkpoint your work now. \
             Priorities: 1) Finish any half-written files, 2) Run verification, \
             3) Report what's done and what remains.",
            extra_turns,
        )
    }
}

/// Aggregated metrics from turn history.
#[derive(Debug)]
struct AggregateSignals {
    /// Average file mutations per turn in the recent window.
    mutation_velocity: f64,
    /// Consecutive turns with zero mutations at the end.
    idle_streak: u32,
    /// Whether the last turn's model response signaled "done".
    last_turn_done_signal: bool,
    /// Whether the last turn's verification passed.
    last_verification_passed: bool,
    /// Total unique files modified in the session.
    total_unique_files: u32,
}

/// Detect if a model's response text signals task completion.
/// Looks for phrases like "task is complete", "all done", "finished implementing".
pub fn detect_completion_signal(response_text: &str) -> bool {
    let lower = response_text.to_lowercase();
    let completion_phrases = [
        "task is complete",
        "task is done",
        "implementation is complete",
        "all files have been created",
        "the application is ready",
        "all done",
        "finished implementing",
        "setup is complete",
        "everything is working",
        "project is complete",
        "all changes have been made",
        "i have completed",
        "the work is done",
    ];
    completion_phrases.iter().any(|phrase| lower.contains(phrase))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_budget_defaults() {
        let budget = AdaptiveTurnBudget::new(10);
        assert_eq!(budget.base_limit, 10);
        assert_eq!(budget.approved_budget, 10);
        assert_eq!(budget.hard_ceiling, 50);
        assert_eq!(budget.extensions_granted, 0);
    }

    #[test]
    fn continue_when_under_budget() {
        let mut budget = AdaptiveTurnBudget::new(20);
        assert!(matches!(budget.evaluate(5), TurnBudgetDecision::Continue));
    }

    #[test]
    fn winddown_near_limit() {
        let mut budget = AdaptiveTurnBudget::new(20);
        // Add some history
        for _ in 0..15 {
            budget.record_turn(TurnSignals { files_mutated: 1, ..Default::default() });
        }
        let decision = budget.evaluate(16);
        assert!(matches!(decision, TurnBudgetDecision::WindDown { .. }));
    }

    #[test]
    fn extend_with_active_progress() {
        let mut budget = AdaptiveTurnBudget::new(10);
        // Record 10 turns of active progress
        for _ in 0..10 {
            budget.record_turn(TurnSignals {
                files_mutated: 2,
                unique_files_modified: 5,
                ..Default::default()
            });
        }
        let decision = budget.evaluate(10);
        assert!(matches!(decision, TurnBudgetDecision::Extend { .. }));
    }

    #[test]
    fn stop_when_idle() {
        let mut budget = AdaptiveTurnBudget::new(10);
        // Record 10 turns with last 5 idle
        for i in 0..10 {
            budget.record_turn(TurnSignals {
                files_mutated: if i < 5 { 1 } else { 0 },
                ..Default::default()
            });
        }
        let decision = budget.evaluate(10);
        assert!(matches!(decision, TurnBudgetDecision::Stop { .. }));
    }

    #[test]
    fn stop_at_hard_ceiling() {
        let mut budget = AdaptiveTurnBudget::new(10);
        // Simulate running to the hard ceiling (50 turns for base_limit=10)
        for _ in 0..50 {
            budget.record_turn(TurnSignals { files_mutated: 1, ..Default::default() });
        }
        // Keep extending until we can't
        for t in 10..60 {
            let d = budget.evaluate(t);
            if t >= 50 {
                // At or past hard ceiling — must stop
                assert!(matches!(d, TurnBudgetDecision::Stop { .. }),
                    "Expected Stop at turn {}, got {:?}", t, d);
                break;
            }
        }
    }

    #[test]
    fn detect_completion_phrases() {
        assert!(detect_completion_signal("The task is complete. All files have been created."));
        assert!(detect_completion_signal("I have completed the implementation."));
        assert!(!detect_completion_signal("I need to continue working on the frontend."));
        assert!(!detect_completion_signal("Let me read the file first."));
    }

    #[test]
    fn multiple_extensions_then_cap() {
        let mut budget = AdaptiveTurnBudget::new(10);
        // Simulate active progress
        for _ in 0..30 {
            budget.record_turn(TurnSignals {
                files_mutated: 1,
                unique_files_modified: 10,
                ..Default::default()
            });
        }

        // First extension
        let d1 = budget.evaluate(10);
        assert!(matches!(d1, TurnBudgetDecision::Extend { .. }));

        // Simulate more turns
        for _ in 0..10 {
            budget.record_turn(TurnSignals { files_mutated: 1, ..Default::default() });
        }

        // Second extension
        let d2 = budget.evaluate(budget.approved_budget);
        assert!(matches!(d2, TurnBudgetDecision::Extend { .. }));

        // Third extension
        for _ in 0..5 {
            budget.record_turn(TurnSignals { files_mutated: 1, ..Default::default() });
        }
        let d3 = budget.evaluate(budget.approved_budget);
        assert!(matches!(d3, TurnBudgetDecision::Extend { .. }));

        // Fourth should still extend (final extension)
        for _ in 0..5 {
            budget.record_turn(TurnSignals { files_mutated: 1, ..Default::default() });
        }
        let d4 = budget.evaluate(budget.approved_budget);
        // Should be final extension or stop
        assert!(matches!(d4, TurnBudgetDecision::Extend { .. } | TurnBudgetDecision::Stop { .. }));
    }
}
