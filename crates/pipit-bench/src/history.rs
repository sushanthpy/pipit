//! Benchmark history — persistence and regression detection.

use crate::BenchRun;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Append-only benchmark history stored in `.pipit/bench/history.jsonl`.
pub struct BenchHistory {
    runs: Vec<BenchRun>,
}

impl BenchHistory {
    /// Load history from disk.
    pub fn load(project_root: &Path) -> Self {
        let path = project_root.join(".pipit").join("bench").join("history.jsonl");
        let runs = if path.exists() {
            std::fs::read_to_string(&path)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str::<BenchRun>(line).ok())
                .collect()
        } else {
            Vec::new()
        };
        Self { runs }
    }

    /// Append a new run.
    pub fn append(&mut self, run: &BenchRun, project_root: &Path) {
        let dir = project_root.join(".pipit").join("bench");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("history.jsonl");
        if let Ok(json) = serde_json::to_string(run) {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                let _ = writeln!(f, "{}", json);
            }
        }
        self.runs.push(run.clone());
    }

    /// Check for regression: Z-score against rolling window of last 20 runs.
    /// Returns Some(z_score) if z > 2.0 (statistically significant decline).
    pub fn check_regression(&self, suite: &str) -> Option<f64> {
        let suite_runs: Vec<&BenchRun> = self.runs.iter()
            .filter(|r| r.suite == suite)
            .collect();

        if suite_runs.len() < 5 {
            return None; // Not enough data
        }

        let window = &suite_runs[suite_runs.len().saturating_sub(20)..suite_runs.len() - 1];
        let latest = suite_runs.last()?;

        let mean: f64 = window.iter().map(|r| r.pass_rate).sum::<f64>() / window.len() as f64;
        let variance: f64 = window.iter()
            .map(|r| (r.pass_rate - mean).powi(2))
            .sum::<f64>() / window.len() as f64;
        let std_dev = variance.sqrt();

        if std_dev < 0.001 {
            return None; // No variance
        }

        let z_score = (mean - latest.pass_rate) / std_dev;
        if z_score > 2.0 {
            Some(z_score)
        } else {
            None
        }
    }

    /// Get all runs for a suite.
    pub fn runs_for_suite(&self, suite: &str) -> Vec<&BenchRun> {
        self.runs.iter().filter(|r| r.suite == suite).collect()
    }

    /// Get a sparkline of pass rates for display.
    pub fn sparkline(&self, suite: &str, width: usize) -> String {
        let runs = self.runs_for_suite(suite);
        let blocks = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let recent: Vec<f64> = runs.iter().rev().take(width).rev().map(|r| r.pass_rate).collect();
        recent.iter().map(|&rate| {
            let idx = (rate * 7.0).round() as usize;
            blocks[idx.min(7)]
        }).collect()
    }
}
