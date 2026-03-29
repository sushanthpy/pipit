//! Continuous Health Monitor — Task 1.1
//!
//! Closed-loop control: sense → decide → act → verify.
//! Models codebase health as H(t) = Σ wᵢ·hᵢ(t) where each hᵢ uses EWMA.
//! Triggers remediation tasks when H(t) < threshold.

use crate::config::ProjectConfig;
use crate::store::DaemonStore;

use anyhow::Result;
use pipit_channel::{MessageOrigin, NormalizedTask, TaskSink};
use pipit_intelligence::{analyze_dependencies, DependencyHealthReport};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing;

/// EWMA decay factor: α = 0.3 → ~77% of signal from last 5 observations.
const EWMA_ALPHA: f64 = 0.3;

/// Default health threshold below which remediation tasks are generated.
const DEFAULT_HEALTH_THRESHOLD: f64 = 0.6;

/// Health metric IDs and default weights.
const METRICS: &[(&str, f64)] = &[
    ("dep_freshness", 0.25),
    ("test_pass_rate", 0.30),
    ("lint_score", 0.20),
    ("dead_code_ratio", 0.10),
    ("doc_coverage", 0.15),
];

/// Per-project health state tracked by the monitor.
#[derive(Debug, Clone)]
pub struct ProjectHealth {
    pub project: String,
    pub metrics: HashMap<String, EwmaMetric>,
    pub composite_health: f64,
    pub last_check: std::time::Instant,
}

/// Exponentially Weighted Moving Average for a single metric.
#[derive(Debug, Clone)]
pub struct EwmaMetric {
    pub name: String,
    pub weight: f64,
    pub value: f64,    // Current EWMA value in [0, 1]
    pub raw_last: f64, // Last raw observation
    pub samples: u32,
}

impl EwmaMetric {
    pub fn new(name: &str, weight: f64) -> Self {
        Self {
            name: name.to_string(),
            weight,
            value: 1.0, // Assume healthy until proven otherwise
            raw_last: 1.0,
            samples: 0,
        }
    }

    /// Update with new observation. EWMA: h(t) = α·raw + (1-α)·h(t-1). O(1).
    pub fn update(&mut self, raw: f64) {
        self.raw_last = raw.clamp(0.0, 1.0);
        if self.samples == 0 {
            self.value = self.raw_last;
        } else {
            self.value = EWMA_ALPHA * self.raw_last + (1.0 - EWMA_ALPHA) * self.value;
        }
        self.samples += 1;
    }
}

impl ProjectHealth {
    pub fn new(project: &str) -> Self {
        let metrics = METRICS
            .iter()
            .map(|(name, weight)| (name.to_string(), EwmaMetric::new(name, *weight)))
            .collect();
        Self {
            project: project.to_string(),
            metrics,
            composite_health: 1.0,
            last_check: std::time::Instant::now(),
        }
    }

    /// Recompute composite health: H(t) = Σ wᵢ·hᵢ(t)
    pub fn recompute(&mut self) {
        self.composite_health = self
            .metrics
            .values()
            .map(|m| m.weight * m.value)
            .sum();
        self.last_check = std::time::Instant::now();
    }

    /// Check if health is below threshold.
    pub fn needs_remediation(&self, threshold: f64) -> bool {
        self.composite_health < threshold
    }

    /// Generate a remediation task prompt from the worst metrics.
    pub fn remediation_prompt(&self) -> String {
        let mut worst: Vec<_> = self.metrics.values().collect();
        worst.sort_by(|a, b| a.value.partial_cmp(&b.value).unwrap_or(std::cmp::Ordering::Equal));

        let issues: Vec<String> = worst
            .iter()
            .take(3)
            .filter(|m| m.value < 0.7)
            .map(|m| format!("- {} health: {:.0}% (last raw: {:.0}%)", m.name, m.value * 100.0, m.raw_last * 100.0))
            .collect();

        if issues.is_empty() {
            return String::new();
        }

        format!(
            "Automated health check detected issues in project '{}':\n{}\n\n\
             Please investigate and fix the most critical issue. Run tests to verify.",
            self.project,
            issues.join("\n")
        )
    }
}

/// Run health checks for a project and update metrics.
pub fn check_project_health(
    health: &mut ProjectHealth,
    project_config: &ProjectConfig,
) {
    // Dependency freshness
    let dep_reports = analyze_dependencies(&project_config.root);
    let dep_health: f64 = if dep_reports.is_empty() {
        1.0
    } else {
        dep_reports.iter().map(|r| r.overall_health).sum::<f64>() / dep_reports.len() as f64
    };
    if let Some(m) = health.metrics.get_mut("dep_freshness") {
        m.update(dep_health);
    }

    // Test pass rate (run test command if configured)
    if let Some(ref test_cmd) = project_config.test_command {
        let result = std::process::Command::new("sh")
            .arg("-c")
            .arg(test_cmd)
            .current_dir(&project_config.root)
            .output();

        let pass_rate = match result {
            Ok(output) => if output.status.success() { 1.0 } else { 0.0 },
            Err(_) => 0.5, // Can't run tests → neutral
        };
        if let Some(m) = health.metrics.get_mut("test_pass_rate") {
            m.update(pass_rate);
        }
    }

    // Other metrics get defaults until more analyzers are built
    // (lint_score, dead_code_ratio, doc_coverage set to 0.8 baseline)
    for name in &["lint_score", "dead_code_ratio", "doc_coverage"] {
        if let Some(m) = health.metrics.get_mut(*name) {
            if m.samples == 0 {
                m.update(0.8);
            }
        }
    }

    health.recompute();
}

/// Spawn the health monitor loop. Checks all projects periodically.
pub async fn health_monitor_loop(
    projects: HashMap<String, ProjectConfig>,
    store: Arc<DaemonStore>,
    task_sink: TaskSink,
    check_interval: std::time::Duration,
    threshold: f64,
    cancel: CancellationToken,
) {
    let mut health_states: HashMap<String, ProjectHealth> = projects
        .keys()
        .map(|name| (name.clone(), ProjectHealth::new(name)))
        .collect();

    tracing::info!(
        projects = health_states.len(),
        interval = ?check_interval,
        threshold,
        "health monitor started"
    );

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("health monitor shutting down");
                break;
            }
            _ = tokio::time::sleep(check_interval) => {
                for (name, config) in &projects {
                    if let Some(health) = health_states.get_mut(name) {
                        check_project_health(health, config);
                        tracing::debug!(
                            project = %name,
                            health = format!("{:.1}%", health.composite_health * 100.0),
                            "health check complete"
                        );

                        if health.needs_remediation(threshold) {
                            let prompt = health.remediation_prompt();
                            if !prompt.is_empty() {
                                let task = NormalizedTask::new(
                                    name.clone(),
                                    prompt,
                                    MessageOrigin::Cron {
                                        schedule_name: "health_monitor".to_string(),
                                        notification_origin: None,
                                    },
                                );
                                if let Err(e) = task_sink.send(task).await {
                                    tracing::error!(error = %e, "failed to submit health remediation task");
                                } else {
                                    tracing::info!(
                                        project = %name,
                                        health = format!("{:.1}%", health.composite_health * 100.0),
                                        "submitted health remediation task"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ewma_convergence() {
        let mut m = EwmaMetric::new("test", 1.0);

        // Feed 10 observations of 0.8
        for _ in 0..10 {
            m.update(0.8);
        }
        // EWMA should converge near 0.8
        assert!((m.value - 0.8).abs() < 0.05, "EWMA should converge: {}", m.value);
    }

    #[test]
    fn test_ewma_filters_noise() {
        let mut m = EwmaMetric::new("test", 1.0);

        // Steady state at 0.9
        for _ in 0..5 {
            m.update(0.9);
        }
        let before = m.value;

        // Single noisy drop
        m.update(0.1);
        let after_noise = m.value;

        // EWMA should dampen: drop should be < 50% of raw drop
        let raw_drop = 0.9 - 0.1;
        let ewma_drop = before - after_noise;
        assert!(ewma_drop < raw_drop * 0.5, "EWMA should dampen noise: drop={}", ewma_drop);
    }

    #[test]
    fn test_composite_health() {
        let mut h = ProjectHealth::new("test-project");

        // Set all metrics to 0.5
        for m in h.metrics.values_mut() {
            m.update(0.5);
        }
        h.recompute();

        // Composite should be ~0.5 (sum of weights * 0.5)
        assert!((h.composite_health - 0.5).abs() < 0.1, "health = {}", h.composite_health);
        assert!(h.needs_remediation(0.6));
        assert!(!h.needs_remediation(0.4));
    }

    #[test]
    fn test_remediation_prompt_generation() {
        let mut h = ProjectHealth::new("my-project");
        h.metrics.get_mut("test_pass_rate").unwrap().update(0.3);
        h.metrics.get_mut("dep_freshness").unwrap().update(0.4);
        h.recompute();

        let prompt = h.remediation_prompt();
        assert!(prompt.contains("my-project"));
        assert!(prompt.contains("test_pass_rate"));
    }
}
