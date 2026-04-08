//! Data Flow Graph Builder — shared by adversarial + compliance modules.
//!
//! Builds a data flow graph from source code: tracks how data moves
//! from inputs (user data, HTTP requests) through transformations
//! to outputs (database writes, file system, responses).
//!
//! Forward dataflow: taint_out(n) = gen(n) ∪ (taint_in(n) \ kill(n))
//! Fixpoint: O(V·E) on the data flow graph.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// A node in the data flow graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataFlowNode {
    pub id: usize,
    pub file: String,
    pub line: usize,
    pub kind: NodeKind,
    pub label: String,
    pub tainted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    Source,    // User input, HTTP request, env var
    Transform, // Assignment, function call, data manipulation
    Sink,      // Database write, file write, HTTP response, log
    Sanitizer, // Input validation, encoding, escaping
}

/// An edge in the data flow graph (data moves from → to).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataFlowEdge {
    pub from: usize,
    pub to: usize,
    pub label: String,
}

/// Complete data flow graph for analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataFlowGraph {
    pub nodes: Vec<DataFlowNode>,
    pub edges: Vec<DataFlowEdge>,
}

/// Source patterns — locations where untrusted data enters.
const SOURCE_PATTERNS: &[(&str, &str)] = &[
    ("request.json", "HTTP JSON body"),
    ("request.form", "HTTP form data"),
    ("request.args", "HTTP query params"),
    ("request.data", "HTTP raw body"),
    ("request.headers", "HTTP headers"),
    ("request.cookies", "HTTP cookies"),
    ("req.body", "Express.js body"),
    ("req.params", "Express.js params"),
    ("req.query", "Express.js query"),
    ("input(", "User stdin input"),
    ("sys.argv", "CLI arguments"),
    ("os.environ", "Environment variable"),
    ("env::var", "Rust env var"),
    ("process.env", "Node.js env"),
    ("getenv(", "C getenv"),
];

/// Sink patterns — locations where data leaves the system.
const SINK_PATTERNS: &[(&str, &str)] = &[
    ("cursor.execute", "SQL query"),
    ("db.execute", "Database query"),
    ("db.query", "Database query"),
    (".save(", "ORM save"),
    (".insert(", "Database insert"),
    (".update(", "Database update"),
    ("open(", "File operation"),
    ("write(", "File write"),
    ("print(", "Console output"),
    ("logging.", "Log output"),
    ("logger.", "Log output"),
    ("console.log", "Console log"),
    ("Response(", "HTTP response"),
    ("jsonify(", "Flask JSON response"),
    ("res.json(", "Express JSON response"),
    ("res.send(", "Express response"),
    ("send_email", "Email send"),
    ("subprocess", "Process spawn"),
    ("Command::new", "Process spawn"),
    ("exec(", "Code execution"),
    ("eval(", "Code evaluation"),
    ("cache.set", "Cache write"),
    ("redis.set", "Redis write"),
];

/// Sanitizer patterns — locations that neutralize taint.
const SANITIZER_PATTERNS: &[(&str, &str)] = &[
    ("escape(", "HTML escape"),
    ("sanitize(", "Input sanitizer"),
    ("validate(", "Input validation"),
    ("parameterize", "SQL parameterization"),
    ("bleach.clean", "HTML sanitizer"),
    ("html.escape", "HTML escape"),
    ("shlex.quote", "Shell escape"),
    ("sqlalchemy.text", "SQL parameterization"),
];

impl DataFlowGraph {
    /// Build a data flow graph from source code.
    pub fn from_source(file_path: &str, source: &str) -> Self {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut id = 0;

        let mut in_block_comment = false;
        let mut last_source_id: Option<usize> = None;

        for (line_num, line) in source.lines().enumerate() {
            let trimmed = line.trim();

            // Track block comments
            if trimmed.contains("/*") && !trimmed.contains("*/") {
                in_block_comment = true;
                continue;
            }
            if trimmed.contains("*/") {
                in_block_comment = false;
                continue;
            }
            if in_block_comment || trimmed.starts_with("//") || trimmed.starts_with('#') {
                continue;
            }

            // Detect sources
            for (pattern, label) in SOURCE_PATTERNS {
                if trimmed.contains(pattern) {
                    nodes.push(DataFlowNode {
                        id,
                        file: file_path.into(),
                        line: line_num + 1,
                        kind: NodeKind::Source,
                        label: label.to_string(),
                        tainted: true,
                    });
                    last_source_id = Some(id);
                    id += 1;
                }
            }

            // Detect sanitizers
            for (pattern, label) in SANITIZER_PATTERNS {
                if trimmed.contains(pattern) {
                    nodes.push(DataFlowNode {
                        id,
                        file: file_path.into(),
                        line: line_num + 1,
                        kind: NodeKind::Sanitizer,
                        label: label.to_string(),
                        tainted: false,
                    });
                    // Edge from last source to sanitizer
                    if let Some(src_id) = last_source_id {
                        edges.push(DataFlowEdge {
                            from: src_id,
                            to: id,
                            label: "sanitizes".into(),
                        });
                    }
                    last_source_id = Some(id); // Sanitized data flows onward
                    id += 1;
                }
            }

            // Detect sinks
            for (pattern, label) in SINK_PATTERNS {
                if trimmed.contains(pattern) {
                    let sink_id = id;
                    nodes.push(DataFlowNode {
                        id: sink_id,
                        file: file_path.into(),
                        line: line_num + 1,
                        kind: NodeKind::Sink,
                        label: label.to_string(),
                        tainted: false,
                    });
                    // Edge from nearest source/transform to this sink
                    if let Some(src_id) = last_source_id {
                        edges.push(DataFlowEdge {
                            from: src_id,
                            to: sink_id,
                            label: "data flows to".into(),
                        });
                    }
                    id += 1;
                }
            }
        }

        DataFlowGraph { nodes, edges }
    }

    /// Run taint propagation. Marks sinks reachable from tainted sources.
    /// Returns tainted sink nodes (potential vulnerabilities).
    pub fn propagate_taint(&mut self) -> Vec<&DataFlowNode> {
        // Build adjacency list
        let n = self.nodes.len();
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
        for edge in &self.edges {
            if edge.from < n && edge.to < n {
                adj[edge.from].push(edge.to);
            }
        }

        // BFS from all tainted sources
        let mut queue: Vec<usize> = self
            .nodes
            .iter()
            .filter(|node| node.tainted)
            .map(|node| node.id)
            .collect();

        let mut visited = HashSet::new();
        while let Some(node_id) = queue.pop() {
            if !visited.insert(node_id) {
                continue;
            }
            if node_id >= n {
                continue;
            }

            // Sanitizers kill taint
            if self.nodes[node_id].kind == NodeKind::Sanitizer {
                continue; // Don't propagate past sanitizers
            }

            // Mark reachable nodes as tainted
            for &next in &adj[node_id] {
                if next < n && !visited.contains(&next) {
                    self.nodes[next].tainted = true;
                    queue.push(next);
                }
            }
        }

        // Return tainted sinks
        self.nodes
            .iter()
            .filter(|n| n.tainted && n.kind == NodeKind::Sink)
            .collect()
    }

    /// Get all source → sink paths (for reporting).
    pub fn tainted_paths(&self) -> Vec<(String, String, usize)> {
        let mut paths = Vec::new();
        for edge in &self.edges {
            if edge.from < self.nodes.len() && edge.to < self.nodes.len() {
                let from = &self.nodes[edge.from];
                let to = &self.nodes[edge.to];
                if from.kind == NodeKind::Source && to.kind == NodeKind::Sink {
                    paths.push((
                        format!("{}:{} ({})", from.file, from.line, from.label),
                        format!("{}:{} ({})", to.file, to.line, to.label),
                        to.line - from.line,
                    ));
                }
            }
        }
        paths
    }

    /// Summary statistics.
    pub fn summary(&self) -> DataFlowSummary {
        DataFlowSummary {
            total_nodes: self.nodes.len(),
            sources: self
                .nodes
                .iter()
                .filter(|n| n.kind == NodeKind::Source)
                .count(),
            sinks: self
                .nodes
                .iter()
                .filter(|n| n.kind == NodeKind::Sink)
                .count(),
            sanitizers: self
                .nodes
                .iter()
                .filter(|n| n.kind == NodeKind::Sanitizer)
                .count(),
            tainted_sinks: self
                .nodes
                .iter()
                .filter(|n| n.tainted && n.kind == NodeKind::Sink)
                .count(),
            edges: self.edges.len(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataFlowSummary {
    pub total_nodes: usize,
    pub sources: usize,
    pub sinks: usize,
    pub sanitizers: usize,
    pub tainted_sinks: usize,
    pub edges: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_data_flow() {
        let code = r#"
from flask import request
data = request.json
cursor.execute(f"INSERT INTO users VALUES ({data})")
"#;
        let graph = DataFlowGraph::from_source("app.py", code);
        assert!(
            graph.nodes.iter().any(|n| n.kind == NodeKind::Source),
            "Should detect source"
        );
        assert!(
            graph.nodes.iter().any(|n| n.kind == NodeKind::Sink),
            "Should detect sink"
        );
        assert!(!graph.edges.is_empty(), "Should connect source to sink");
    }

    #[test]
    fn test_sanitizer_kills_taint() {
        let code = r#"
user_input = request.form["name"]
clean = html.escape(user_input)
print(clean)
"#;
        let mut graph = DataFlowGraph::from_source("app.py", code);
        let tainted = graph.propagate_taint();
        // print() sink should NOT be tainted because html.escape() sanitizes
        let tainted_prints: Vec<_> = tainted
            .iter()
            .filter(|n| n.label.contains("Console"))
            .collect();
        assert!(
            tainted_prints.is_empty(),
            "Sanitized output should not be tainted"
        );
    }

    #[test]
    fn test_unsanitized_sql_is_tainted() {
        let code = r#"
data = request.json
cursor.execute(f"SELECT * FROM users WHERE id={data['id']}")
"#;
        let mut graph = DataFlowGraph::from_source("app.py", code);
        let tainted = graph.propagate_taint();
        assert!(!tainted.is_empty(), "Unsanitized SQL should be tainted");
        assert!(tainted.iter().any(|n| n.label.contains("SQL")));
    }

    #[test]
    fn test_ignores_comments() {
        let code = r#"
# cursor.execute("this is a comment")
// request.json is also a comment
"#;
        let graph = DataFlowGraph::from_source("app.py", code);
        assert!(graph.nodes.is_empty(), "Should ignore comments");
    }

    #[test]
    fn test_summary() {
        let code = "data = request.form['x']\ncursor.execute(data)\nprint(data)";
        let graph = DataFlowGraph::from_source("a.py", code);
        let summary = graph.summary();
        assert!(summary.sources >= 1);
        assert!(summary.sinks >= 1);
    }
}
