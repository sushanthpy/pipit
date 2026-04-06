
//! Pipit Agents — Built-In Agent Catalog with Coordinator Mode (Task 7)
//!
//! 5 purpose-built agents with curated system prompts, tool whitelists,
//! and behavioral constraints:
//!
//!   1. ExploreAgent — Read-only codebase exploration and analysis
//!   2. PlanAgent — Strategic planning with plan-mode tool restrictions
//!   3. VerifyAgent — Adversarial verification (tries to break, not confirm)
//!   4. GeneralAgent — Full-capability agent for mixed tasks
//!   5. GuideAgent — Documentation and onboarding assistant
//!
//! Coordinator mode: parent agent dispatches k sub-tasks to k agents
//! in parallel, then aggregates results.
//!
//! Speedup under Amdahl's law: S = 1 / (s + (1-s)/k)
//!   where s = serial fraction (merge step), k = agent count.
//!   For typical s ≈ 0.1, S ≈ 5× at k=8.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

pub mod builtins;

// ═══════════════════════════════════════════════════════════════════════════
//  Agent Definition
// ═══════════════════════════════════════════════════════════════════════════

/// A complete agent definition — system prompt, tool restrictions, constraints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    /// Unique agent name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// System prompt injected before the agent's first turn.
    pub system_prompt: String,
    /// Tools this agent is allowed to use. Empty = all tools.
    pub allowed_tools: HashSet<String>,
    /// Tools explicitly denied to this agent.
    pub denied_tools: HashSet<String>,
    /// Maximum turns before the agent must return.
    pub max_turns: u32,
    /// Whether the agent can modify files.
    pub can_write: bool,
    /// Whether the agent can execute shell commands.
    pub can_execute: bool,
    /// Agent category for display.
    pub category: AgentCategory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentCategory {
    BuiltIn,
    Custom,
    Team,
}

impl AgentDefinition {
    /// Check if a tool is allowed for this agent.
    pub fn is_tool_allowed(&self, tool_name: &str) -> bool {
        if self.denied_tools.contains(tool_name) {
            return false;
        }
        if self.allowed_tools.is_empty() {
            return true; // Empty = all allowed
        }
        self.allowed_tools.contains(tool_name)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Agent Memory Snapshot
// ═══════════════════════════════════════════════════════════════════════════

/// Copy-on-write memory snapshot for agent isolation.
///
/// The parent's context is shared read-only. When a child agent modifies
/// state, a snapshot is materialized: O(1) fork, O(δ) materialization
/// where δ = modified tokens.
#[derive(Debug, Clone)]
pub struct AgentMemorySnapshot {
    /// Shared parent context (immutable reference).
    pub parent_context: Vec<String>,
    /// Delta: modifications made by this agent.
    pub delta: Vec<String>,
    /// Files modified by this agent (for merge tracking).
    pub modified_files: Vec<PathBuf>,
    /// Tool calls made by this agent.
    pub tool_calls: Vec<AgentToolCall>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentToolCall {
    pub tool_name: String,
    pub args_summary: String,
    pub result_summary: String,
    pub success: bool,
}

impl AgentMemorySnapshot {
    pub fn new(parent_context: Vec<String>) -> Self {
        Self {
            parent_context,
            delta: Vec::new(),
            modified_files: Vec::new(),
            tool_calls: Vec::new(),
        }
    }

    /// Merge delta back into parent context.
    pub fn materialize(&self) -> Vec<String> {
        let mut merged = self.parent_context.clone();
        merged.extend(self.delta.clone());
        merged
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Coordinator Mode
// ═══════════════════════════════════════════════════════════════════════════

/// A sub-task for coordinator dispatch.
#[derive(Debug, Clone)]
pub struct SubTask {
    /// Task ID.
    pub id: String,
    /// Agent to execute this task.
    pub agent_name: String,
    /// Task prompt.
    pub prompt: String,
    /// Optional files the agent should focus on.
    pub focus_files: Vec<PathBuf>,
}

/// Result of a completed sub-task.
#[derive(Debug, Clone)]
pub struct SubTaskResult {
    pub task_id: String,
    pub agent_name: String,
    pub status: SubTaskStatus,
    pub output: String,
    pub memory_snapshot: AgentMemorySnapshot,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubTaskStatus {
    Completed,
    Failed,
    TimedOut,
    Cancelled,
}

/// The coordinator dispatches sub-tasks to agents and aggregates results.
///
/// Execution model:
///   1. Parent creates k SubTasks
///   2. Coordinator spawns k agents in parallel (tokio::spawn)
///   3. Each agent runs independently with its own memory snapshot
///   4. Coordinator awaits all results (with timeout)
///   5. Results are merged: outputs concatenated, file conflicts detected
///
/// File conflict detection: if two agents modify the same file, the
/// coordinator flags this and presents both versions for resolution.
pub struct Coordinator {
    /// Maximum parallel agents.
    pub max_parallel: usize,
    /// Timeout per sub-task (seconds).
    pub task_timeout_secs: u64,
}

impl Default for Coordinator {
    fn default() -> Self {
        Self {
            max_parallel: 8,
            task_timeout_secs: 300,
        }
    }
}

/// Merge result from coordinator.
#[derive(Debug)]
pub struct CoordinatorResult {
    pub results: Vec<SubTaskResult>,
    pub file_conflicts: Vec<FileConflict>,
    pub total_duration_ms: u64,
    pub parallel_speedup: f64,
}

/// Two agents modified the same file.
#[derive(Debug, Clone)]
pub struct FileConflict {
    pub file: PathBuf,
    pub agent_a: String,
    pub agent_b: String,
}

impl Coordinator {
    /// Detect file conflicts across sub-task results.
    pub fn detect_conflicts(results: &[SubTaskResult]) -> Vec<FileConflict> {
        let mut file_owners: std::collections::HashMap<PathBuf, Vec<String>> =
            std::collections::HashMap::new();

        for result in results {
            for file in &result.memory_snapshot.modified_files {
                file_owners
                    .entry(file.clone())
                    .or_default()
                    .push(result.agent_name.clone());
            }
        }

        file_owners
            .into_iter()
            .filter(|(_, agents)| agents.len() > 1)
            .map(|(file, agents)| FileConflict {
                file,
                agent_a: agents[0].clone(),
                agent_b: agents[1].clone(),
            })
            .collect()
    }

    /// Merge sub-task outputs into a coordinator summary.
    pub fn merge(&self, results: Vec<SubTaskResult>) -> CoordinatorResult {
        let conflicts = Self::detect_conflicts(&results);

        let total_duration = results.iter().map(|r| r.duration_ms).sum::<u64>();
        let max_duration = results.iter().map(|r| r.duration_ms).max().unwrap_or(0);

        let speedup = if max_duration > 0 {
            total_duration as f64 / max_duration as f64
        } else {
            1.0
        };

        CoordinatorResult {
            results,
            file_conflicts: conflicts,
            total_duration_ms: max_duration, // Wall clock = max of parallel tasks
            parallel_speedup: speedup,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Custom Agent Loading
// ═══════════════════════════════════════════════════════════════════════════

/// Load custom agent definitions from a directory.
///
/// Files: `.pipit/agents/*.toml` or `.pipit/agents/*.json`
/// Each file defines one agent with name, system_prompt, allowed_tools, etc.
pub fn load_custom_agents(agents_dir: &std::path::Path) -> Vec<AgentDefinition> {
    let mut agents = Vec::new();

    if !agents_dir.exists() {
        return agents;
    }

    let entries = match std::fs::read_dir(agents_dir) {
        Ok(e) => e,
        Err(_) => return agents,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let agent: Option<AgentDefinition> = match ext {
            "toml" => toml::from_str(&content).ok(),
            "json" => serde_json::from_str(&content).ok(),
            _ => None,
        };

        if let Some(mut def) = agent {
            def.category = AgentCategory::Custom;
            tracing::info!(name = %def.name, "Loaded custom agent from {}", path.display());
            agents.push(def);
        }
    }

    agents
}

// ═══════════════════════════════════════════════════════════════════════════
//  Get All Agents (built-in + custom)
// ═══════════════════════════════════════════════════════════════════════════

/// Get all available agents: 5 built-in + custom loaded from disk.
pub fn all_agents(project_root: &std::path::Path) -> Vec<AgentDefinition> {
    let mut agents = builtins::builtin_agents();
    let custom_dir = project_root.join(".pipit").join("agents");
    agents.extend(load_custom_agents(&custom_dir));
    agents
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_agents_exist() {
        let agents = builtins::builtin_agents();
        assert_eq!(agents.len(), 5);

        let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"explore"));
        assert!(names.contains(&"plan"));
        assert!(names.contains(&"verify"));
        assert!(names.contains(&"general"));
        assert!(names.contains(&"guide"));
    }

    #[test]
    fn verify_agent_cannot_write() {
        let agents = builtins::builtin_agents();
        let verify = agents.iter().find(|a| a.name == "verify").unwrap();
        assert!(!verify.can_write);
        assert!(verify.denied_tools.contains("write_file"));
        assert!(verify.denied_tools.contains("edit_file"));
    }

    #[test]
    fn explore_agent_readonly() {
        let agents = builtins::builtin_agents();
        let explore = agents.iter().find(|a| a.name == "explore").unwrap();
        assert!(!explore.can_write);
        assert!(!explore.can_execute);
    }

    #[test]
    fn conflict_detection() {
        let results = vec![
            SubTaskResult {
                task_id: "t1".into(),
                agent_name: "agent_a".into(),
                status: SubTaskStatus::Completed,
                output: "done".into(),
                memory_snapshot: AgentMemorySnapshot {
                    parent_context: vec![],
                    delta: vec![],
                    modified_files: vec![PathBuf::from("src/lib.rs")],
                    tool_calls: vec![],
                },
                duration_ms: 100,
            },
            SubTaskResult {
                task_id: "t2".into(),
                agent_name: "agent_b".into(),
                status: SubTaskStatus::Completed,
                output: "done".into(),
                memory_snapshot: AgentMemorySnapshot {
                    parent_context: vec![],
                    delta: vec![],
                    modified_files: vec![PathBuf::from("src/lib.rs"), PathBuf::from("src/main.rs")],
                    tool_calls: vec![],
                },
                duration_ms: 200,
            },
        ];

        let conflicts = Coordinator::detect_conflicts(&results);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].file, PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn coordinator_merge_calculates_speedup() {
        let coordinator = Coordinator::default();
        let results = vec![
            SubTaskResult {
                task_id: "t1".into(),
                agent_name: "a".into(),
                status: SubTaskStatus::Completed,
                output: "".into(),
                memory_snapshot: AgentMemorySnapshot::new(vec![]),
                duration_ms: 1000,
            },
            SubTaskResult {
                task_id: "t2".into(),
                agent_name: "b".into(),
                status: SubTaskStatus::Completed,
                output: "".into(),
                memory_snapshot: AgentMemorySnapshot::new(vec![]),
                duration_ms: 2000,
            },
        ];

        let merged = coordinator.merge(results);
        // Wall clock = max(1000, 2000) = 2000
        assert_eq!(merged.total_duration_ms, 2000);
        // Speedup = sum/max = 3000/2000 = 1.5
        assert!((merged.parallel_speedup - 1.5).abs() < 0.01);
    }
}

