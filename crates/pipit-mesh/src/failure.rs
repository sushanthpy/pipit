//! Phi Accrual Failure Detector — probabilistic liveness detection.
//!
//! Instead of binary alive/dead, computes φ (phi) — the suspicion level
//! that a node has failed. Higher φ → more suspicious.
//!
//! φ = -log₁₀(1 - F(t_now - t_last))
//!
//! where F is the CDF of the inter-arrival time distribution (modeled as
//! normal distribution from a sliding window of heartbeat intervals).
//!
//! Threshold: φ > 8 ≈ 10⁻⁸ false positive rate.
//!            φ > 3 ≈ 10⁻³ (more aggressive, faster detection).
//!
//! Reference: Hayashibara et al., "The φ Accrual Failure Detector" (2004).

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Configuration for the phi accrual detector.
#[derive(Debug, Clone)]
pub struct PhiAccrualConfig {
    /// Suspicion threshold. Default: 8.0 (10⁻⁸ false positive rate).
    pub threshold: f64,
    /// Initial heartbeat interval estimate (before enough samples).
    pub initial_heartbeat_ms: f64,
    /// Sliding window size for interval history.
    pub window_size: usize,
    /// Minimum standard deviation (prevents division by zero).
    pub min_std_dev_ms: f64,
}

impl Default for PhiAccrualConfig {
    fn default() -> Self {
        Self {
            threshold: 8.0,
            initial_heartbeat_ms: 1000.0,
            window_size: 200,
            min_std_dev_ms: 100.0,
        }
    }
}

/// Per-node heartbeat history.
struct HeartbeatHistory {
    /// Circular buffer of inter-arrival times in milliseconds.
    intervals: Vec<f64>,
    /// Write position in the circular buffer.
    write_pos: usize,
    /// Number of recorded intervals (≤ window_size).
    count: usize,
    /// Timestamp of last heartbeat.
    last_heartbeat: Instant,
    /// Running sum of intervals in the window (for O(1) mean).
    rolling_sum: f64,
    /// Running sum of squared intervals (for O(1) variance).
    rolling_sum_sq: f64,
    /// Cached mean (derived from rolling_sum).
    mean: f64,
    /// Cached variance (derived from rolling sums).
    variance: f64,
}

impl HeartbeatHistory {
    fn new(window_size: usize, initial_interval_ms: f64) -> Self {
        Self {
            intervals: vec![0.0; window_size],
            write_pos: 0,
            count: 0,
            last_heartbeat: Instant::now(),
            rolling_sum: 0.0,
            rolling_sum_sq: 0.0,
            mean: initial_interval_ms,
            variance: initial_interval_ms * initial_interval_ms / 4.0,
        }
    }

    /// Record a heartbeat arrival — O(1) amortized.
    /// Uses a rolling sum/sum-of-squares over a fixed ring buffer instead of
    /// recomputing from the entire window on every heartbeat.
    fn record_heartbeat(&mut self, now: Instant) {
        let interval = now.duration_since(self.last_heartbeat).as_secs_f64() * 1000.0;
        self.last_heartbeat = now;

        let window_size = self.intervals.len();

        // Subtract the value being evicted from the ring buffer
        let evicted = self.intervals[self.write_pos];
        if self.count == window_size {
            self.rolling_sum -= evicted;
            self.rolling_sum_sq -= evicted * evicted;
        } else {
            self.count += 1;
        }

        // Insert the new interval
        self.intervals[self.write_pos] = interval;
        self.write_pos = (self.write_pos + 1) % window_size;
        self.rolling_sum += interval;
        self.rolling_sum_sq += interval * interval;

        // Derive mean and variance from rolling sums — O(1)
        let n = self.count as f64;
        self.mean = self.rolling_sum / n;
        // Var(X) = E[X²] - E[X]² — numerically adequate for this use case
        self.variance = (self.rolling_sum_sq / n - self.mean * self.mean).max(0.0);
    }

    /// Standard deviation of inter-arrival times.
    fn std_dev(&self, min_std_dev: f64) -> f64 {
        self.variance.sqrt().max(min_std_dev)
    }

    /// Compute φ value given current time.
    /// φ = -log₁₀(1 - F(t_now - t_last))
    /// where F is the normal CDF.
    fn phi(&self, now: Instant, min_std_dev: f64) -> f64 {
        let elapsed_ms = now.duration_since(self.last_heartbeat).as_secs_f64() * 1000.0;
        let std_dev = self.std_dev(min_std_dev);

        // z-score
        let z = (elapsed_ms - self.mean) / std_dev;

        // Normal CDF approximation (Abramowitz & Stegun 26.2.17)
        // Accurate to 10⁻⁵ for the tail.
        let p = normal_cdf(z);

        // φ = -log₁₀(1 - p)
        // Guard against log(0): clamp to a large φ value
        let q = 1.0 - p;
        if q <= 1e-15 {
            return 16.0; // Extremely suspicious
        }

        -q.log10()
    }
}

/// Normal CDF approximation using the error function.
/// Uses the Horner form of the rational approximation.
fn normal_cdf(x: f64) -> f64 {
    // For very negative values, tail is ~0
    if x < -8.0 {
        return 0.0;
    }
    // For very positive values, tail is ~1
    if x > 8.0 {
        return 1.0;
    }

    // Approximation: 0.5 * erfc(-x / √2)
    // Using the complementary error function approximation
    let t = 1.0 / (1.0 + 0.2316419 * x.abs());
    let d = 0.3989422804014327; // 1/√(2π)
    let p = d * (-x * x / 2.0).exp();
    let val = p
        * t
        * (0.319381530
            + t * (-0.356563782 + t * (1.781477937 + t * (-1.821255978 + t * 1.330274429))));

    if x >= 0.0 { 1.0 - val } else { val }
}

// ── The detector ────────────────────────────────────────────────────

/// Phi accrual failure detector for mesh nodes.
pub struct PhiAccrualDetector {
    config: PhiAccrualConfig,
    nodes: HashMap<String, HeartbeatHistory>,
}

/// Liveness assessment for a node.
#[derive(Debug, Clone)]
pub struct NodeLiveness {
    pub node_id: String,
    pub phi: f64,
    pub alive: bool,
    pub last_heartbeat_ms_ago: u64,
    pub mean_interval_ms: f64,
    pub std_dev_ms: f64,
}

impl PhiAccrualDetector {
    pub fn new(config: PhiAccrualConfig) -> Self {
        Self {
            config,
            nodes: HashMap::new(),
        }
    }

    /// Record a heartbeat from a node.
    pub fn heartbeat(&mut self, node_id: &str) {
        let now = Instant::now();
        let history = self.nodes.entry(node_id.to_string()).or_insert_with(|| {
            HeartbeatHistory::new(self.config.window_size, self.config.initial_heartbeat_ms)
        });
        history.record_heartbeat(now);
    }

    /// Compute φ for a node. Returns None if node has never been seen.
    pub fn phi(&self, node_id: &str) -> Option<f64> {
        let history = self.nodes.get(node_id)?;
        Some(history.phi(Instant::now(), self.config.min_std_dev_ms))
    }

    /// Check if a node is considered alive (φ < threshold).
    pub fn is_alive(&self, node_id: &str) -> bool {
        match self.phi(node_id) {
            Some(phi) => phi < self.config.threshold,
            None => false, // Never seen = not alive
        }
    }

    /// Get liveness details for a node.
    pub fn liveness(&self, node_id: &str) -> Option<NodeLiveness> {
        let history = self.nodes.get(node_id)?;
        let now = Instant::now();
        let phi = history.phi(now, self.config.min_std_dev_ms);
        let elapsed_ms = now.duration_since(history.last_heartbeat).as_millis() as u64;

        Some(NodeLiveness {
            node_id: node_id.to_string(),
            phi,
            alive: phi < self.config.threshold,
            last_heartbeat_ms_ago: elapsed_ms,
            mean_interval_ms: history.mean,
            std_dev_ms: history.std_dev(self.config.min_std_dev_ms),
        })
    }

    /// Get all nodes currently considered failed (φ ≥ threshold).
    pub fn failed_nodes(&self) -> Vec<String> {
        let now = Instant::now();
        self.nodes
            .iter()
            .filter(|(_, h)| h.phi(now, self.config.min_std_dev_ms) >= self.config.threshold)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Get all nodes currently considered alive.
    pub fn alive_nodes(&self) -> Vec<String> {
        let now = Instant::now();
        self.nodes
            .iter()
            .filter(|(_, h)| h.phi(now, self.config.min_std_dev_ms) < self.config.threshold)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Remove a node from tracking.
    pub fn remove(&mut self, node_id: &str) {
        self.nodes.remove(node_id);
    }

    /// Number of tracked nodes.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
}

impl Default for PhiAccrualDetector {
    fn default() -> Self {
        Self::new(PhiAccrualConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_normal_cdf_center() {
        let p = normal_cdf(0.0);
        assert!((p - 0.5).abs() < 0.001, "CDF(0) should be ~0.5, got {}", p);
    }

    #[test]
    fn test_normal_cdf_tails() {
        assert!(normal_cdf(-3.0) < 0.01);
        assert!(normal_cdf(3.0) > 0.99);
    }

    #[test]
    fn test_phi_increases_with_silence() {
        let mut detector = PhiAccrualDetector::new(PhiAccrualConfig {
            threshold: 8.0,
            initial_heartbeat_ms: 100.0,
            window_size: 10,
            min_std_dev_ms: 50.0,
        });

        // Record several heartbeats at ~50ms intervals
        for _ in 0..5 {
            detector.heartbeat("node-1");
            thread::sleep(Duration::from_millis(50));
        }

        // Phi should be low right after a heartbeat
        let phi_after = detector.phi("node-1").unwrap();
        assert!(
            phi_after < 3.0,
            "φ should be low right after heartbeat: {}",
            phi_after
        );

        // Wait longer than usual
        thread::sleep(Duration::from_millis(500));
        let phi_late = detector.phi("node-1").unwrap();

        assert!(
            phi_late > phi_after,
            "φ should increase with silence: {} > {}",
            phi_late,
            phi_after
        );
    }

    #[test]
    fn test_unknown_node() {
        let detector = PhiAccrualDetector::default();
        assert_eq!(detector.phi("nonexistent"), None);
        assert!(!detector.is_alive("nonexistent"));
    }
}
