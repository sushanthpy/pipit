//! PEV Phase Timeout and Deadlock Prevention
//!
//! Per-phase wall-clock timeouts with escalation semantics.
//! Timeout budget is allocated proportionally:
//!   Plan: 10%, Execute: 70%, Verify: 20%
//! If a phase times out, remaining budget is redistributed.

use crate::pev::PevPhase;
use std::time::{Duration, Instant};

/// Default phase budget proportions.
const PLAN_PROPORTION: f64 = 0.10;
const EXECUTE_PROPORTION: f64 = 0.70;
const VERIFY_PROPORTION: f64 = 0.20;

/// Phase timeout configuration.
#[derive(Debug, Clone)]
pub struct PhaseTimeoutConfig {
    /// Total session timeout.
    pub total_timeout: Duration,
    /// Plan phase proportion (0.0–1.0).
    pub plan_proportion: f64,
    /// Execute phase proportion (0.0–1.0).
    pub execute_proportion: f64,
    /// Verify phase proportion (0.0–1.0).
    pub verify_proportion: f64,
}

impl Default for PhaseTimeoutConfig {
    fn default() -> Self {
        Self {
            total_timeout: Duration::from_secs(600), // 10 minutes
            plan_proportion: PLAN_PROPORTION,
            execute_proportion: EXECUTE_PROPORTION,
            verify_proportion: VERIFY_PROPORTION,
        }
    }
}

/// Tracks timeout budgets per phase with redistribution on timeout.
pub struct PhaseTimeoutTracker {
    config: PhaseTimeoutConfig,
    /// Session start time.
    session_start: Instant,
    /// Budget remaining for each phase.
    budgets: PhaseBudgets,
    /// Current phase start time.
    current_phase_start: Option<Instant>,
    /// Current phase.
    current_phase: Option<PevPhase>,
    /// Phases that have timed out.
    timed_out_phases: Vec<PevPhase>,
}

#[derive(Debug, Clone)]
struct PhaseBudgets {
    plan: Duration,
    execute: Duration,
    verify: Duration,
}

impl PhaseTimeoutTracker {
    pub fn new(config: PhaseTimeoutConfig) -> Self {
        let total = config.total_timeout;
        let budgets = PhaseBudgets {
            plan: Duration::from_secs_f64(total.as_secs_f64() * config.plan_proportion),
            execute: Duration::from_secs_f64(total.as_secs_f64() * config.execute_proportion),
            verify: Duration::from_secs_f64(total.as_secs_f64() * config.verify_proportion),
        };

        Self {
            config,
            session_start: Instant::now(),
            budgets,
            current_phase_start: None,
            current_phase: None,
            timed_out_phases: Vec::new(),
        }
    }

    /// Start tracking a phase.
    pub fn enter_phase(&mut self, phase: PevPhase) {
        // Commit time spent in previous phase
        if let (Some(start), Some(prev_phase)) = (self.current_phase_start, self.current_phase) {
            let elapsed = start.elapsed();
            self.deduct_from_phase(&prev_phase, elapsed);
        }

        self.current_phase = Some(phase);
        self.current_phase_start = Some(Instant::now());
    }

    /// Get the timeout duration for the current phase.
    pub fn current_phase_timeout(&self) -> Option<Duration> {
        self.current_phase
            .map(|phase| self.budget_for_phase(&phase))
    }

    /// Check if the current phase has exceeded its timeout.
    pub fn is_current_phase_timed_out(&self) -> bool {
        match (self.current_phase_start, self.current_phase) {
            (Some(start), Some(phase)) => start.elapsed() > self.budget_for_phase(&phase),
            _ => false,
        }
    }

    /// Handle a phase timeout: redistribute remaining budget.
    pub fn handle_timeout(&mut self) -> PhaseTimeoutAction {
        let phase = match self.current_phase {
            Some(p) => p,
            None => return PhaseTimeoutAction::Continue,
        };

        self.timed_out_phases.push(phase);
        let remaining = self.budget_for_phase(&phase).saturating_sub(
            self.current_phase_start
                .map(|s| s.elapsed())
                .unwrap_or_default(),
        );

        // Redistribute remaining budget to subsequent phases
        match phase {
            PevPhase::Planning => {
                // Redistribute to Execute and Verify proportionally
                let exec_share = remaining.mul_f64(
                    self.config.execute_proportion
                        / (self.config.execute_proportion + self.config.verify_proportion),
                );
                let verify_share = remaining.saturating_sub(exec_share);
                self.budgets.execute += exec_share;
                self.budgets.verify += verify_share;
                PhaseTimeoutAction::SkipToExecute
            }
            PevPhase::Executing => {
                // Give all remaining to Verify
                self.budgets.verify += remaining;
                PhaseTimeoutAction::SkipToVerify
            }
            PevPhase::Verifying => PhaseTimeoutAction::Escalate,
            PevPhase::Repairing => PhaseTimeoutAction::Escalate,
            _ => PhaseTimeoutAction::Continue,
        }
    }

    /// Get total session elapsed time.
    pub fn session_elapsed(&self) -> Duration {
        self.session_start.elapsed()
    }

    /// Check if the total session timeout has been exceeded.
    pub fn is_session_timed_out(&self) -> bool {
        self.session_start.elapsed() > self.config.total_timeout
    }

    /// Get a summary of phase timing.
    pub fn summary(&self) -> PhaseTimingSummary {
        PhaseTimingSummary {
            session_elapsed: self.session_elapsed(),
            total_timeout: self.config.total_timeout,
            plan_budget_remaining: self.budgets.plan,
            execute_budget_remaining: self.budgets.execute,
            verify_budget_remaining: self.budgets.verify,
            timed_out_phases: self.timed_out_phases.clone(),
        }
    }

    fn budget_for_phase(&self, phase: &PevPhase) -> Duration {
        match phase {
            PevPhase::Planning => self.budgets.plan,
            PevPhase::Executing => self.budgets.execute,
            PevPhase::Verifying => self.budgets.verify,
            PevPhase::Repairing => self.budgets.verify, // Repairs share verify budget
            _ => Duration::from_secs(60),               // Default for other phases
        }
    }

    fn deduct_from_phase(&mut self, phase: &PevPhase, elapsed: Duration) {
        match phase {
            PevPhase::Planning => {
                self.budgets.plan = self.budgets.plan.saturating_sub(elapsed);
            }
            PevPhase::Executing => {
                self.budgets.execute = self.budgets.execute.saturating_sub(elapsed);
            }
            PevPhase::Verifying | PevPhase::Repairing => {
                self.budgets.verify = self.budgets.verify.saturating_sub(elapsed);
            }
            _ => {}
        }
    }
}

/// Action to take when a phase times out.
#[derive(Debug, Clone, PartialEq)]
pub enum PhaseTimeoutAction {
    /// Continue normally (shouldn't happen).
    Continue,
    /// Skip directly to execute phase.
    SkipToExecute,
    /// Skip directly to verify phase.
    SkipToVerify,
    /// Escalate — all phases exhausted.
    Escalate,
}

/// Summary of phase timing for display.
#[derive(Debug, Clone)]
pub struct PhaseTimingSummary {
    pub session_elapsed: Duration,
    pub total_timeout: Duration,
    pub plan_budget_remaining: Duration,
    pub execute_budget_remaining: Duration,
    pub verify_budget_remaining: Duration,
    pub timed_out_phases: Vec<PevPhase>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_allocation_sums_to_total() {
        let config = PhaseTimeoutConfig {
            total_timeout: Duration::from_secs(100),
            ..Default::default()
        };
        let tracker = PhaseTimeoutTracker::new(config);

        let total_budget = tracker.budgets.plan + tracker.budgets.execute + tracker.budgets.verify;
        // Allow 1ms rounding error
        assert!(
            (total_budget.as_secs_f64() - 100.0).abs() < 0.01,
            "Budget sum {} != 100",
            total_budget.as_secs_f64()
        );
    }

    #[test]
    fn timeout_redistribution() {
        let config = PhaseTimeoutConfig {
            total_timeout: Duration::from_secs(100),
            ..Default::default()
        };
        let mut tracker = PhaseTimeoutTracker::new(config);

        let original_execute = tracker.budgets.execute;
        let original_verify = tracker.budgets.verify;
        let plan_budget = tracker.budgets.plan;

        tracker.enter_phase(PevPhase::Planning);
        // Simulate immediate timeout (budget not consumed)
        tracker.current_phase_start = Some(Instant::now() - plan_budget - Duration::from_secs(1));
        let action = tracker.handle_timeout();

        assert_eq!(action, PhaseTimeoutAction::SkipToExecute);
        // Execute and verify should have gotten the plan budget
        assert!(tracker.budgets.execute >= original_execute);
        assert!(tracker.budgets.verify >= original_verify);
    }
}
