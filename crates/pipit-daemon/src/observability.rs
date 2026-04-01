//! Worker Observability — per-worker metrics, dashboards, and alerting.
//!
//! Answers: "Worker X has handled 47 cases this week, 3 escalated,
//! avg resolution 12 minutes."
//!
//! Architecture:
//! - WorkerMetricsCollector: periodically polls workers, aggregates stats
//! - WorkerDashboard: renders text-based dashboard for TUI/API
//! - AlertRule: threshold-based alerting on worker metrics

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::worker::{WorkerHandle, WorkerId, WorkerState, WorkerStatus};

// ── Metric snapshots ────────────────────────────────────────────────

/// Point-in-time snapshot of a worker's metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerMetricSnapshot {
    pub worker_id: String,
    pub worker_name: String,
    pub state: WorkerState,
    pub tasks_handled: u64,
    pub tasks_failed: u64,
    pub success_rate: f64,
    pub uptime_secs: u64,
    pub current_task: Option<String>,
    pub timestamp: String,
}

impl From<WorkerStatus> for WorkerMetricSnapshot {
    fn from(s: WorkerStatus) -> Self {
        let total = s.tasks_handled + s.tasks_failed;
        let success_rate = if total > 0 {
            s.tasks_handled as f64 / total as f64
        } else {
            1.0
        };
        Self {
            worker_id: s.id.0,
            worker_name: String::new(),
            state: s.state,
            tasks_handled: s.tasks_handled,
            tasks_failed: s.tasks_failed,
            success_rate,
            uptime_secs: s.uptime_secs,
            current_task: s.current_task,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }
}

// ── Aggregate fleet metrics ─────────────────────────────────────────

/// Aggregate metrics across all workers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetMetrics {
    pub total_workers: usize,
    pub active_workers: usize,
    pub idle_workers: usize,
    pub failed_workers: usize,
    pub total_tasks_handled: u64,
    pub total_tasks_failed: u64,
    pub fleet_success_rate: f64,
    pub total_uptime_hours: f64,
    pub worker_snapshots: Vec<WorkerMetricSnapshot>,
}

impl FleetMetrics {
    /// Compute from a set of worker snapshots.
    pub fn from_snapshots(snapshots: Vec<WorkerMetricSnapshot>) -> Self {
        let total_workers = snapshots.len();
        let active_workers = snapshots
            .iter()
            .filter(|s| s.state == WorkerState::Working)
            .count();
        let idle_workers = snapshots
            .iter()
            .filter(|s| s.state == WorkerState::Idle)
            .count();
        let failed_workers = snapshots
            .iter()
            .filter(|s| s.state == WorkerState::Failed)
            .count();
        let total_handled: u64 = snapshots.iter().map(|s| s.tasks_handled).sum();
        let total_failed: u64 = snapshots.iter().map(|s| s.tasks_failed).sum();
        let total = total_handled + total_failed;
        let success_rate = if total > 0 {
            total_handled as f64 / total as f64
        } else {
            1.0
        };
        let total_uptime: u64 = snapshots.iter().map(|s| s.uptime_secs).sum();

        Self {
            total_workers,
            active_workers,
            idle_workers,
            failed_workers,
            total_tasks_handled: total_handled,
            total_tasks_failed: total_failed,
            fleet_success_rate: success_rate,
            total_uptime_hours: total_uptime as f64 / 3600.0,
            worker_snapshots: snapshots,
        }
    }
}

// ── Alert rules ─────────────────────────────────────────────────────

/// A threshold-based alert rule on worker metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertRule {
    pub name: String,
    pub metric: AlertMetric,
    pub threshold: f64,
    pub comparison: AlertComparison,
    pub severity: AlertSeverity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertMetric {
    SuccessRate,
    TasksFailedTotal,
    UptimeHours,
    IdleWorkerRatio,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertComparison {
    LessThan,
    GreaterThan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
}

/// A triggered alert.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    pub rule_name: String,
    pub severity: AlertSeverity,
    pub message: String,
    pub current_value: f64,
    pub threshold: f64,
    pub timestamp: String,
}

impl AlertRule {
    /// Evaluate this rule against fleet metrics.
    pub fn evaluate(&self, metrics: &FleetMetrics) -> Option<Alert> {
        let current_value = match self.metric {
            AlertMetric::SuccessRate => metrics.fleet_success_rate,
            AlertMetric::TasksFailedTotal => metrics.total_tasks_failed as f64,
            AlertMetric::UptimeHours => metrics.total_uptime_hours,
            AlertMetric::IdleWorkerRatio => {
                if metrics.total_workers > 0 {
                    metrics.idle_workers as f64 / metrics.total_workers as f64
                } else {
                    0.0
                }
            }
        };

        let triggered = match self.comparison {
            AlertComparison::LessThan => current_value < self.threshold,
            AlertComparison::GreaterThan => current_value > self.threshold,
        };

        if triggered {
            Some(Alert {
                rule_name: self.name.clone(),
                severity: self.severity.clone(),
                message: format!(
                    "{}: {:.2} {} {:.2}",
                    self.name,
                    current_value,
                    match self.comparison {
                        AlertComparison::LessThan => "<",
                        AlertComparison::GreaterThan => ">",
                    },
                    self.threshold,
                ),
                current_value,
                threshold: self.threshold,
                timestamp: chrono::Utc::now().to_rfc3339(),
            })
        } else {
            None
        }
    }
}

// ── Dashboard renderer ──────────────────────────────────────────────

/// Render a text-based worker dashboard.
pub fn render_dashboard(metrics: &FleetMetrics) -> String {
    let mut out = String::new();

    out.push_str("╔══════════════════════════════════════════════════════════════╗\n");
    out.push_str("║                    WORKER FLEET DASHBOARD                   ║\n");
    out.push_str("╠══════════════════════════════════════════════════════════════╣\n");

    out.push_str(&format!(
        "║ Workers: {} total │ {} active │ {} idle │ {} failed          \n",
        metrics.total_workers, metrics.active_workers, metrics.idle_workers, metrics.failed_workers
    ));
    out.push_str(&format!(
        "║ Tasks: {} handled │ {} failed │ {:.1}% success rate         \n",
        metrics.total_tasks_handled,
        metrics.total_tasks_failed,
        metrics.fleet_success_rate * 100.0,
    ));
    out.push_str(&format!(
        "║ Uptime: {:.1} hours                                        \n",
        metrics.total_uptime_hours,
    ));

    out.push_str("╠══════════════════════════════════════════════════════════════╣\n");
    out.push_str("║ Worker             │ State    │ Tasks │ Success │ Current   ║\n");
    out.push_str("╠══════════════════════════════════════════════════════════════╣\n");

    for snap in &metrics.worker_snapshots {
        let state_str = match snap.state {
            WorkerState::Working => "WORKING ",
            WorkerState::Idle => "IDLE    ",
            WorkerState::Failed => "FAILED  ",
            WorkerState::Stopped => "STOPPED ",
            WorkerState::Initializing => "INIT    ",
            WorkerState::ShuttingDown => "SHUTTING",
        };
        let current = snap
            .current_task
            .as_deref()
            .unwrap_or("-")
            .chars()
            .take(8)
            .collect::<String>();

        out.push_str(&format!(
            "║ {:18} │ {} │ {:>5} │ {:>6.1}% │ {:8} ║\n",
            truncate(&snap.worker_id, 18),
            state_str,
            snap.tasks_handled + snap.tasks_failed,
            snap.success_rate * 100.0,
            current,
        ));
    }

    out.push_str("╚══════════════════════════════════════════════════════════════╝\n");
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        format!("{:<width$}", s, width = max)
    } else {
        format!("{}…", &s[..max - 1])
    }
}

/// Default alert rules for a worker fleet.
pub fn default_alert_rules() -> Vec<AlertRule> {
    vec![
        AlertRule {
            name: "Low success rate".to_string(),
            metric: AlertMetric::SuccessRate,
            threshold: 0.7,
            comparison: AlertComparison::LessThan,
            severity: AlertSeverity::Warning,
        },
        AlertRule {
            name: "Critical failure rate".to_string(),
            metric: AlertMetric::SuccessRate,
            threshold: 0.5,
            comparison: AlertComparison::LessThan,
            severity: AlertSeverity::Critical,
        },
        AlertRule {
            name: "High failure count".to_string(),
            metric: AlertMetric::TasksFailedTotal,
            threshold: 10.0,
            comparison: AlertComparison::GreaterThan,
            severity: AlertSeverity::Warning,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_snapshot(id: &str, state: WorkerState, handled: u64, failed: u64) -> WorkerMetricSnapshot {
        WorkerMetricSnapshot {
            worker_id: id.into(),
            worker_name: id.into(),
            state,
            tasks_handled: handled,
            tasks_failed: failed,
            success_rate: if handled + failed > 0 {
                handled as f64 / (handled + failed) as f64
            } else {
                1.0
            },
            uptime_secs: 3600,
            current_task: None,
            timestamp: "2026-03-30T00:00:00Z".into(),
        }
    }

    #[test]
    fn test_fleet_metrics() {
        let snapshots = vec![
            make_snapshot("worker-1", WorkerState::Working, 40, 3),
            make_snapshot("worker-2", WorkerState::Idle, 7, 0),
            make_snapshot("worker-3", WorkerState::Failed, 10, 5),
        ];

        let fleet = FleetMetrics::from_snapshots(snapshots);
        assert_eq!(fleet.total_workers, 3);
        assert_eq!(fleet.active_workers, 1);
        assert_eq!(fleet.idle_workers, 1);
        assert_eq!(fleet.failed_workers, 1);
        assert_eq!(fleet.total_tasks_handled, 57);
        assert_eq!(fleet.total_tasks_failed, 8);
    }

    #[test]
    fn test_alert_triggers() {
        let snapshots = vec![
            make_snapshot("w1", WorkerState::Working, 3, 7), // 30% success
        ];
        let fleet = FleetMetrics::from_snapshots(snapshots);

        let rule = AlertRule {
            name: "Low success".into(),
            metric: AlertMetric::SuccessRate,
            threshold: 0.5,
            comparison: AlertComparison::LessThan,
            severity: AlertSeverity::Critical,
        };

        let alert = rule.evaluate(&fleet);
        assert!(alert.is_some());
        assert!(alert.unwrap().message.contains("Low success"));
    }

    #[test]
    fn test_dashboard_renders() {
        let snapshots = vec![
            make_snapshot("ci-fixer", WorkerState::Working, 47, 3),
            make_snapshot("code-reviewer", WorkerState::Idle, 120, 5),
        ];
        let fleet = FleetMetrics::from_snapshots(snapshots);
        let dashboard = render_dashboard(&fleet);
        assert!(dashboard.contains("WORKER FLEET DASHBOARD"));
        assert!(dashboard.contains("ci-fixer"));
        assert!(dashboard.contains("code-reviewer"));
    }
}
