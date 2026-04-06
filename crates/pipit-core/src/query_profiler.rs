//! Query Profiler and Latency Budget Controller
//!
//! Instruments the developer loop with checkpointed latency telemetry.
//! Tracks: TTFI (time to first input), TTFT (time to first token),
//! tool execution time, verification time, total turn time.
//!
//! Provides HDR histogram-like percentile tracking and latency budget enforcement.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A checkpoint in the query timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Checkpoint {
    /// User input accepted.
    InputAccepted,
    /// Planning started.
    PlanningStart,
    /// Planning completed.
    PlanningEnd,
    /// LLM request dispatched.
    RequestDispatched,
    /// First token received from LLM.
    FirstToken,
    /// Full response received.
    ResponseComplete,
    /// Tool execution started.
    ToolStart,
    /// Tool execution completed.
    ToolEnd,
    /// Verification started.
    VerificationStart,
    /// Verification completed.
    VerificationEnd,
    /// Turn completed.
    TurnComplete,
}

/// A single latency observation between two checkpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyObservation {
    pub turn: u32,
    pub from: Checkpoint,
    pub to: Checkpoint,
    pub duration_ms: u64,
}

/// Per-turn profiling data.
#[derive(Debug, Clone)]
pub struct TurnProfile {
    pub turn_number: u32,
    checkpoints: HashMap<Checkpoint, Instant>,
    observations: Vec<LatencyObservation>,
}

impl TurnProfile {
    pub fn new(turn_number: u32) -> Self {
        Self {
            turn_number,
            checkpoints: HashMap::new(),
            observations: Vec::new(),
        }
    }

    /// Record a checkpoint timestamp.
    pub fn checkpoint(&mut self, cp: Checkpoint) {
        self.checkpoints.insert(cp, Instant::now());
    }

    /// Compute duration between two checkpoints.
    pub fn duration(&self, from: Checkpoint, to: Checkpoint) -> Option<Duration> {
        let start = self.checkpoints.get(&from)?;
        let end = self.checkpoints.get(&to)?;
        Some(end.duration_since(*start))
    }

    /// Record an observation for the given checkpoint pair.
    pub fn observe(&mut self, from: Checkpoint, to: Checkpoint) {
        if let Some(dur) = self.duration(from, to) {
            self.observations.push(LatencyObservation {
                turn: self.turn_number,
                from,
                to,
                duration_ms: dur.as_millis() as u64,
            });
        }
    }

    /// Time to first token (TTFT).
    pub fn ttft(&self) -> Option<Duration> {
        self.duration(Checkpoint::InputAccepted, Checkpoint::FirstToken)
    }

    /// Total turn time.
    pub fn total_turn_time(&self) -> Option<Duration> {
        self.duration(Checkpoint::InputAccepted, Checkpoint::TurnComplete)
    }

    /// Tool execution time.
    pub fn tool_time(&self) -> Option<Duration> {
        self.duration(Checkpoint::ToolStart, Checkpoint::ToolEnd)
    }

    /// Verification time.
    pub fn verification_time(&self) -> Option<Duration> {
        self.duration(Checkpoint::VerificationStart, Checkpoint::VerificationEnd)
    }
}

/// Latency budget — acceptable thresholds for developer loop responsiveness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyBudget {
    /// Maximum acceptable TTFT (p50).
    pub ttft_p50_ms: u64,
    /// Maximum acceptable TTFT (p95).
    pub ttft_p95_ms: u64,
    /// Maximum verification time as fraction of total turn time.
    pub verification_max_fraction: f64,
    /// Maximum tool execution time per tool (seconds).
    pub tool_timeout_secs: u64,
}

impl Default for LatencyBudget {
    fn default() -> Self {
        Self {
            ttft_p50_ms: 800,
            ttft_p95_ms: 3000,
            verification_max_fraction: 0.15,
            tool_timeout_secs: 120,
        }
    }
}

/// Aggregate latency statistics across turns.
pub struct QueryProfiler {
    /// Per-turn profiles.
    profiles: Vec<TurnProfile>,
    /// Current turn profile.
    current: Option<TurnProfile>,
    /// Latency budget.
    budget: LatencyBudget,
    /// TTFT observations (milliseconds) for percentile computation.
    ttft_observations: Vec<u64>,
}

impl QueryProfiler {
    pub fn new(budget: LatencyBudget) -> Self {
        Self {
            profiles: Vec::new(),
            current: None,
            budget,
            ttft_observations: Vec::new(),
        }
    }

    /// Start profiling a new turn.
    pub fn start_turn(&mut self, turn_number: u32) {
        if let Some(prev) = self.current.take() {
            // Finalize previous turn
            if let Some(ttft) = prev.ttft() {
                self.ttft_observations.push(ttft.as_millis() as u64);
            }
            self.profiles.push(prev);
        }
        let mut profile = TurnProfile::new(turn_number);
        profile.checkpoint(Checkpoint::InputAccepted);
        self.current = Some(profile);
    }

    /// Record a checkpoint in the current turn.
    pub fn checkpoint(&mut self, cp: Checkpoint) {
        if let Some(ref mut profile) = self.current {
            profile.checkpoint(cp);
        }
    }

    /// Finalize the current turn and collect observations.
    pub fn end_turn(&mut self) {
        if let Some(ref mut profile) = self.current {
            profile.checkpoint(Checkpoint::TurnComplete);
            // Auto-observe standard pairs
            profile.observe(Checkpoint::InputAccepted, Checkpoint::FirstToken);
            profile.observe(Checkpoint::InputAccepted, Checkpoint::TurnComplete);
            profile.observe(Checkpoint::ToolStart, Checkpoint::ToolEnd);
            profile.observe(Checkpoint::VerificationStart, Checkpoint::VerificationEnd);
        }
        if let Some(prev) = self.current.take() {
            if let Some(ttft) = prev.ttft() {
                self.ttft_observations.push(ttft.as_millis() as u64);
            }
            self.profiles.push(prev);
        }
    }

    /// Compute TTFT percentile (0-100).
    pub fn ttft_percentile(&self, p: f64) -> Option<u64> {
        if self.ttft_observations.is_empty() {
            return None;
        }
        let mut sorted = self.ttft_observations.clone();
        sorted.sort();
        let idx = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
        Some(sorted[idx.min(sorted.len() - 1)])
    }

    /// Check budget violations.
    pub fn budget_violations(&self) -> Vec<String> {
        let mut violations = Vec::new();
        if let Some(p50) = self.ttft_percentile(50.0) {
            if p50 > self.budget.ttft_p50_ms {
                violations.push(format!(
                    "TTFT p50 is {}ms (budget: {}ms)",
                    p50, self.budget.ttft_p50_ms
                ));
            }
        }
        if let Some(p95) = self.ttft_percentile(95.0) {
            if p95 > self.budget.ttft_p95_ms {
                violations.push(format!(
                    "TTFT p95 is {}ms (budget: {}ms)",
                    p95, self.budget.ttft_p95_ms
                ));
            }
        }
        violations
    }

    /// Summary of profiling data.
    pub fn summary(&self) -> ProfileSummary {
        ProfileSummary {
            total_turns: self.profiles.len() as u32,
            ttft_p50_ms: self.ttft_percentile(50.0),
            ttft_p95_ms: self.ttft_percentile(95.0),
            ttft_p99_ms: self.ttft_percentile(99.0),
            budget_violations: self.budget_violations(),
        }
    }
}

impl Default for QueryProfiler {
    fn default() -> Self {
        Self::new(LatencyBudget::default())
    }
}

/// Summary of profiling observations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSummary {
    pub total_turns: u32,
    pub ttft_p50_ms: Option<u64>,
    pub ttft_p95_ms: Option<u64>,
    pub ttft_p99_ms: Option<u64>,
    pub budget_violations: Vec<String>,
}

// ─── Closed-Loop Telemetry Controller ──────────────────────────────────

/// Control signals produced by the telemetry controller.
/// These feed back into planner depth, compaction thresholds, and model routing.
#[derive(Debug, Clone, Default)]
pub struct ControlSignals {
    /// If true, the runtime should prefer shorter plans (fewer tool calls).
    pub reduce_plan_depth: bool,
    /// If true, trigger proactive compaction even if budget isn't exceeded.
    pub trigger_compaction: bool,
    /// If true, context pressure is high — evict stale results.
    pub evict_stale_results: bool,
    /// Recommended tool timeout override (None = use default).
    pub tool_timeout_override_secs: Option<u64>,
    /// If true, skip verification to save latency.
    pub skip_verification: bool,
    /// EMA of tool amplification ratio (tools per turn).
    pub tool_amplification_ema: f64,
}

/// Closed-loop telemetry controller — converts observations into control signals.
///
/// Maintains EMA (Exponential Moving Average) estimates for key metrics and
/// produces threshold-based control decisions. O(1) per turn.
pub struct TelemetryController {
    /// EMA of time-to-first-token (ms).
    ttft_ema_ms: f64,
    /// EMA of total turn latency (ms).
    turn_latency_ema_ms: f64,
    /// EMA of tool calls per turn.
    tools_per_turn_ema: f64,
    /// EMA smoothing factor (0-1, higher = more responsive).
    alpha: f64,
    /// TTFT threshold for triggering compaction (ms).
    ttft_compaction_threshold_ms: f64,
    /// Turn latency threshold for reducing plan depth (ms).
    turn_latency_threshold_ms: f64,
    /// Tool amplification threshold for warning.
    tool_amplification_threshold: f64,
    /// Number of observations recorded.
    observation_count: u64,
}

impl TelemetryController {
    pub fn new() -> Self {
        Self {
            ttft_ema_ms: 0.0,
            turn_latency_ema_ms: 0.0,
            tools_per_turn_ema: 0.0,
            alpha: 0.3, // 30% weight to new observation
            ttft_compaction_threshold_ms: 5000.0,
            turn_latency_threshold_ms: 30000.0,
            tool_amplification_threshold: 5.0,
            observation_count: 0,
        }
    }

    /// Record observations from a completed turn. O(1).
    pub fn observe_turn(
        &mut self,
        ttft_ms: Option<u64>,
        turn_latency_ms: u64,
        tool_calls: u32,
    ) {
        self.observation_count += 1;

        if let Some(ttft) = ttft_ms {
            let ttft_f = ttft as f64;
            if self.observation_count == 1 {
                self.ttft_ema_ms = ttft_f;
            } else {
                self.ttft_ema_ms = self.alpha * ttft_f + (1.0 - self.alpha) * self.ttft_ema_ms;
            }
        }

        let latency_f = turn_latency_ms as f64;
        if self.observation_count == 1 {
            self.turn_latency_ema_ms = latency_f;
            self.tools_per_turn_ema = tool_calls as f64;
        } else {
            self.turn_latency_ema_ms = self.alpha * latency_f + (1.0 - self.alpha) * self.turn_latency_ema_ms;
            self.tools_per_turn_ema = self.alpha * (tool_calls as f64) + (1.0 - self.alpha) * self.tools_per_turn_ema;
        }
    }

    /// Produce control signals based on accumulated observations.
    /// Threshold-based initially; can be upgraded to bandit/MPC later.
    pub fn control_signals(&self) -> ControlSignals {
        if self.observation_count < 2 {
            return ControlSignals::default();
        }

        ControlSignals {
            reduce_plan_depth: self.turn_latency_ema_ms > self.turn_latency_threshold_ms,
            trigger_compaction: self.ttft_ema_ms > self.ttft_compaction_threshold_ms,
            evict_stale_results: self.ttft_ema_ms > self.ttft_compaction_threshold_ms * 0.8,
            tool_timeout_override_secs: if self.turn_latency_ema_ms > self.turn_latency_threshold_ms {
                Some(60) // Reduce timeout under pressure
            } else {
                None
            },
            skip_verification: false, // Never skip verification automatically
            tool_amplification_ema: self.tools_per_turn_ema,
        }
    }

    /// Current TTFT EMA estimate.
    pub fn ttft_ema_ms(&self) -> f64 {
        self.ttft_ema_ms
    }

    /// Current turn latency EMA estimate.
    pub fn turn_latency_ema_ms(&self) -> f64 {
        self.turn_latency_ema_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn turn_profile_tracks_checkpoints() {
        let mut profile = TurnProfile::new(1);
        profile.checkpoint(Checkpoint::InputAccepted);
        thread::sleep(Duration::from_millis(10));
        profile.checkpoint(Checkpoint::FirstToken);
        thread::sleep(Duration::from_millis(10));
        profile.checkpoint(Checkpoint::TurnComplete);

        let ttft = profile.ttft().unwrap();
        assert!(ttft.as_millis() >= 10);

        let total = profile.total_turn_time().unwrap();
        assert!(total.as_millis() >= 20);
    }

    #[test]
    fn percentile_computation() {
        let mut profiler = QueryProfiler::default();
        // Simulate 10 turns with varying TTFT
        for i in 0..10 {
            profiler.ttft_observations.push((i + 1) * 100);
        }
        // p50 should be around 500ms
        let p50 = profiler.ttft_percentile(50.0).unwrap();
        assert!(p50 >= 400 && p50 <= 600);
    }

    #[test]
    fn budget_violation_detection() {
        let mut profiler = QueryProfiler::new(LatencyBudget {
            ttft_p50_ms: 100,
            ttft_p95_ms: 500,
            ..Default::default()
        });
        // All TTFTs are 1000ms — should violate both budgets
        for _ in 0..10 {
            profiler.ttft_observations.push(1000);
        }
        let violations = profiler.budget_violations();
        assert_eq!(violations.len(), 2);
    }
}
