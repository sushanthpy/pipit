//! Automated Benchmark-Driven Rewrite Loop — Task PGO-3
//!
//! PEV for performance: Plan(hypothesis) → Execute(rewrite) → Verify(benchmark).
//! Validation: Mann-Whitney U test (non-parametric, no normality assumption).
//! Terminates on: no improvement, budget exceeded, or consecutive failures.

use crate::hypothesis::OptimizationHypothesis;
use serde::{Deserialize, Serialize};
use std::process::Command;

/// Configuration for the rewrite loop.
#[derive(Debug, Clone)]
pub struct RewriteLoop {
    pub benchmark_command: String,
    pub test_command: String,
    pub max_attempts: usize,
    pub sample_count: usize,
    pub time_budget_secs: u64,
    pub consecutive_failure_limit: usize,
}

impl Default for RewriteLoop {
    fn default() -> Self {
        Self {
            benchmark_command: "cargo bench".into(),
            test_command: "cargo test".into(),
            max_attempts: 10,
            sample_count: 20,
            time_budget_secs: 1800,
            consecutive_failure_limit: 3,
        }
    }
}

/// Result of applying an optimization hypothesis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewriteResult {
    pub hypothesis_description: String,
    pub applied: bool,
    pub tests_passed: bool,
    pub speedup: Option<f64>,
    pub statistically_significant: bool,
    pub p_value: Option<f64>,
    pub before_times_ms: Vec<f64>,
    pub after_times_ms: Vec<f64>,
    pub kept: bool,
    pub reason: String,
}

impl RewriteLoop {
    /// Collect benchmark samples by running the benchmark command N times.
    pub fn collect_samples(&self, working_dir: &str, n: usize) -> Vec<f64> {
        let mut times = Vec::new();
        for _ in 0..n {
            let start = std::time::Instant::now();
            let result = Command::new("sh")
                .args(["-c", &self.benchmark_command])
                .current_dir(working_dir)
                .output();
            let elapsed = start.elapsed().as_millis() as f64;
            if result.map(|o| o.status.success()).unwrap_or(false) {
                times.push(elapsed);
            }
        }
        times
    }

    /// Run the test suite. Returns true if all tests pass.
    pub fn run_tests(&self, working_dir: &str) -> bool {
        Command::new("sh")
            .args(["-c", &self.test_command])
            .current_dir(working_dir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Mann-Whitney U test for comparing two sample sets.
    /// Returns (U statistic, approximate p-value, significant at α=0.05).
    pub fn mann_whitney_u(before: &[f64], after: &[f64]) -> (f64, f64, bool) {
        let n1 = before.len();
        let n2 = after.len();
        if n1 < 5 || n2 < 5 {
            return (0.0, 1.0, false); // Insufficient samples
        }

        // Combine and rank
        let mut combined: Vec<(f64, bool)> = before
            .iter()
            .map(|&v| (v, true))
            .chain(after.iter().map(|&v| (v, false)))
            .collect();
        combined.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        // Sum of ranks for 'after' group
        let mut rank_sum_after: f64 = 0.0;
        for (rank, (_, is_before)) in combined.iter().enumerate() {
            if !is_before {
                rank_sum_after += (rank + 1) as f64;
            }
        }

        // U statistic for the 'after' group
        let u2 = rank_sum_after - (n2 as f64 * (n2 as f64 + 1.0)) / 2.0;
        let u1 = (n1 as f64) * (n2 as f64) - u2;
        let u = u1.min(u2);

        // Normal approximation for large samples
        let mu = (n1 as f64 * n2 as f64) / 2.0;
        let sigma = ((n1 as f64 * n2 as f64 * (n1 as f64 + n2 as f64 + 1.0)) / 12.0).sqrt();

        if sigma == 0.0 {
            return (u, 1.0, false);
        }

        let z = (u - mu).abs() / sigma;

        // Approximate p-value from z-score (two-tailed)
        // Using the complementary error function approximation
        let p = 2.0 * (1.0 - normal_cdf(z));

        let significant = p < 0.05;
        (u, p, significant)
    }

    /// Evaluate a rewrite: did it improve performance significantly?
    pub fn evaluate_rewrite(before: &[f64], after: &[f64]) -> RewriteResult {
        if before.is_empty() || after.is_empty() {
            return RewriteResult {
                hypothesis_description: String::new(),
                applied: false,
                tests_passed: false,
                speedup: None,
                statistically_significant: false,
                p_value: None,
                before_times_ms: before.to_vec(),
                after_times_ms: after.to_vec(),
                kept: false,
                reason: "No benchmark data".into(),
            };
        }

        let before_mean: f64 = before.iter().sum::<f64>() / before.len() as f64;
        let after_mean: f64 = after.iter().sum::<f64>() / after.len() as f64;
        let speedup = if after_mean > 0.0 {
            before_mean / after_mean
        } else {
            1.0
        };

        let (_, p_value, significant) = Self::mann_whitney_u(before, after);

        let kept = significant && speedup > 1.05; // Require 5% improvement + significance
        let reason = if !significant {
            format!("Not statistically significant (p={:.4})", p_value)
        } else if speedup <= 1.05 {
            format!("Speedup too small ({:.2}x, need >1.05x)", speedup)
        } else {
            format!("Improvement: {:.2}x speedup (p={:.4})", speedup, p_value)
        };

        RewriteResult {
            hypothesis_description: String::new(),
            applied: true,
            tests_passed: true,
            speedup: Some(speedup),
            statistically_significant: significant,
            p_value: Some(p_value),
            before_times_ms: before.to_vec(),
            after_times_ms: after.to_vec(),
            kept,
            reason,
        }
    }
}

/// Standard normal CDF approximation (Abramowitz and Stegun).
fn normal_cdf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.2316419 * x.abs());
    let d = 0.3989422804014327; // 1/√(2π)
    let p = d * (-x * x / 2.0).exp();
    let mut cdf = p
        * t
        * (0.319381530
            + t * (-0.356563782 + t * (1.781477937 + t * (-1.821255978 + t * 1.330274429))));
    if x > 0.0 {
        cdf = 1.0 - cdf;
    }
    cdf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mann_whitney_identical_samples() {
        let a = vec![
            100.0, 101.0, 99.0, 100.5, 100.2, 99.8, 100.1, 100.3, 99.9, 100.0,
        ];
        let b = a.clone();
        let (_, p, significant) = RewriteLoop::mann_whitney_u(&a, &b);
        assert!(
            !significant,
            "Identical samples should not be significant (p={})",
            p
        );
    }

    #[test]
    fn test_mann_whitney_clearly_different() {
        let before = vec![
            100.0, 102.0, 98.0, 101.0, 99.0, 103.0, 97.0, 100.5, 101.5, 99.5,
        ];
        let after = vec![50.0, 52.0, 48.0, 51.0, 49.0, 53.0, 47.0, 50.5, 51.5, 49.5];
        let (_, p, significant) = RewriteLoop::mann_whitney_u(&before, &after);
        assert!(significant, "50% speedup should be significant (p={})", p);
    }

    #[test]
    fn test_evaluate_improvement() {
        let before = vec![100.0; 10];
        let after = vec![50.0; 10];
        let result = RewriteLoop::evaluate_rewrite(&before, &after);
        assert!(result.speedup.unwrap() > 1.5, "Should detect 2x speedup");
        assert!(result.kept, "Should keep significant improvement");
    }

    #[test]
    fn test_evaluate_no_improvement() {
        let before = vec![100.0, 101.0, 99.0, 100.5, 100.2];
        let after = vec![100.1, 100.9, 99.1, 100.4, 100.3];
        let result = RewriteLoop::evaluate_rewrite(&before, &after);
        assert!(!result.kept, "Should not keep marginal change");
    }
}
