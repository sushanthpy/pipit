//! Query-Level Profiling with Checkpoints (Task 5.1)
//!
//! A lightweight checkpoint profiler that emits structured timing data per turn.
//! Checkpoints use `Instant::now()` (monotonic clock, ~20ns overhead per call).
//! Storage: circular buffer of last 100 turns, each with up to 20 checkpoints.
//! Total memory: 100 × 20 × 16 bytes = 32KB.
//!
//! Supports streaming P95 estimation via the P² algorithm.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Named checkpoint within a turn.
#[derive(Debug, Clone)]
pub struct Checkpoint {
    pub name: &'static str,
    pub timestamp: Instant,
    /// Duration since the previous checkpoint (or turn start).
    pub elapsed_since_prev: Duration,
}

/// All checkpoints for a single turn.
#[derive(Debug, Clone)]
pub struct TurnProfile {
    pub turn_number: u32,
    pub start: Instant,
    pub checkpoints: Vec<Checkpoint>,
    pub total_duration: Option<Duration>,
}

impl TurnProfile {
    fn new(turn_number: u32) -> Self {
        Self {
            turn_number,
            start: Instant::now(),
            checkpoints: Vec::with_capacity(20),
            total_duration: None,
        }
    }

    fn checkpoint(&mut self, name: &'static str) {
        let now = Instant::now();
        let prev = self
            .checkpoints
            .last()
            .map(|c| c.timestamp)
            .unwrap_or(self.start);
        self.checkpoints.push(Checkpoint {
            name,
            timestamp: now,
            elapsed_since_prev: now.duration_since(prev),
        });
    }

    fn finish(&mut self) {
        self.total_duration = Some(self.start.elapsed());
    }

    /// Get the duration of a specific checkpoint phase.
    pub fn phase_duration(&self, name: &str) -> Option<Duration> {
        self.checkpoints
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.elapsed_since_prev)
    }

    /// Get a breakdown of all phases as (name, duration_ms) pairs.
    pub fn breakdown(&self) -> Vec<(&'static str, f64)> {
        self.checkpoints
            .iter()
            .map(|c| (c.name, c.elapsed_since_prev.as_secs_f64() * 1000.0))
            .collect()
    }
}

/// The profiler — maintains a circular buffer of turn profiles.
pub struct QueryProfiler {
    /// Circular buffer of recent turn profiles.
    profiles: VecDeque<TurnProfile>,
    /// Maximum number of turns to retain.
    max_turns: usize,
    /// Current in-progress turn.
    current: Option<TurnProfile>,
    /// P² quantile estimator for total turn duration.
    p95_estimator: P2Estimator,
}

/// Well-known checkpoint names used across the agent loop.
pub mod checkpoints {
    pub const TURN_START: &str = "turn_start";
    pub const STEERING_DRAIN: &str = "steering_drain";
    pub const COMPRESSION_START: &str = "compression_start";
    pub const COMPRESSION_END: &str = "compression_end";
    pub const PLAN_SELECT: &str = "plan_select";
    pub const REQUEST_BUILD: &str = "request_build";
    pub const API_CALL_START: &str = "api_call_start";
    pub const API_STREAMING_START: &str = "api_streaming_start";
    pub const API_STREAMING_END: &str = "api_streaming_end";
    pub const TOOL_EXEC_START: &str = "tool_exec_start";
    pub const TOOL_EXEC_END: &str = "tool_exec_end";
    pub const VERIFICATION_START: &str = "verification_start";
    pub const VERIFICATION_END: &str = "verification_end";
    pub const TURN_END: &str = "turn_end";
}

impl QueryProfiler {
    pub fn new() -> Self {
        Self {
            profiles: VecDeque::with_capacity(100),
            max_turns: 100,
            current: None,
            p95_estimator: P2Estimator::new(0.95),
        }
    }

    /// Start profiling a new turn.
    pub fn start_turn(&mut self, turn_number: u32) {
        // Finish any in-progress turn
        if let Some(mut prev) = self.current.take() {
            prev.finish();
            if let Some(dur) = prev.total_duration {
                self.p95_estimator.observe(dur.as_secs_f64() * 1000.0);
            }
            self.push_profile(prev);
        }

        self.current = Some(TurnProfile::new(turn_number));
    }

    /// Record a checkpoint in the current turn.
    pub fn checkpoint(&mut self, name: &'static str) {
        if let Some(ref mut turn) = self.current {
            turn.checkpoint(name);
        }
    }

    /// Finish the current turn.
    pub fn end_turn(&mut self) {
        if let Some(mut turn) = self.current.take() {
            turn.finish();
            if let Some(dur) = turn.total_duration {
                self.p95_estimator.observe(dur.as_secs_f64() * 1000.0);
            }
            self.push_profile(turn);
        }
    }

    /// Get the current turn's profile (for live display).
    pub fn current_turn(&self) -> Option<&TurnProfile> {
        self.current.as_ref()
    }

    /// Get the last N completed turn profiles.
    pub fn recent_profiles(&self, n: usize) -> impl Iterator<Item = &TurnProfile> {
        self.profiles.iter().rev().take(n)
    }

    /// Get the estimated P95 turn duration in milliseconds.
    pub fn p95_duration_ms(&self) -> Option<f64> {
        self.p95_estimator.estimate()
    }

    /// Get average turn duration in milliseconds.
    pub fn avg_duration_ms(&self) -> Option<f64> {
        if self.profiles.is_empty() {
            return None;
        }
        let sum: f64 = self
            .profiles
            .iter()
            .filter_map(|p| p.total_duration)
            .map(|d| d.as_secs_f64() * 1000.0)
            .sum();
        Some(sum / self.profiles.len() as f64)
    }

    /// Get a summary of per-phase P50 durations across all profiled turns.
    /// Returns (phase_name, median_duration_ms) sorted by duration descending.
    pub fn phase_summary(&self) -> Vec<(String, f64)> {
        use std::collections::HashMap;

        let mut phase_durations: HashMap<&str, Vec<f64>> = HashMap::new();
        for profile in &self.profiles {
            for cp in &profile.checkpoints {
                phase_durations
                    .entry(cp.name)
                    .or_default()
                    .push(cp.elapsed_since_prev.as_secs_f64() * 1000.0);
            }
        }

        let mut result: Vec<(String, f64)> = phase_durations
            .into_iter()
            .map(|(name, mut durations)| {
                durations.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let median = if durations.is_empty() {
                    0.0
                } else {
                    durations[durations.len() / 2]
                };
                (name.to_string(), median)
            })
            .collect();
        result.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        result
    }

    fn push_profile(&mut self, profile: TurnProfile) {
        if self.profiles.len() >= self.max_turns {
            self.profiles.pop_front();
        }
        self.profiles.push_back(profile);
    }
}

impl Default for QueryProfiler {
    fn default() -> Self {
        Self::new()
    }
}

// ─── P² Algorithm for Streaming Quantile Estimation ─────────────────────
//
// Jain & Chlamtac, 1985.
// O(1) memory, O(1) per update.
// Uses 5 markers to estimate an arbitrary quantile.

/// P² streaming quantile estimator.
pub struct P2Estimator {
    quantile: f64,
    /// The 5 marker heights (sorted observations).
    q: [f64; 5],
    /// The 5 marker positions.
    n: [f64; 5],
    /// Desired marker positions.
    n_prime: [f64; 5],
    /// Increment for desired positions.
    dn: [f64; 5],
    /// Number of observations so far.
    count: usize,
    /// Initial observations buffer (need at least 5 to initialize).
    init_buffer: Vec<f64>,
    /// Whether the estimator is initialized.
    initialized: bool,
}

impl P2Estimator {
    pub fn new(quantile: f64) -> Self {
        let p = quantile;
        Self {
            quantile: p,
            q: [0.0; 5],
            n: [0.0; 5],
            n_prime: [0.0; 5],
            dn: [0.0, p / 2.0, p, (1.0 + p) / 2.0, 1.0],
            count: 0,
            init_buffer: Vec::with_capacity(5),
            initialized: false,
        }
    }

    pub fn observe(&mut self, value: f64) {
        self.count += 1;

        if !self.initialized {
            self.init_buffer.push(value);
            if self.init_buffer.len() >= 5 {
                self.initialize();
            }
            return;
        }

        // Find the cell k where value falls
        let k = if value < self.q[0] {
            self.q[0] = value;
            0
        } else if value < self.q[1] {
            0
        } else if value < self.q[2] {
            1
        } else if value < self.q[3] {
            2
        } else if value < self.q[4] {
            3
        } else {
            self.q[4] = value;
            3
        };

        // Increment positions of markers k+1 through 4
        for i in (k + 1)..5 {
            self.n[i] += 1.0;
        }

        // Update desired positions
        for i in 0..5 {
            self.n_prime[i] += self.dn[i];
        }

        // Adjust marker heights using P² formula
        for i in 1..4 {
            let d = self.n_prime[i] - self.n[i];
            if (d >= 1.0 && self.n[i + 1] - self.n[i] > 1.0)
                || (d <= -1.0 && self.n[i - 1] - self.n[i] < -1.0)
            {
                let sign = if d > 0.0 { 1.0 } else { -1.0 };

                // Parabolic formula
                let qi = self.q[i]
                    + sign
                        / (self.n[i + 1] - self.n[i - 1])
                        * ((self.n[i] - self.n[i - 1] + sign)
                            * (self.q[i + 1] - self.q[i])
                            / (self.n[i + 1] - self.n[i])
                            + (self.n[i + 1] - self.n[i] - sign)
                                * (self.q[i] - self.q[i - 1])
                                / (self.n[i] - self.n[i - 1]));

                if qi > self.q[i - 1] && qi < self.q[i + 1] {
                    self.q[i] = qi;
                } else {
                    // Linear formula as fallback
                    self.q[i] += sign
                        * (self.q[(i as isize + sign as isize) as usize] - self.q[i])
                        / (self.n[(i as isize + sign as isize) as usize] - self.n[i]);
                }
                self.n[i] += sign;
            }
        }
    }

    pub fn estimate(&self) -> Option<f64> {
        if !self.initialized {
            if self.init_buffer.is_empty() {
                return None;
            }
            // Fallback: use the buffer directly for small sample sizes
            let mut sorted = self.init_buffer.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let idx = ((sorted.len() as f64 * self.quantile) as usize).min(sorted.len() - 1);
            return Some(sorted[idx]);
        }
        Some(self.q[2]) // The middle marker is the quantile estimate
    }

    fn initialize(&mut self) {
        self.init_buffer
            .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        for i in 0..5 {
            self.q[i] = self.init_buffer[i];
            self.n[i] = i as f64;
        }
        let p = self.quantile;
        self.n_prime = [0.0, 2.0 * p, 4.0 * p, 2.0 + 2.0 * p, 4.0];
        self.initialized = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiler_records_checkpoints() {
        let mut profiler = QueryProfiler::new();

        profiler.start_turn(1);
        profiler.checkpoint(checkpoints::TURN_START);
        std::thread::sleep(Duration::from_millis(5));
        profiler.checkpoint(checkpoints::API_CALL_START);
        std::thread::sleep(Duration::from_millis(10));
        profiler.checkpoint(checkpoints::API_STREAMING_END);
        profiler.checkpoint(checkpoints::TURN_END);
        profiler.end_turn();

        let profiles: Vec<_> = profiler.recent_profiles(1).collect();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].turn_number, 1);
        assert_eq!(profiles[0].checkpoints.len(), 4);
        assert!(profiles[0].total_duration.unwrap() >= Duration::from_millis(10));
    }

    #[test]
    fn profiler_circular_buffer_evicts_oldest() {
        let mut profiler = QueryProfiler::new();

        for i in 0..150 {
            profiler.start_turn(i);
            profiler.checkpoint(checkpoints::TURN_START);
            profiler.end_turn();
        }

        assert!(profiler.profiles.len() <= 100);
        // Oldest should be ~turn 50
        assert!(profiler.profiles.front().unwrap().turn_number >= 50);
    }

    #[test]
    fn p2_estimator_converges() {
        let mut est = P2Estimator::new(0.95);

        // Feed 1000 uniform samples
        for i in 0..1000 {
            est.observe(i as f64);
        }

        let p95 = est.estimate().unwrap();
        // P95 of uniform(0..1000) should be ~950
        assert!(
            (p95 - 950.0).abs() < 50.0,
            "P95 estimate {} too far from expected 950",
            p95
        );
    }
}
