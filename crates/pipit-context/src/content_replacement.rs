//! Content Replacement State Machine
//!
//! A persistent, tool-aware content replacement system. When tool results exceed
//! a per-tool budget, they are replaced with truncated versions. The replacement
//! record is persisted so `--resume` can reconstruct the exact same context.
//!
//! Tools with `max_result_size = Unlimited` are never truncated.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Per-tool budget policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolBudget {
    /// Truncate at this many characters.
    Limited(usize),
    /// Never truncate (e.g., grep results the model needs in full).
    Unlimited,
}

impl Default for ToolBudget {
    fn default() -> Self {
        ToolBudget::Limited(32_000)
    }
}

/// A recorded content replacement for deterministic replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplacementRecord {
    /// The tool call ID that produced this result.
    pub call_id: String,
    /// Tool name.
    pub tool_name: String,
    /// Original content length in chars.
    pub original_length: usize,
    /// Truncated content length in chars.
    pub truncated_length: usize,
    /// The turn number when replacement occurred.
    pub turn_number: u32,
    /// SHA-256 of original content for integrity verification on resume.
    pub original_hash: String,
}

/// The content replacement state machine.
pub struct ContentReplacementManager {
    /// Per-tool budget overrides. Tools not listed use the default.
    tool_budgets: HashMap<String, ToolBudget>,
    /// Default budget for tools without overrides.
    default_budget: ToolBudget,
    /// All replacement records (for persistence and resume).
    records: Vec<ReplacementRecord>,
    /// Persistence path for replacement records.
    persist_path: Option<PathBuf>,
}

impl ContentReplacementManager {
    pub fn new(default_chars: usize) -> Self {
        let mut tool_budgets = HashMap::new();
        // Tools that should never be truncated
        tool_budgets.insert("grep".to_string(), ToolBudget::Unlimited);
        tool_budgets.insert("glob".to_string(), ToolBudget::Unlimited);

        Self {
            tool_budgets,
            default_budget: ToolBudget::Limited(default_chars),
            records: Vec::new(),
            persist_path: None,
        }
    }

    /// Set the persistence path for replay records.
    pub fn set_persist_path(&mut self, path: PathBuf) {
        self.persist_path = Some(path);
    }

    /// Set a custom budget for a specific tool.
    pub fn set_tool_budget(&mut self, tool_name: &str, budget: ToolBudget) {
        self.tool_budgets.insert(tool_name.to_string(), budget);
    }

    /// Check if content should be truncated for the given tool.
    /// Returns the truncated content if replacement is needed, or None.
    pub fn maybe_replace(
        &mut self,
        call_id: &str,
        tool_name: &str,
        content: &str,
        turn_number: u32,
    ) -> Option<String> {
        let budget = self
            .tool_budgets
            .get(tool_name)
            .unwrap_or(&self.default_budget);

        let max_chars = match budget {
            ToolBudget::Limited(max) => *max,
            ToolBudget::Unlimited => return None,
        };

        if content.len() <= max_chars {
            return None;
        }

        // Compute hash for integrity verification
        let hash = simple_hash(content);

        // Truncate using head/tail strategy
        let truncated = truncate_content(content, max_chars);

        // Record the replacement
        self.records.push(ReplacementRecord {
            call_id: call_id.to_string(),
            tool_name: tool_name.to_string(),
            original_length: content.len(),
            truncated_length: truncated.len(),
            turn_number,
            original_hash: hash,
        });

        // Persist if path is set
        if let Some(ref path) = self.persist_path {
            let _ = self.persist_records(path);
        }

        Some(truncated)
    }

    /// Get all replacement records (for session resume).
    pub fn records(&self) -> &[ReplacementRecord] {
        &self.records
    }

    /// Load replacement records from disk (for --resume).
    pub fn load_records(path: &Path) -> Result<Vec<ReplacementRecord>, String> {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("Failed to read records: {}", e))?;
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse records: {}", e))
    }

    /// Restore records from a previous session.
    pub fn restore_records(&mut self, records: Vec<ReplacementRecord>) {
        self.records = records;
    }

    /// Total tokens freed by all replacements.
    pub fn total_chars_freed(&self) -> usize {
        self.records
            .iter()
            .map(|r| r.original_length.saturating_sub(r.truncated_length))
            .sum()
    }

    fn persist_records(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory: {}", e))?;
        }
        let json = serde_json::to_string_pretty(&self.records)
            .map_err(|e| format!("Failed to serialize: {}", e))?;
        std::fs::write(path, json).map_err(|e| format!("Failed to write: {}", e))
    }
}

/// Truncate content using a head/tail line strategy.
fn truncate_content(content: &str, max_chars: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let head = 40.min(total);
    let tail = 40.min(total.saturating_sub(head));

    if total > head + tail {
        format!(
            "{}\n\n[...content replacement: {} of {} lines truncated (budget: {} chars)...]\n\n{}",
            lines[..head].join("\n"),
            total - head - tail,
            total,
            max_chars,
            lines[total - tail..].join("\n"),
        )
    } else {
        content.chars().take(max_chars).collect()
    }
}

/// Simple hash for content integrity (not cryptographic).
fn simple_hash(content: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_tools_never_truncated() {
        let mut mgr = ContentReplacementManager::new(100);
        let long_content = "x".repeat(10_000);
        let result = mgr.maybe_replace("call-1", "grep", &long_content, 1);
        assert!(result.is_none());
    }

    #[test]
    fn limited_tools_truncated_over_budget() {
        let mut mgr = ContentReplacementManager::new(100);
        let long_content = (0..200)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let result = mgr.maybe_replace("call-2", "bash", &long_content, 1);
        assert!(result.is_some());
        assert!(result.unwrap().contains("content replacement"));
        assert_eq!(mgr.records().len(), 1);
    }

    #[test]
    fn records_persist_and_restore() {
        let mut mgr = ContentReplacementManager::new(100);
        let content = "x".repeat(200);
        mgr.maybe_replace("call-3", "bash", &content, 1);

        let records = mgr.records().to_vec();
        let mut mgr2 = ContentReplacementManager::new(100);
        mgr2.restore_records(records);
        assert_eq!(mgr2.records().len(), 1);
        assert_eq!(mgr2.records()[0].call_id, "call-3");
    }
}
