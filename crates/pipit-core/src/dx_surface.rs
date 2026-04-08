//! Developer Experience Surface — Diagnostics, Theming, Cost Visualization
//!
//! Powers /doctor, /theme, /cost, /context commands with structured output
//! and TUI rendering primitives.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

// ═══════════════════════════════════════════════════════════════════════
//  System Diagnostics (/doctor)
// ═══════════════════════════════════════════════════════════════════════

/// A diagnostic check result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticCheck {
    pub name: String,
    pub status: DiagnosticStatus,
    pub message: String,
    pub fix_hint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticStatus {
    Pass,
    Warning,
    Fail,
}

/// Run all diagnostic checks.
pub fn run_diagnostics(project_root: &Path) -> Vec<DiagnosticCheck> {
    let mut checks = Vec::new();

    // 1. Git status
    checks.push(check_git(project_root));

    // 2. Project structure
    checks.push(check_project_structure(project_root));

    // 3. Config file
    checks.push(check_config(project_root));

    // 4. API connectivity (can't do async here, so check env vars)
    checks.push(check_api_keys());

    // 5. Skills discovery
    checks.push(check_skills(project_root));

    // 6. MCP config
    checks.push(check_mcp(project_root));

    checks
}

fn check_git(root: &Path) -> DiagnosticCheck {
    if root.join(".git").exists() {
        DiagnosticCheck {
            name: "Git repository".into(),
            status: DiagnosticStatus::Pass,
            message: "Git repository detected".into(),
            fix_hint: None,
        }
    } else {
        DiagnosticCheck {
            name: "Git repository".into(),
            status: DiagnosticStatus::Warning,
            message: "Not a git repository — /undo and worktree isolation won't work".into(),
            fix_hint: Some("Run `git init` to initialize".into()),
        }
    }
}

fn check_project_structure(root: &Path) -> DiagnosticCheck {
    let pipit_dir = root.join(".pipit");
    if pipit_dir.exists() {
        DiagnosticCheck {
            name: "Project config".into(),
            status: DiagnosticStatus::Pass,
            message: ".pipit/ directory found".into(),
            fix_hint: None,
        }
    } else {
        DiagnosticCheck {
            name: "Project config".into(),
            status: DiagnosticStatus::Warning,
            message: "No .pipit/ directory — using defaults".into(),
            fix_hint: Some("Run `pipit setup` to create project config".into()),
        }
    }
}

fn check_config(root: &Path) -> DiagnosticCheck {
    let config_paths = [root.join(".pipit/config.toml"), dirs_config_path()];
    for path in &config_paths {
        if path.exists() {
            return DiagnosticCheck {
                name: "Configuration".into(),
                status: DiagnosticStatus::Pass,
                message: format!("Config found at {}", path.display()),
                fix_hint: None,
            };
        }
    }
    DiagnosticCheck {
        name: "Configuration".into(),
        status: DiagnosticStatus::Warning,
        message: "No config file found — using defaults".into(),
        fix_hint: Some("Run `pipit setup` or create ~/.config/pipit/config.toml".into()),
    }
}

fn check_api_keys() -> DiagnosticCheck {
    let known_keys = ["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "GOOGLE_API_KEY"];
    let found: Vec<&str> = known_keys
        .iter()
        .filter(|k| std::env::var(k).is_ok())
        .copied()
        .collect();

    if found.is_empty() {
        DiagnosticCheck {
            name: "API keys".into(),
            status: DiagnosticStatus::Fail,
            message: "No API keys found in environment".into(),
            fix_hint: Some(
                "Set ANTHROPIC_API_KEY, OPENAI_API_KEY, or run `pipit auth login`".into(),
            ),
        }
    } else {
        DiagnosticCheck {
            name: "API keys".into(),
            status: DiagnosticStatus::Pass,
            message: format!("Found: {}", found.join(", ")),
            fix_hint: None,
        }
    }
}

fn check_skills(root: &Path) -> DiagnosticCheck {
    let skills_dir = root.join(".pipit/skills");
    if skills_dir.exists() {
        let count = std::fs::read_dir(&skills_dir)
            .map(|d| {
                d.filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().map(|ext| ext == "md").unwrap_or(false))
                    .count()
            })
            .unwrap_or(0);
        DiagnosticCheck {
            name: "Skills".into(),
            status: DiagnosticStatus::Pass,
            message: format!("{} skill(s) discovered", count),
            fix_hint: None,
        }
    } else {
        DiagnosticCheck {
            name: "Skills".into(),
            status: DiagnosticStatus::Pass,
            message: "No project skills (using builtins only)".into(),
            fix_hint: None,
        }
    }
}

fn check_mcp(root: &Path) -> DiagnosticCheck {
    let mcp_paths = [root.join(".pipit/mcp.json"), root.join("mcp.json")];
    for path in &mcp_paths {
        if path.exists() {
            return DiagnosticCheck {
                name: "MCP servers".into(),
                status: DiagnosticStatus::Pass,
                message: format!("MCP config at {}", path.display()),
                fix_hint: None,
            };
        }
    }
    DiagnosticCheck {
        name: "MCP servers".into(),
        status: DiagnosticStatus::Pass,
        message: "No MCP servers configured".into(),
        fix_hint: None,
    }
}

fn dirs_config_path() -> std::path::PathBuf {
    dirs_home_path().join(".config/pipit/config.toml")
}

fn dirs_home_path() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
}

/// Format diagnostic results for terminal display.
pub fn format_diagnostics(checks: &[DiagnosticCheck]) -> String {
    let mut output = String::from("System Diagnostics\n\n");
    for check in checks {
        let icon = match check.status {
            DiagnosticStatus::Pass => "✓",
            DiagnosticStatus::Warning => "⚠",
            DiagnosticStatus::Fail => "✗",
        };
        output.push_str(&format!("  {} {}: {}\n", icon, check.name, check.message));
        if let Some(ref hint) = check.fix_hint {
            output.push_str(&format!("    → {}\n", hint));
        }
    }
    let pass = checks
        .iter()
        .filter(|c| c.status == DiagnosticStatus::Pass)
        .count();
    let total = checks.len();
    output.push_str(&format!("\n  {}/{} checks passed\n", pass, total));
    output
}

// ═══════════════════════════════════════════════════════════════════════
//  Cost Visualization (/cost)
// ═══════════════════════════════════════════════════════════════════════

/// Time-series ring buffer for cost tracking.
pub struct CostTimeSeries {
    entries: Vec<CostEntry>,
    capacity: usize,
    write_pos: usize,
}

#[derive(Debug, Clone, Default)]
pub struct CostEntry {
    pub timestamp: u64,
    pub delta_cost: f64,
    pub provider: String,
    pub model: String,
}

impl CostTimeSeries {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            capacity,
            write_pos: 0,
        }
    }

    pub fn record(&mut self, entry: CostEntry) {
        if self.entries.len() < self.capacity {
            self.entries.push(entry);
        } else {
            self.entries[self.write_pos % self.capacity] = entry;
        }
        self.write_pos += 1;
    }

    /// Render a sparkline of recent costs. Uses Unicode block elements.
    pub fn sparkline(&self, width: usize) -> String {
        if self.entries.is_empty() {
            return String::new();
        }

        let blocks = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let values: Vec<f64> = self.entries.iter().map(|e| e.delta_cost).collect();
        let min = values.iter().cloned().fold(f64::MAX, f64::min);
        let max = values.iter().cloned().fold(f64::MIN, f64::max);
        let range = (max - min).max(f64::EPSILON);

        // Take last `width` values
        let start = values.len().saturating_sub(width);
        values[start..]
            .iter()
            .map(|v| {
                let idx = ((v - min) / range * 7.0).floor() as usize;
                blocks[idx.min(7)]
            })
            .collect()
    }

    /// Per-provider cost breakdown.
    pub fn by_provider(&self) -> HashMap<String, f64> {
        let mut map: HashMap<String, f64> = HashMap::new();
        for entry in &self.entries {
            *map.entry(entry.provider.clone()).or_default() += entry.delta_cost;
        }
        map
    }

    /// Total cost.
    pub fn total(&self) -> f64 {
        self.entries.iter().map(|e| e.delta_cost).sum()
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Context Visualization (/context)
// ═══════════════════════════════════════════════════════════════════════

/// Context budget visualization data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBudgetView {
    pub model_limit: u64,
    pub system_prompt_tokens: u64,
    pub pinned_tokens: u64,
    pub active_tokens: u64,
    pub historical_tokens: u64,
    pub exhaust_tokens: u64,
    pub output_reserve: u64,
    pub available: u64,
}

impl ContextBudgetView {
    /// Render as a stacked bar (text-mode).
    pub fn render_bar(&self, width: usize) -> String {
        let total = self.model_limit as f64;
        let sections = [
            ("SYS", self.system_prompt_tokens, "36"), // cyan
            ("PIN", self.pinned_tokens, "35"),        // magenta
            ("ACT", self.active_tokens, "32"),        // green
            ("HIS", self.historical_tokens, "33"),    // yellow
            ("EXH", self.exhaust_tokens, "90"),       // gray
            ("OUT", self.output_reserve, "34"),       // blue
        ];

        let mut bar = String::new();
        let mut chars_used = 0;
        for (label, tokens, color) in &sections {
            let section_width = ((*tokens as f64 / total) * width as f64).round() as usize;
            if section_width > 0 {
                let fill = label
                    .chars()
                    .chain(std::iter::repeat('░'))
                    .take(section_width)
                    .collect::<String>();
                bar.push_str(&format!("\x1b[{}m{}\x1b[0m", color, fill));
                chars_used += section_width;
            }
        }
        // Fill remaining with available space
        let remaining = width.saturating_sub(chars_used);
        if remaining > 0 {
            bar.push_str(&" ".repeat(remaining));
        }
        bar
    }

    /// Render as a summary table.
    pub fn render_summary(&self) -> String {
        let used = self.system_prompt_tokens
            + self.pinned_tokens
            + self.active_tokens
            + self.historical_tokens
            + self.exhaust_tokens
            + self.output_reserve;
        let pct = (used as f64 / self.model_limit as f64 * 100.0).round();
        format!(
            "Context Budget ({:.0}% used)\n\
             ├─ System:     {:>6} tokens\n\
             ├─ Pinned:     {:>6} tokens\n\
             ├─ Active:     {:>6} tokens\n\
             ├─ Historical: {:>6} tokens\n\
             ├─ Exhaust:    {:>6} tokens\n\
             ├─ Output:     {:>6} tokens (reserved)\n\
             └─ Available:  {:>6} tokens\n\
             Total: {}/{}\n",
            pct,
            self.system_prompt_tokens,
            self.pinned_tokens,
            self.active_tokens,
            self.historical_tokens,
            self.exhaust_tokens,
            self.output_reserve,
            self.available,
            used,
            self.model_limit,
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Theme System
// ═══════════════════════════════════════════════════════════════════════

/// Color scheme for TUI rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    pub agent_text: String,
    pub user_text: String,
    pub tool_name: String,
    pub error: String,
    pub warning: String,
    pub success: String,
    pub dim: String,
    pub accent: String,
    pub background: Option<String>,
}

impl Theme {
    pub fn default_dark() -> Self {
        Self {
            name: "dark".into(),
            agent_text: "37".into(), // white
            user_text: "36".into(),  // cyan
            tool_name: "33".into(),  // yellow
            error: "31".into(),      // red
            warning: "33".into(),    // yellow
            success: "32".into(),    // green
            dim: "90".into(),        // gray
            accent: "35".into(),     // magenta
            background: None,
        }
    }

    pub fn default_light() -> Self {
        Self {
            name: "light".into(),
            agent_text: "30".into(), // black
            user_text: "34".into(),  // blue
            tool_name: "33".into(),  // yellow
            error: "31".into(),      // red
            warning: "33".into(),    // yellow
            success: "32".into(),    // green
            dim: "37".into(),        // light gray
            accent: "35".into(),     // magenta
            background: None,
        }
    }

    /// Format text with the given style.
    pub fn style(&self, text: &str, color: &str) -> String {
        format!("\x1b[{}m{}\x1b[0m", color, text)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::default_dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn diagnostics_run() {
        let checks = run_diagnostics(&PathBuf::from("."));
        assert!(!checks.is_empty());
        let formatted = format_diagnostics(&checks);
        assert!(formatted.contains("System Diagnostics"));
    }

    #[test]
    fn cost_sparkline() {
        let mut ts = CostTimeSeries::new(100);
        for i in 0..20 {
            ts.record(CostEntry {
                timestamp: i,
                delta_cost: (i as f64) * 0.01,
                provider: "anthropic".into(),
                model: "claude".into(),
            });
        }
        let spark = ts.sparkline(20);
        assert!(!spark.is_empty());
        assert!(spark.len() <= 80); // unicode chars
    }

    #[test]
    fn context_budget_summary() {
        let view = ContextBudgetView {
            model_limit: 200_000,
            system_prompt_tokens: 5_000,
            pinned_tokens: 2_000,
            active_tokens: 50_000,
            historical_tokens: 30_000,
            exhaust_tokens: 10_000,
            output_reserve: 8_000,
            available: 95_000,
        };
        let summary = view.render_summary();
        assert!(summary.contains("Context Budget"));
        assert!(summary.contains("Pinned"));
        assert!(summary.contains("Active"));
    }

    #[test]
    fn theme_styling() {
        let theme = Theme::default_dark();
        let styled = theme.style("hello", &theme.success);
        assert!(styled.contains("\x1b[32m"));
        assert!(styled.contains("hello"));
    }
}
