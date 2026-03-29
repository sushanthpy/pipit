//! Agent Capability Registry — Task 2.1
//!
//! Each agent registers capabilities (tool set, language expertise, project context)
//! into a shared registry. Discovery via cosine similarity on sparse binary vectors.
//! sim(q, c) = |q ∩ c| / √(|q| · |c|) — O(min(|q|, |c|)) via sorted set intersection.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Arc;

/// Unique identifier for an agent in the mesh.
pub type AgentId = String;

/// Description of an agent's capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDescriptor {
    pub id: AgentId,
    pub name: String,
    /// Tools this agent can execute (e.g., "bash", "read_file", "pytest").
    pub tools: BTreeSet<String>,
    /// Languages this agent supports (e.g., "rust", "python", "typescript").
    pub languages: BTreeSet<String>,
    /// Projects this agent has context for.
    pub projects: BTreeSet<String>,
    /// Custom capability tags (e.g., "security_audit", "performance_tuning").
    pub tags: BTreeSet<String>,
    /// Endpoint for reaching this agent (e.g., "local", "tcp://host:port").
    pub endpoint: String,
    /// When this registration was last refreshed.
    pub last_seen: chrono::DateTime<chrono::Utc>,
}

/// A capability query — what capabilities does the requester need?
#[derive(Debug, Clone)]
pub struct AgentCapability {
    pub required_tools: BTreeSet<String>,
    pub required_languages: BTreeSet<String>,
    pub required_tags: BTreeSet<String>,
}

/// Thread-safe registry of agent descriptors.
pub struct MeshRegistry {
    agents: DashMap<AgentId, AgentDescriptor>,
}

impl MeshRegistry {
    pub fn new() -> Self {
        Self {
            agents: DashMap::new(),
        }
    }

    /// Register or update an agent's capabilities.
    pub fn register(&self, descriptor: AgentDescriptor) {
        self.agents.insert(descriptor.id.clone(), descriptor);
    }

    /// Remove an agent from the registry.
    pub fn unregister(&self, id: &str) {
        self.agents.remove(id);
    }

    /// Find agents matching a capability query, ranked by similarity.
    /// Uses cosine similarity on sparse binary capability vectors.
    pub fn discover(&self, query: &AgentCapability) -> Vec<(AgentDescriptor, f64)> {
        let query_set = build_capability_set(
            &query.required_tools,
            &query.required_languages,
            &query.required_tags,
        );
        let query_size = query_set.len() as f64;
        if query_size == 0.0 {
            return Vec::new();
        }

        let mut results: Vec<(AgentDescriptor, f64)> = self
            .agents
            .iter()
            .map(|entry| {
                let agent = entry.value().clone();
                let agent_set = build_capability_set(
                    &agent.tools,
                    &agent.languages,
                    &agent.tags,
                );
                let agent_size = agent_set.len() as f64;
                if agent_size == 0.0 {
                    return (agent, 0.0);
                }
                // Cosine similarity for sparse binary vectors:
                // |q ∩ c| / √(|q| · |c|)
                let intersection = query_set.intersection(&agent_set).count() as f64;
                let similarity = intersection / (query_size.sqrt() * agent_size.sqrt());
                (agent, similarity)
            })
            .filter(|(_, sim)| *sim > 0.0)
            .collect();

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results
    }

    /// List all registered agents.
    pub fn list_agents(&self) -> Vec<AgentDescriptor> {
        self.agents.iter().map(|e| e.value().clone()).collect()
    }

    /// Number of registered agents.
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }
}

impl Default for MeshRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a unified capability set from tools, languages, and tags.
fn build_capability_set(
    tools: &BTreeSet<String>,
    languages: &BTreeSet<String>,
    tags: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for t in tools {
        set.insert(format!("tool:{}", t));
    }
    for l in languages {
        set.insert(format!("lang:{}", l));
    }
    for t in tags {
        set.insert(format!("tag:{}", t));
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_agent(id: &str, tools: &[&str], langs: &[&str], tags: &[&str]) -> AgentDescriptor {
        AgentDescriptor {
            id: id.to_string(),
            name: id.to_string(),
            tools: tools.iter().map(|s| s.to_string()).collect(),
            languages: langs.iter().map(|s| s.to_string()).collect(),
            projects: BTreeSet::new(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            endpoint: "local".to_string(),
            last_seen: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_register_and_discover() {
        let registry = MeshRegistry::new();

        registry.register(make_agent("rust-expert", &["bash", "cargo"], &["rust"], &["performance"]));
        registry.register(make_agent("python-expert", &["bash", "pytest"], &["python"], &["testing"]));
        registry.register(make_agent("fullstack", &["bash"], &["rust", "python", "typescript"], &[]));

        let query = AgentCapability {
            required_tools: ["pytest"].iter().map(|s| s.to_string()).collect(),
            required_languages: ["python"].iter().map(|s| s.to_string()).collect(),
            required_tags: BTreeSet::new(),
        };

        let results = registry.discover(&query);
        assert!(!results.is_empty());
        assert_eq!(results[0].0.id, "python-expert", "Python expert should rank first");
    }

    #[test]
    fn test_cosine_similarity_ranking() {
        let registry = MeshRegistry::new();

        // Agent with 2/3 matching capabilities
        registry.register(make_agent("good", &["bash", "pytest"], &["python"], &["testing"]));
        // Agent with 1/3 matching capabilities
        registry.register(make_agent("weak", &["bash"], &["go"], &[]));

        let query = AgentCapability {
            required_tools: ["pytest", "bash"].iter().map(|s| s.to_string()).collect(),
            required_languages: ["python"].iter().map(|s| s.to_string()).collect(),
            required_tags: BTreeSet::new(),
        };

        let results = registry.discover(&query);
        assert!(results.len() >= 2);
        assert!(results[0].1 > results[1].1, "Good match should score higher");
    }
}
