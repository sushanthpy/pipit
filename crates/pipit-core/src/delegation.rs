//! Delegated Execution Proxy — Task 2.3
//!
//! A `DelegationTool` that appears in the LLM's tool list like any other tool
//! but transparently routes execution to a remote agent via the mesh.
//! Supports timeout, retry with exponential backoff, and schema validation.
//!
//! Cost-benefit: E[V_delegate] > E[V_local] + λ·L_d

use pipit_agent_mesh::{AgentCapability, AgentDescriptor, MeshRegistry};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

/// Result from a delegated execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationResult {
    pub agent_id: String,
    pub agent_name: String,
    pub success: bool,
    pub output: String,
    pub elapsed_ms: u64,
    pub retries: u32,
}

/// Configuration for delegation behavior.
#[derive(Debug, Clone)]
pub struct DelegationConfig {
    pub timeout: Duration,
    pub max_retries: u32,
    pub base_backoff: Duration,
    pub max_backoff: Duration,
    /// Minimum similarity score to consider an agent for delegation.
    pub min_capability_score: f64,
}

impl Default for DelegationConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(120),
            max_retries: 3,
            base_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            min_capability_score: 0.3,
        }
    }
}

/// A delegation request that the LLM can generate as a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationRequest {
    /// Natural language description of the sub-task.
    pub task: String,
    /// Required capabilities (tools, languages, tags).
    pub required_tools: Vec<String>,
    pub required_languages: Vec<String>,
    /// Optional: specific agent ID to delegate to.
    pub target_agent: Option<String>,
}

/// Proxy that resolves delegation requests against the mesh registry.
pub struct DelegationProxy {
    registry: Arc<MeshRegistry>,
    config: DelegationConfig,
}

impl DelegationProxy {
    pub fn new(registry: Arc<MeshRegistry>, config: DelegationConfig) -> Self {
        Self { registry, config }
    }

    /// Find the best agent for a delegation request.
    pub fn resolve_agent(&self, request: &DelegationRequest) -> Option<(AgentDescriptor, f64)> {
        // If a specific agent is requested, look it up directly
        if let Some(ref target) = request.target_agent {
            let agents = self.registry.list_agents();
            return agents
                .into_iter()
                .find(|a| a.id == *target)
                .map(|a| (a, 1.0));
        }

        // Otherwise, capability-based discovery
        let query = AgentCapability {
            required_tools: request.required_tools.iter().cloned().collect(),
            required_languages: request.required_languages.iter().cloned().collect(),
            required_tags: BTreeSet::new(),
        };

        let matches = self.registry.discover(&query);
        matches
            .into_iter()
            .find(|(_, score)| *score >= self.config.min_capability_score)
    }

    /// Compute the backoff duration for retry n.
    /// wait_time(n) = min(base * 2^n + jitter, max_wait)
    pub fn backoff_duration(&self, attempt: u32) -> Duration {
        let base_ms = self.config.base_backoff.as_millis() as u64;
        let exp = base_ms.saturating_mul(1u64 << attempt.min(10));
        // Simple jitter: add attempt * 100ms
        let jittered = exp.saturating_add(attempt as u64 * 100);
        let capped = jittered.min(self.config.max_backoff.as_millis() as u64);
        Duration::from_millis(capped)
    }

    /// Check if delegation is worthwhile given local vs remote expected value.
    /// E[V_delegate] > E[V_local] + λ * L_d
    pub fn should_delegate(
        expected_remote_value: f64,
        expected_local_value: f64,
        estimated_latency_ms: u64,
        time_cost_coefficient: f64,
    ) -> bool {
        let latency_cost = time_cost_coefficient * (estimated_latency_ms as f64 / 1000.0);
        expected_remote_value > expected_local_value + latency_cost
    }

    /// Create an isolated git worktree for a subagent.
    /// Returns the worktree handle for the agent to work in, and handles
    /// merge-back when the handle is dropped or explicitly merged.
    pub fn create_isolated_worktree(
        &self,
        project_root: &std::path::Path,
        agent_name: Option<&str>,
    ) -> Result<crate::worktree::WorktreeHandle, String> {
        let manager = crate::worktree::WorktreeManager::new(project_root)
            .map_err(|e| format!("Worktree setup failed: {}", e))?;
        manager
            .create(agent_name)
            .map_err(|e| format!("Worktree creation failed: {}", e))
    }
}

/// Build the tool declaration for the delegation tool.
pub fn delegation_tool_schema() -> serde_json::Value {
    serde_json::json!({
        "name": "delegate",
        "description": "Delegate a sub-task to a specialist agent. Use when another agent has better tools or expertise for a specific part of the task.",
        "input_schema": {
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "Description of the sub-task to delegate"
                },
                "required_tools": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Tools the target agent must have (e.g., 'pytest', 'cargo')"
                },
                "required_languages": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Languages the target agent must support"
                },
                "target_agent": {
                    "type": "string",
                    "description": "Optional specific agent ID to delegate to"
                }
            },
            "required": ["task"]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backoff_exponential() {
        let proxy = DelegationProxy {
            registry: Arc::new(MeshRegistry::new()),
            config: DelegationConfig::default(),
        };

        let d0 = proxy.backoff_duration(0);
        let d1 = proxy.backoff_duration(1);
        let d2 = proxy.backoff_duration(2);

        assert!(d1 > d0, "Backoff should increase: {:?} > {:?}", d1, d0);
        assert!(d2 > d1, "Backoff should increase: {:?} > {:?}", d2, d1);
        assert!(d2 <= proxy.config.max_backoff, "Should not exceed max");
    }

    #[test]
    fn test_should_delegate_calculation() {
        // Remote is clearly better
        assert!(DelegationProxy::should_delegate(0.9, 0.3, 2000, 0.01));
        // Remote is slightly better but latency too high
        assert!(!DelegationProxy::should_delegate(0.6, 0.5, 20000, 0.01));
        // Local is better
        assert!(!DelegationProxy::should_delegate(0.3, 0.8, 1000, 0.01));
    }
}
