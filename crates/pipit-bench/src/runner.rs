//! Benchmark task runner — executes tasks against the pipit agent loop.

use crate::{BenchResult, BenchRun, BenchTask};

/// Run a single benchmark task. Returns the result.
pub async fn run_task(task: &BenchTask, _model: &str) -> BenchResult {
    let start = std::time::Instant::now();

    // Execute: run the test script to verify
    let test_result = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&task.test_script)
        .output()
        .await;

    let elapsed = start.elapsed().as_secs_f64();

    match test_result {
        Ok(output) => BenchResult {
            task_id: task.id.clone(),
            passed: output.status.success(),
            elapsed_secs: elapsed,
            cost_usd: 0.0, // Will be filled by caller from agent metrics
            turns: 0,
            error: if output.status.success() {
                None
            } else {
                Some(String::from_utf8_lossy(&output.stderr).to_string())
            },
        },
        Err(e) => BenchResult {
            task_id: task.id.clone(),
            passed: false,
            elapsed_secs: elapsed,
            cost_usd: 0.0,
            turns: 0,
            error: Some(format!("Execution error: {}", e)),
        },
    }
}

/// Compute aggregate stats for a benchmark run.
pub fn compute_run_stats(suite: &str, model: &str, results: Vec<BenchResult>) -> BenchRun {
    let total = results.len() as f64;
    let passed = results.iter().filter(|r| r.passed).count() as f64;
    let total_cost: f64 = results.iter().map(|r| r.cost_usd).sum();
    let total_time: f64 = results.iter().map(|r| r.elapsed_secs).sum();

    BenchRun {
        suite: suite.to_string(),
        model: model.to_string(),
        config_hash: String::new(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        pass_rate: if total > 0.0 { passed / total } else { 0.0 },
        avg_cost: if total > 0.0 { total_cost / total } else { 0.0 },
        avg_time: if total > 0.0 { total_time / total } else { 0.0 },
        total_cost,
        results,
    }
}
