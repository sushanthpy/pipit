//! Lazy MCP Tool Virtualization (Tool/Skill Task 2)
//!
//! Prevents tool-space explosion from degrading prompt quality when many
//! MCP servers are attached. Uses a two-stage index:
//! 1. Server → searchable tool metadata (name, description, category)
//! 2. Concrete tool schemas retrieved only after search/select
//!
//! Tools from servers with >LAZY_THRESHOLD tools are NOT eagerly registered.
//! Instead, a `mcp_search` meta-tool is registered that searches the index.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Threshold: servers with more tools than this use lazy loading.
pub const LAZY_THRESHOLD: usize = 20;

/// Lightweight tool metadata for the search index (no full schema).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolIndexEntry {
    /// Tool name.
    pub name: String,
    /// Server that provides this tool.
    pub server: String,
    /// Human-readable description.
    pub description: String,
    /// Semantic category hint (inferred from name/description).
    pub category: Option<String>,
    /// Relevance score (updated by search hits).
    pub hit_count: u32,
}

/// The lazy tool index: searchable metadata for all tools across all servers.
pub struct LazyToolIndex {
    /// All indexed tools.
    entries: Vec<ToolIndexEntry>,
    /// Name → index into entries.
    by_name: HashMap<String, usize>,
    /// Inverted index: word → indices of entries containing that word.
    inverted: HashMap<String, Vec<usize>>,
}

impl LazyToolIndex {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            by_name: HashMap::new(),
            inverted: HashMap::new(),
        }
    }

    /// Add tools from a server to the index.
    pub fn index_server(&mut self, server_name: &str, tools: &[(String, String)]) {
        for (name, description) in tools {
            let idx = self.entries.len();
            let entry = ToolIndexEntry {
                name: name.clone(),
                server: server_name.to_string(),
                description: description.clone(),
                category: infer_category(name, description),
                hit_count: 0,
            };
            self.by_name.insert(name.clone(), idx);

            // Build inverted index from name + description words
            let text = format!("{} {}", name, description).to_lowercase();
            for word in tokenize(&text) {
                self.inverted
                    .entry(word)
                    .or_default()
                    .push(idx);
            }

            self.entries.push(entry);
        }
    }

    /// Search for tools matching a query. Returns top-k results ranked by BM25.
    pub fn search(&mut self, query: &str, top_k: usize) -> Vec<&ToolIndexEntry> {
        let query_tokens = tokenize(&query.to_lowercase());
        let n = self.entries.len();
        if n == 0 {
            return vec![];
        }

        // BM25 scoring
        let k1: f64 = 1.2;
        let b: f64 = 0.75;
        let avg_doc_len: f64 = self
            .entries
            .iter()
            .map(|e| (e.name.len() + e.description.len()) as f64)
            .sum::<f64>()
            / n as f64;

        let mut scores: Vec<(usize, f64)> = Vec::new();

        for (idx, entry) in self.entries.iter().enumerate() {
            let doc_text = format!("{} {}", entry.name, entry.description).to_lowercase();
            let doc_len = doc_text.len() as f64;
            let doc_tokens = tokenize(&doc_text);
            let mut score = 0.0;

            for qt in &query_tokens {
                let tf = doc_tokens.iter().filter(|t| t == &qt).count() as f64;
                let df = self
                    .inverted
                    .get(qt.as_str())
                    .map(|v| v.len())
                    .unwrap_or(0) as f64;

                if df == 0.0 {
                    continue;
                }

                let idf = ((n as f64 - df + 0.5) / (df + 0.5) + 1.0).ln();
                let tf_norm = (tf * (k1 + 1.0)) / (tf + k1 * (1.0 - b + b * doc_len / avg_doc_len));
                score += idf * tf_norm;
            }

            // Boost by prior hit count
            score += (entry.hit_count as f64).ln_1p() * 0.1;

            if score > 0.0 {
                scores.push((idx, score));
            }
        }

        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let top_indices: Vec<usize> = scores
            .iter()
            .take(top_k)
            .map(|(idx, _)| *idx)
            .collect();

        // Update hit counts
        for &idx in &top_indices {
            self.entries[idx].hit_count += 1;
        }

        // Return references
        top_indices
            .iter()
            .map(|&idx| &self.entries[idx])
            .collect()
    }

    /// Look up a tool by exact name.
    pub fn get_by_name(&self, name: &str) -> Option<&ToolIndexEntry> {
        self.by_name.get(name).map(|&idx| &self.entries[idx])
    }

    /// Total number of indexed tools.
    pub fn total_tools(&self) -> usize {
        self.entries.len()
    }

    /// Number of servers indexed.
    pub fn server_count(&self) -> usize {
        let servers: std::collections::HashSet<&str> =
            self.entries.iter().map(|e| e.server.as_str()).collect();
        servers.len()
    }

    /// List all tools for a specific server.
    pub fn tools_for_server(&self, server: &str) -> Vec<&ToolIndexEntry> {
        self.entries
            .iter()
            .filter(|e| e.server == server)
            .collect()
    }
}

impl Default for LazyToolIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Simple whitespace + punctuation tokenizer.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() >= 2)
        .map(|w| w.to_string())
        .collect()
}

/// Infer a tool category from its name and description.
fn infer_category(name: &str, description: &str) -> Option<String> {
    let text = format!("{} {}", name, description).to_lowercase();
    if text.contains("read") || text.contains("get") || text.contains("list") || text.contains("search") {
        Some("read".to_string())
    } else if text.contains("write") || text.contains("create") || text.contains("update") || text.contains("edit") {
        Some("write".to_string())
    } else if text.contains("delete") || text.contains("remove") {
        Some("delete".to_string())
    } else if text.contains("run") || text.contains("execute") {
        Some("execute".to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_and_search() {
        let mut index = LazyToolIndex::new();
        index.index_server("github", &[
            ("create_issue".to_string(), "Create a new GitHub issue".to_string()),
            ("list_repos".to_string(), "List repositories for a user".to_string()),
            ("get_pull_request".to_string(), "Get details of a pull request".to_string()),
            ("create_pull_request".to_string(), "Create a new pull request".to_string()),
        ]);

        assert_eq!(index.total_tools(), 4);

        let results = index.search("pull request", 3);
        assert!(!results.is_empty());
        // Both PR-related tools should rank high
        let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"get_pull_request") || names.contains(&"create_pull_request"));
    }

    #[test]
    fn category_inference() {
        assert_eq!(infer_category("read_file", "Read a file"), Some("read".to_string()));
        assert_eq!(infer_category("create_issue", "Create issue"), Some("write".to_string()));
        assert_eq!(infer_category("delete_branch", "Delete a branch"), Some("delete".to_string()));
    }

    #[test]
    fn exact_lookup() {
        let mut index = LazyToolIndex::new();
        index.index_server("test", &[
            ("my_tool".to_string(), "Does something".to_string()),
        ]);
        assert!(index.get_by_name("my_tool").is_some());
        assert!(index.get_by_name("nonexistent").is_none());
    }
}
