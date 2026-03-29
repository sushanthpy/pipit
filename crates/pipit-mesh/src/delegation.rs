//! Mesh task delegation engine.
//!
//! Routes tasks to the best-equipped node based on capability matching,
//! current load, and affinity rules.

use crate::node::{NodeDescriptor, NodeRegistry};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A task to be delegated to a mesh node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshTask {
    pub id: String,
    pub prompt: String,
    pub required_capabilities: Vec<String>,
    pub project_root: Option<String>,
    pub timeout_secs: u64,
}

/// Result of a delegated task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshTaskResult {
    pub task_id: String,
    pub node_id: String,
    pub success: bool,
    pub output: String,
    pub elapsed_secs: f64,
    pub cost_usd: f64,
}

/// Affinity rule: route tasks matching a pattern to a specific node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffinityRule {
    pub pattern: String,
    pub target_node: String,
}

/// The mesh delegation engine.
pub struct MeshDelegation {
    affinity_rules: Vec<AffinityRule>,
}

impl MeshDelegation {
    pub fn new() -> Self {
        Self { affinity_rules: Vec::new() }
    }

    pub fn add_affinity(&mut self, pattern: String, target_node: String) {
        self.affinity_rules.push(AffinityRule { pattern, target_node });
    }

    /// Select the best node for a task.
    pub fn select_node<'a>(
        &self,
        task: &MeshTask,
        registry: &'a NodeRegistry,
    ) -> Option<&'a NodeDescriptor> {
        // Check affinity rules first
        for rule in &self.affinity_rules {
            if task.prompt.contains(&rule.pattern)
                || task.required_capabilities.iter().any(|c| c.contains(&rule.pattern))
            {
                let nodes = registry.alive_nodes();
                if let Some(node) = nodes.iter().find(|n| n.id == rule.target_node) {
                    return Some(node);
                }
            }
        }

        // Score all capable nodes
        let mut candidates: Vec<(&NodeDescriptor, f64)> = registry
            .find_by_capability(&task.required_capabilities)
            .into_iter()
            .map(|node| (node, registry.score_node(node, &task.required_capabilities)))
            .collect();

        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        candidates.first().map(|(node, _)| *node)
    }

    /// Delegate a task to a remote node via TCP.
    pub async fn delegate(
        &self,
        task: &MeshTask,
        target: &NodeDescriptor,
    ) -> Result<MeshTaskResult, String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let json = serde_json::to_vec(task).map_err(|e| e.to_string())?;
        let mut stream = tokio::net::TcpStream::connect(target.addr).await
            .map_err(|e| format!("Connect to {}: {}", target.addr, e))?;
        stream.write_all(&json).await.map_err(|e| e.to_string())?;
        stream.flush().await.map_err(|e| e.to_string())?;

        // Read response
        let mut buf = vec![0u8; 1 << 20]; // 1MB max
        let n = stream.read(&mut buf).await.map_err(|e| e.to_string())?;
        let result: MeshTaskResult = serde_json::from_slice(&buf[..n])
            .map_err(|e| format!("Parse response: {}", e))?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeRegistry;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    fn node(id: &str, caps: &[&str], load: f64) -> NodeDescriptor {
        NodeDescriptor {
            id: id.to_string(),
            name: id.to_string(),
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9000),
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
            model: None,
            load,
            gpu: None,
            project_roots: Vec::new(),
            joined_at: chrono::Utc::now(),
            last_heartbeat: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_delegation_selects_best_node() {
        let mut reg = NodeRegistry::new();
        reg.upsert(node("gpu", &["ml", "cuda"], 0.1));
        reg.upsert(node("ci", &["docker", "test"], 0.5));

        let engine = MeshDelegation::new();
        let task = MeshTask {
            id: "t1".to_string(),
            prompt: "train model".to_string(),
            required_capabilities: vec!["ml".to_string()],
            project_root: None,
            timeout_secs: 300,
        };

        let selected = engine.select_node(&task, &reg);
        assert!(selected.is_some());
        assert_eq!(selected.unwrap().id, "gpu");
    }

    #[test]
    fn test_affinity_rules() {
        let mut reg = NodeRegistry::new();
        reg.upsert(node("gpu", &["ml"], 0.1));
        reg.upsert(node("ci", &["test"], 0.1));

        let mut engine = MeshDelegation::new();
        engine.add_affinity("test".to_string(), "ci".to_string());

        let task = MeshTask {
            id: "t1".to_string(),
            prompt: "run tests".to_string(),
            required_capabilities: vec!["test".to_string()],
            project_root: None,
            timeout_secs: 60,
        };

        let selected = engine.select_node(&task, &reg);
        assert_eq!(selected.unwrap().id, "ci");
    }
}
