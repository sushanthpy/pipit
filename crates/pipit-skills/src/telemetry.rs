//! Skill Telemetry — per-skill execution metrics and analytics.
//!
//! Answers: "How often does skill X succeed, at what cost, with what latency?"
//!
//! Data model:
//! - SkillExecutionRecord: one invocation of one skill
//! - SkillTelemetryStore: append-only log with aggregation queries
//!
//! Storage: `.pipit/telemetry/skills.jsonl` (one JSON line per record).
//! Aggregation: O(N) scan with memoized rollups.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A single skill execution record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillExecutionRecord {
    /// Skill package name.
    pub skill_name: String,
    /// Skill package version.
    pub skill_version: String,
    /// ISO-8601 timestamp of invocation start.
    pub started_at: String,
    /// Wall-clock duration in milliseconds.
    pub elapsed_ms: u64,
    /// Number of agent turns consumed.
    pub turns: u32,
    /// Total cost in USD.
    pub cost_usd: f64,
    /// Whether the skill completed successfully.
    pub success: bool,
    /// Error message if failed.
    pub error: Option<String>,
    /// Input token count.
    pub input_tokens: u64,
    /// Output token count.
    pub output_tokens: u64,
    /// Model used for execution.
    pub model: String,
    /// Tools invoked during execution.
    pub tools_used: Vec<String>,
    /// Number of policy denials during execution.
    pub policy_denials: u32,
}

/// Aggregated statistics for a skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillStats {
    pub skill_name: String,
    pub total_invocations: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub success_rate: f64,
    pub avg_cost_usd: f64,
    pub total_cost_usd: f64,
    pub avg_elapsed_ms: f64,
    pub p50_elapsed_ms: u64,
    pub p95_elapsed_ms: u64,
    pub avg_turns: f64,
    pub total_policy_denials: u64,
    /// Last 7 invocations success sparkline.
    pub recent_trend: String,
}

/// Append-only telemetry store for skill executions.
pub struct SkillTelemetryStore {
    records: Vec<SkillExecutionRecord>,
    store_path: PathBuf,
}

impl SkillTelemetryStore {
    /// Load from `.pipit/telemetry/skills.jsonl`.
    pub fn load(project_root: &Path) -> Self {
        let store_path = project_root
            .join(".pipit")
            .join("telemetry")
            .join("skills.jsonl");

        let records = if store_path.exists() {
            std::fs::read_to_string(&store_path)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str::<SkillExecutionRecord>(line).ok())
                .collect()
        } else {
            Vec::new()
        };

        Self {
            records,
            store_path,
        }
    }

    /// Record a skill execution.
    pub fn record(&mut self, record: SkillExecutionRecord) {
        // Append to file
        if let Some(parent) = self.store_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(&record) {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.store_path)
            {
                let _ = writeln!(f, "{}", json);
            }
        }
        self.records.push(record);
    }

    /// Get aggregated stats for a single skill.
    pub fn stats_for(&self, skill_name: &str) -> Option<SkillStats> {
        let records: Vec<&SkillExecutionRecord> = self
            .records
            .iter()
            .filter(|r| r.skill_name == skill_name)
            .collect();

        if records.is_empty() {
            return None;
        }

        let total = records.len() as u64;
        let successes = records.iter().filter(|r| r.success).count() as u64;
        let failures = total - successes;
        let total_cost: f64 = records.iter().map(|r| r.cost_usd).sum();
        let total_time: u64 = records.iter().map(|r| r.elapsed_ms).sum();
        let total_turns: u64 = records.iter().map(|r| r.turns as u64).sum();
        let total_denials: u64 = records.iter().map(|r| r.policy_denials as u64).sum();

        // Percentiles
        let mut latencies: Vec<u64> = records.iter().map(|r| r.elapsed_ms).collect();
        latencies.sort_unstable();
        let p50 = latencies[latencies.len() / 2];
        let p95_idx = (latencies.len() as f64 * 0.95).ceil() as usize;
        let p95 = latencies[p95_idx.min(latencies.len() - 1)];

        // Recent trend sparkline
        let blocks = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let recent: String = records
            .iter()
            .rev()
            .take(7)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|r| if r.success { blocks[7] } else { blocks[0] })
            .collect();

        Some(SkillStats {
            skill_name: skill_name.to_string(),
            total_invocations: total,
            success_count: successes,
            failure_count: failures,
            success_rate: successes as f64 / total as f64,
            avg_cost_usd: total_cost / total as f64,
            total_cost_usd: total_cost,
            avg_elapsed_ms: total_time as f64 / total as f64,
            p50_elapsed_ms: p50,
            p95_elapsed_ms: p95,
            avg_turns: total_turns as f64 / total as f64,
            total_policy_denials: total_denials,
            recent_trend: recent,
        })
    }

    /// Get stats for all skills.
    pub fn all_stats(&self) -> Vec<SkillStats> {
        let mut skill_names: Vec<String> = self
            .records
            .iter()
            .map(|r| r.skill_name.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        skill_names.sort();

        skill_names
            .iter()
            .filter_map(|name| self.stats_for(name))
            .collect()
    }

    /// Format a summary table for TUI display.
    pub fn summary_table(&self) -> String {
        let stats = self.all_stats();
        if stats.is_empty() {
            return "No skill telemetry data.".to_string();
        }

        let mut table = String::from(
            "Skill                  | Invocations | Success | Avg Cost | P50 Latency | Trend\n",
        );
        table.push_str(
            "-----------------------|-------------|---------|----------|-------------|------\n",
        );

        for s in &stats {
            table.push_str(&format!(
                "{:<22} | {:>11} | {:>6.0}% | ${:>7.4} | {:>8}ms | {}\n",
                truncate_str(&s.skill_name, 22),
                s.total_invocations,
                s.success_rate * 100.0,
                s.avg_cost_usd,
                s.p50_elapsed_ms,
                s.recent_trend,
            ));
        }

        table
    }

    /// Total number of recorded executions.
    pub fn total_records(&self) -> usize {
        self.records.len()
    }

    /// Cost per skill — useful for budget dashboards.
    pub fn cost_by_skill(&self) -> HashMap<String, f64> {
        let mut costs = HashMap::new();
        for r in &self.records {
            *costs.entry(r.skill_name.clone()).or_insert(0.0) += r.cost_usd;
        }
        costs
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max.saturating_sub(1);
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(name: &str, success: bool, cost: f64, elapsed_ms: u64) -> SkillExecutionRecord {
        SkillExecutionRecord {
            skill_name: name.to_string(),
            skill_version: "1.0.0".to_string(),
            started_at: "2026-03-30T00:00:00Z".to_string(),
            elapsed_ms,
            turns: 5,
            cost_usd: cost,
            success,
            error: if success { None } else { Some("failed".into()) },
            input_tokens: 1000,
            output_tokens: 500,
            model: "test".into(),
            tools_used: vec!["bash".into()],
            policy_denials: 0,
        }
    }

    #[test]
    fn test_stats_aggregation() {
        let mut store = SkillTelemetryStore {
            records: Vec::new(),
            store_path: PathBuf::from("/tmp/test.jsonl"),
        };

        store
            .records
            .push(make_record("code-review", true, 0.10, 5000));
        store
            .records
            .push(make_record("code-review", true, 0.15, 8000));
        store
            .records
            .push(make_record("code-review", false, 0.20, 12000));
        store.records.push(make_record("lint", true, 0.05, 2000));

        let stats = store.stats_for("code-review").unwrap();
        assert_eq!(stats.total_invocations, 3);
        assert_eq!(stats.success_count, 2);
        assert_eq!(stats.failure_count, 1);
        assert!((stats.success_rate - 0.6667).abs() < 0.01);
        assert!((stats.avg_cost_usd - 0.15).abs() < 0.001);

        let all = store.all_stats();
        assert_eq!(all.len(), 2);

        let costs = store.cost_by_skill();
        assert!((costs["code-review"] - 0.45).abs() < 0.001);
        assert!((costs["lint"] - 0.05).abs() < 0.001);
    }
}
