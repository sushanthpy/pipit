//! Cost Oracle — predictive task cost estimation (ECO-5).
//!
//! Before executing a task, estimates cost based on:
//! - Similar past tasks (by prompt length/type)
//! - Expected turns (Poisson model from history)
//! - Current model's pricing
//!
//! Displays: "Estimated cost: $0.15-0.45 | Estimated time: 2-5 minutes"

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Write;
use std::path::Path;

/// Atomically write data to a file: write(tmp) → fsync → rename(tmp, target).
fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut file = File::create(&tmp)?;
    file.write_all(data)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Historical task record for cost prediction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub prompt_length: usize,
    pub file_count: usize,
    pub turns: u32,
    pub cost_usd: f64,
    pub elapsed_secs: f64,
    pub task_type: TaskType,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TaskType {
    Question,
    SmallEdit,
    MultiFileEdit,
    Refactor,
    FeatureImplementation,
    BugFix,
    Review,
    Unknown,
}

/// Cost prediction result.
#[derive(Debug, Clone)]
pub struct CostEstimate {
    pub estimated_turns: f64,
    pub cost_low: f64,
    pub cost_high: f64,
    pub time_low_secs: f64,
    pub time_high_secs: f64,
    pub confidence: f64,
}

impl CostEstimate {
    /// Format for display.
    pub fn display(&self) -> String {
        let time_low = format_duration(self.time_low_secs);
        let time_high = format_duration(self.time_high_secs);
        format!(
            "Estimated cost: ${:.2}-{:.2} | Estimated time: {}-{} | Confidence: {:.0}%",
            self.cost_low,
            self.cost_high,
            time_low,
            time_high,
            self.confidence * 100.0
        )
    }
}

fn format_duration(secs: f64) -> String {
    if secs < 60.0 {
        format!("{:.0}s", secs)
    } else {
        format!("{:.0}m", secs / 60.0)
    }
}

/// Cost oracle: predicts task cost from historical data.
pub struct CostOracle {
    history: Vec<TaskRecord>,
}

impl CostOracle {
    /// Load history from `.pipit/cost-history.json`.
    pub fn load(project_root: &Path) -> Self {
        let path = project_root.join(".pipit").join("cost-history.json");
        let history = if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Self { history }
    }

    /// Record a completed task for future predictions.
    pub fn record(&mut self, record: TaskRecord, project_root: &Path) {
        self.history.push(record);
        // Keep last 200 records
        if self.history.len() > 200 {
            self.history.drain(..self.history.len() - 200);
        }
        let path = project_root.join(".pipit").join("cost-history.json");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = atomic_write(
            &path,
            serde_json::to_string(&self.history)
                .unwrap_or_default()
                .as_bytes(),
        );
    }

    /// Predict cost for a new task.
    pub fn predict(
        &self,
        prompt_length: usize,
        file_count: usize,
        price_per_1k_tokens: f64,
    ) -> CostEstimate {
        if self.history.is_empty() {
            // No history — use heuristic
            let estimated_turns = classify_complexity(prompt_length, file_count);
            let tokens_per_turn = 3000.0; // rough average
            let cost_per_turn = tokens_per_turn / 1000.0 * price_per_1k_tokens;
            return CostEstimate {
                estimated_turns,
                cost_low: cost_per_turn * estimated_turns * 0.5,
                cost_high: cost_per_turn * estimated_turns * 2.0,
                time_low_secs: estimated_turns * 8.0,
                time_high_secs: estimated_turns * 30.0,
                confidence: 0.3,
            };
        }

        // Find similar tasks by prompt length bucket
        let similar: Vec<&TaskRecord> = self
            .history
            .iter()
            .filter(|r| {
                let ratio = r.prompt_length as f64 / prompt_length.max(1) as f64;
                ratio > 0.3 && ratio < 3.0
            })
            .collect();

        if similar.is_empty() {
            return self.predict_from_heuristic(prompt_length, file_count, price_per_1k_tokens);
        }

        let turns: Vec<f64> = similar.iter().map(|r| r.turns as f64).collect();
        let costs: Vec<f64> = similar.iter().map(|r| r.cost_usd).collect();
        let times: Vec<f64> = similar.iter().map(|r| r.elapsed_secs).collect();

        let mean_turns = turns.iter().sum::<f64>() / turns.len() as f64;
        let mean_cost = costs.iter().sum::<f64>() / costs.len() as f64;
        let mean_time = times.iter().sum::<f64>() / times.len() as f64;

        let std_cost = (costs.iter().map(|c| (c - mean_cost).powi(2)).sum::<f64>()
            / costs.len() as f64)
            .sqrt();
        let std_time = (times.iter().map(|t| (t - mean_time).powi(2)).sum::<f64>()
            / times.len() as f64)
            .sqrt();

        CostEstimate {
            estimated_turns: mean_turns,
            cost_low: (mean_cost - 2.0 * std_cost).max(0.001),
            cost_high: mean_cost + 2.0 * std_cost,
            time_low_secs: (mean_time - 2.0 * std_time).max(5.0),
            time_high_secs: mean_time + 2.0 * std_time,
            confidence: (similar.len() as f64 / 20.0).min(0.95),
        }
    }

    fn predict_from_heuristic(
        &self,
        prompt_length: usize,
        file_count: usize,
        price: f64,
    ) -> CostEstimate {
        let turns = classify_complexity(prompt_length, file_count);
        let cost_per_turn = 3.0 * price; // ~3K tokens/turn
        CostEstimate {
            estimated_turns: turns,
            cost_low: cost_per_turn * turns * 0.5,
            cost_high: cost_per_turn * turns * 2.0,
            time_low_secs: turns * 10.0,
            time_high_secs: turns * 40.0,
            confidence: 0.2,
        }
    }
}

/// Classify task complexity by prompt length and file count.
fn classify_complexity(prompt_length: usize, file_count: usize) -> f64 {
    let base = if prompt_length < 100 {
        2.0
    } else if prompt_length < 500 {
        4.0
    } else if prompt_length < 2000 {
        8.0
    } else {
        15.0
    };

    let file_factor = 1.0 + (file_count as f64 * 0.5).min(5.0);
    base * file_factor / file_factor.sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_oracle_prediction() {
        let oracle = CostOracle {
            history: Vec::new(),
        };
        let estimate = oracle.predict(100, 2, 0.003);
        assert!(estimate.cost_low > 0.0);
        assert!(estimate.cost_high > estimate.cost_low);
        assert!(estimate.confidence < 0.5);
    }

    #[test]
    fn test_oracle_with_history() {
        let oracle = CostOracle {
            history: vec![
                TaskRecord {
                    prompt_length: 100,
                    file_count: 1,
                    turns: 3,
                    cost_usd: 0.05,
                    elapsed_secs: 20.0,
                    task_type: TaskType::SmallEdit,
                },
                TaskRecord {
                    prompt_length: 120,
                    file_count: 1,
                    turns: 4,
                    cost_usd: 0.08,
                    elapsed_secs: 30.0,
                    task_type: TaskType::SmallEdit,
                },
                TaskRecord {
                    prompt_length: 80,
                    file_count: 2,
                    turns: 5,
                    cost_usd: 0.12,
                    elapsed_secs: 45.0,
                    task_type: TaskType::BugFix,
                },
            ],
        };
        let estimate = oracle.predict(110, 1, 0.003);
        assert!(estimate.estimated_turns > 1.0);
        assert!(estimate.confidence > 0.1);
    }
}
