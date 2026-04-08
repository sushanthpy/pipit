//! Agent profiler — per-turn cost/latency/token flame graph data.

use serde::{Deserialize, Serialize};

/// Metrics collected for a single agent turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnProfile {
    pub turn_number: u32,
    pub ttft_ms: u64,
    pub total_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cost_usd: f64,
    pub tools: Vec<ToolProfile>,
    pub productive: bool,
}

/// Metrics for a single tool execution within a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolProfile {
    pub name: String,
    pub elapsed_ms: u64,
    pub output_bytes: usize,
}

/// A complete profiling session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSession {
    pub turns: Vec<TurnProfile>,
    pub total_cost: f64,
    pub total_time_ms: u64,
}

impl ProfileSession {
    pub fn new() -> Self {
        Self {
            turns: Vec::new(),
            total_cost: 0.0,
            total_time_ms: 0,
        }
    }

    /// Productivity score: 1 - (loop_hits / total_turns).
    pub fn productivity_score(&self) -> f64 {
        if self.turns.is_empty() {
            return 1.0;
        }
        let productive = self.turns.iter().filter(|t| t.productive).count();
        productive as f64 / self.turns.len() as f64
    }

    /// Render a text flame graph for TUI display.
    pub fn flame_graph(&self, width: usize) -> Vec<String> {
        let max_ms = self.turns.iter().map(|t| t.total_ms).max().unwrap_or(1);
        let mut lines = Vec::new();

        for turn in &self.turns {
            let bar_len = (turn.total_ms as f64 / max_ms as f64 * width as f64) as usize;
            let bar: String = "█".repeat(bar_len.max(1));
            let color = if turn.cost_usd > 0.10 {
                "31"
            }
            // red for expensive
            else if turn.cost_usd > 0.03 {
                "33"
            }
            // yellow for moderate
            else {
                "32"
            }; // green for cheap
            lines.push(format!(
                "T{:2} \x1b[{}m{}\x1b[0m {:.0}ms ${:.4} {}tok",
                turn.turn_number,
                color,
                bar,
                turn.total_ms,
                turn.cost_usd,
                turn.input_tokens + turn.output_tokens
            ));
        }
        lines
    }
}
