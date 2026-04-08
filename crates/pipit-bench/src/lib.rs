//! pipit-bench: Self-benchmarking agent evaluation harness.
//!
//! Run Terminal-Bench tasks, SWE-bench instances, and custom eval suites.
//! Track performance across model upgrades with regression detection.
//!
//! ## Architecture
//! ```text
//! BenchRunner → TaskRunner → Docker container
//!     ↓              ↓
//! BenchHistory   BenchResult (pass/fail, cost, time)
//!     ↓
//! Regression detection (Z-score against rolling window)
//! ```

pub mod history;
pub mod profiler;
pub mod runner;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A benchmark suite containing multiple tasks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchSuite {
    pub name: String,
    pub description: String,
    pub tasks: Vec<BenchTask>,
}

/// A single benchmark task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchTask {
    pub id: String,
    pub instruction: String,
    pub dockerfile: Option<String>,
    pub test_script: String,
    pub timeout_secs: u64,
}

/// Result of running a single benchmark task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchResult {
    pub task_id: String,
    pub passed: bool,
    pub elapsed_secs: f64,
    pub cost_usd: f64,
    pub turns: u32,
    pub error: Option<String>,
}

/// Result of an entire benchmark run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchRun {
    pub suite: String,
    pub model: String,
    pub config_hash: String,
    pub timestamp: String,
    pub results: Vec<BenchResult>,
    pub pass_rate: f64,
    pub avg_cost: f64,
    pub avg_time: f64,
    pub total_cost: f64,
}

impl BenchRun {
    pub fn summary_line(&self) -> String {
        format!(
            "{}: {}/{} passed ({:.0}%) — avg ${:.4}/task, {:.1}s/task — model: {}",
            self.suite,
            self.results.iter().filter(|r| r.passed).count(),
            self.results.len(),
            self.pass_rate * 100.0,
            self.avg_cost,
            self.avg_time,
            self.model,
        )
    }
}

/// Load custom benchmark suites from `.pipit/benchmarks/`.
pub fn load_custom_suites(project_root: &Path) -> Vec<BenchSuite> {
    let bench_dir = project_root.join(".pipit").join("benchmarks");
    if !bench_dir.exists() {
        return Vec::new();
    }

    let mut suites = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&bench_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let instruction_path = path.join("instruction.md");
                let test_path = path.join("test.sh");

                if instruction_path.exists() && test_path.exists() {
                    let instruction =
                        std::fs::read_to_string(&instruction_path).unwrap_or_default();
                    let test_script = std::fs::read_to_string(&test_path).unwrap_or_default();
                    let dockerfile = path.join("Dockerfile");

                    suites.push(BenchSuite {
                        name: name.clone(),
                        description: format!("Custom benchmark: {}", name),
                        tasks: vec![BenchTask {
                            id: name,
                            instruction,
                            dockerfile: if dockerfile.exists() {
                                Some(std::fs::read_to_string(&dockerfile).unwrap_or_default())
                            } else {
                                None
                            },
                            test_script,
                            timeout_secs: 120,
                        }],
                    });
                }
            }
        }
    }
    suites
}
