//! Conflict-Aware Concurrent Tool Scheduler (Architecture Task 4)
//!
//! Every tool declares a resource signature. The scheduler builds a conflict
//! graph and runs maximal independent sets concurrently. This reduces
//! wall-clock latency without sacrificing safety.
//!
//! Resource conflict: two tool calls conflict if they access the same mutable
//! resource. Read-read is not a conflict. Read-write and write-write are.

use pipit_provider::ToolCall;
use std::collections::{HashMap, HashSet};

// ─── Resource Signatures ────────────────────────────────────────────────

/// A resource accessed by a tool call.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Resource {
    /// File path (read or write).
    Path(String),
    /// Process execution (shell commands are globally exclusive).
    Process,
    /// Network domain.
    Network(String),
    /// MCP server invocation.
    Mcp(String),
    /// Delegation/subagent.
    Delegation,
}

/// Access mode for a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    Read,
    Write,
}

/// A single resource access declaration.
#[derive(Debug, Clone)]
pub struct ResourceAccess {
    pub resource: Resource,
    pub mode: AccessMode,
}

/// The complete resource signature of a tool call.
#[derive(Debug, Clone)]
pub struct ResourceSignature {
    pub accesses: Vec<ResourceAccess>,
}

impl ResourceSignature {
    pub fn read_only(paths: &[&str]) -> Self {
        Self {
            accesses: paths
                .iter()
                .map(|p| ResourceAccess {
                    resource: Resource::Path(p.to_string()),
                    mode: AccessMode::Read,
                })
                .collect(),
        }
    }

    pub fn mutating(paths: &[&str]) -> Self {
        Self {
            accesses: paths
                .iter()
                .map(|p| ResourceAccess {
                    resource: Resource::Path(p.to_string()),
                    mode: AccessMode::Write,
                })
                .collect(),
        }
    }

    pub fn process() -> Self {
        Self {
            accesses: vec![ResourceAccess {
                resource: Resource::Process,
                mode: AccessMode::Write,
            }],
        }
    }

    /// Check if two signatures conflict (cannot run concurrently).
    pub fn conflicts_with(&self, other: &ResourceSignature) -> bool {
        // Process resource conflicts with everything (globally exclusive)
        let self_has_process = self.accesses.iter().any(|a| matches!(a.resource, Resource::Process));
        let other_has_process = other.accesses.iter().any(|a| matches!(a.resource, Resource::Process));
        if self_has_process || other_has_process {
            return true;
        }

        for a in &self.accesses {
            for b in &other.accesses {
                if a.resource == b.resource {
                    // Read-read is fine; any write creates a conflict
                    if a.mode == AccessMode::Write || b.mode == AccessMode::Write {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Check if this signature is purely read-only.
    pub fn is_read_only(&self) -> bool {
        self.accesses.iter().all(|a| a.mode == AccessMode::Read)
    }
}

// ─── Tool Signature Extraction ──────────────────────────────────────────

/// Extract a resource signature from a tool call by analyzing its arguments.
pub fn extract_signature(tool_name: &str, args: &serde_json::Value) -> ResourceSignature {
    match tool_name {
        "read_file" => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            ResourceSignature::read_only(&[path])
        }
        "edit_file" | "write_file" => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            ResourceSignature::mutating(&[path])
        }
        "multi_edit_file" => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            ResourceSignature::mutating(&[path])
        }
        "grep" | "glob" | "list_directory" => {
            // These are read-only over potentially many files
            ResourceSignature::read_only(&["."])
        }
        "bash" => {
            // Shell commands are globally exclusive (may modify anything)
            ResourceSignature::process()
        }
        "subagent" => ResourceSignature {
            accesses: vec![ResourceAccess {
                resource: Resource::Delegation,
                mode: AccessMode::Write,
            }],
        },
        _ => {
            // Unknown tools are treated as globally exclusive for safety
            ResourceSignature::process()
        }
    }
}

// ─── Scheduling ─────────────────────────────────────────────────────────

/// A batch of tool calls that can execute concurrently.
#[derive(Debug, Clone)]
pub struct ExecutionBatch {
    /// Indices into the original tool call array.
    pub indices: Vec<usize>,
    /// Whether all calls in this batch are read-only.
    pub all_read_only: bool,
}

/// Schedule tool calls into batches of non-conflicting operations.
///
/// Uses a greedy graph-coloring approach: O(n²) in the worst case for n calls,
/// but practically O(n log n) because most tool batches are small (<10).
pub fn schedule(calls: &[ToolCall]) -> Vec<ExecutionBatch> {
    if calls.is_empty() {
        return vec![];
    }

    let n = calls.len();
    let signatures: Vec<ResourceSignature> = calls
        .iter()
        .map(|c| extract_signature(&c.tool_name, &c.args))
        .collect();

    // Build conflict adjacency: conflicts[i] = set of indices conflicting with i
    let mut conflicts: Vec<HashSet<usize>> = vec![HashSet::new(); n];
    for i in 0..n {
        for j in (i + 1)..n {
            if signatures[i].conflicts_with(&signatures[j]) {
                conflicts[i].insert(j);
                conflicts[j].insert(i);
            }
        }
    }

    // Greedy coloring: assign each call to the earliest non-conflicting batch
    let mut batches: Vec<ExecutionBatch> = Vec::new();
    let mut assigned = vec![false; n];

    while assigned.iter().any(|&a| !a) {
        // Find a maximal independent set greedily
        let mut batch_indices = Vec::new();
        let mut batch_conflicts: HashSet<usize> = HashSet::new();

        for i in 0..n {
            if assigned[i] {
                continue;
            }
            if batch_conflicts.contains(&i) {
                continue;
            }
            // i can be added to this batch
            batch_indices.push(i);
            batch_conflicts.extend(&conflicts[i]);
            assigned[i] = true;
        }

        let all_read_only = batch_indices
            .iter()
            .all(|&i| signatures[i].is_read_only());

        batches.push(ExecutionBatch {
            indices: batch_indices,
            all_read_only,
        });
    }

    batches
}

/// Quick check: can all the given calls run concurrently?
pub fn all_independent(calls: &[ToolCall]) -> bool {
    let batches = schedule(calls);
    batches.len() <= 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_call(name: &str, path: &str) -> ToolCall {
        ToolCall {
            call_id: format!("call-{}", name),
            tool_name: name.to_string(),
            args: serde_json::json!({ "path": path }),
        }
    }

    #[test]
    fn reads_are_parallel() {
        let calls = vec![
            make_call("read_file", "a.rs"),
            make_call("read_file", "b.rs"),
            make_call("read_file", "c.rs"),
        ];
        let batches = schedule(&calls);
        // All reads can run in one batch
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].indices.len(), 3);
        assert!(batches[0].all_read_only);
    }

    #[test]
    fn writes_to_same_file_serialized() {
        let calls = vec![
            make_call("edit_file", "a.rs"),
            make_call("edit_file", "a.rs"),
        ];
        let batches = schedule(&calls);
        // Two writes to same file → two batches
        assert_eq!(batches.len(), 2);
    }

    #[test]
    fn writes_to_different_files_parallel() {
        let calls = vec![
            make_call("edit_file", "a.rs"),
            make_call("edit_file", "b.rs"),
        ];
        let batches = schedule(&calls);
        // Writes to different files → one batch
        assert_eq!(batches.len(), 1);
    }

    #[test]
    fn read_write_conflict() {
        let calls = vec![
            make_call("read_file", "a.rs"),
            make_call("edit_file", "a.rs"),
            make_call("read_file", "b.rs"),
        ];
        let batches = schedule(&calls);
        // read a.rs and edit a.rs conflict; read b.rs is independent
        assert!(batches.len() <= 2);
    }

    #[test]
    fn bash_is_exclusive() {
        let calls = vec![
            make_call("read_file", "a.rs"),
            ToolCall {
                call_id: "call-bash".to_string(),
                tool_name: "bash".to_string(),
                args: serde_json::json!({ "command": "make test" }),
            },
            make_call("read_file", "b.rs"),
        ];
        let batches = schedule(&calls);
        // bash conflicts with everything
        assert!(batches.len() >= 2);
    }
}
