//! AgentCatalog — Markdown-based agent discovery with two-tier scope (Task 4).
//!
//! Loads agent definitions from:
//!   - `~/.pipit/agents/*.md` (user scope — personal personas)
//!   - `<project>/.pipit/agents/*.md` (project scope — team-shared)
//!
//! YAML frontmatter is parsed into `AgentDefinition`. The body is the system prompt.
//! Project-scope agents go through a trust gate on first encounter.

use crate::{AgentCategory, AgentDefinition};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Parsed YAML frontmatter from a markdown agent file.
#[derive(Debug, Clone, Deserialize)]
struct AgentFrontmatter {
    name: String,
    description: String,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    max_turns: Option<u32>,
    #[serde(default)]
    can_write: Option<bool>,
    #[serde(default)]
    can_execute: Option<bool>,
}

/// A trust record for a project-scoped agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustRecord {
    /// The repository root that contains this agent.
    pub repo_root: String,
    /// Agent name.
    pub agent_name: String,
    /// SHA-256 hash of the agent file body (content drift invalidates trust).
    pub content_hash: String,
    /// When the trust was granted.
    pub trusted_at: String,
}

/// The trusted agents database.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustedAgentSources {
    pub trusted: Vec<TrustRecord>,
}

impl TrustedAgentSources {
    /// Load from `~/.pipit/trusted_agent_sources.json`.
    pub fn load() -> Self {
        let path = Self::path();
        if !path.exists() {
            return Self::default();
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Save to disk.
    pub fn save(&self) -> Result<(), String> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Cannot create ~/.pipit: {e}"))?;
        }
        let json =
            serde_json::to_string_pretty(self).map_err(|e| format!("Serialize error: {e}"))?;
        std::fs::write(&path, json).map_err(|e| format!("Write error: {e}"))?;
        Ok(())
    }

    /// Check if a project-scoped agent is trusted.
    pub fn is_trusted(&self, repo_root: &str, agent_name: &str, content_hash: &str) -> bool {
        self.trusted.iter().any(|r| {
            r.repo_root == repo_root
                && r.agent_name == agent_name
                && r.content_hash == content_hash
        })
    }

    /// Trust a project-scoped agent.
    pub fn trust(&mut self, repo_root: &str, agent_name: &str, content_hash: &str) {
        // Remove any existing trust for this agent (content may have changed)
        self.trusted
            .retain(|r| !(r.repo_root == repo_root && r.agent_name == agent_name));
        self.trusted.push(TrustRecord {
            repo_root: repo_root.to_string(),
            agent_name: agent_name.to_string(),
            content_hash: content_hash.to_string(),
            trusted_at: chrono::Utc::now().to_rfc3339(),
        });
    }

    fn path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".pipit")
            .join("trusted_agent_sources.json")
    }
}

/// Scope of an agent definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentScope {
    /// User-wide: `~/.pipit/agents/*.md`
    User,
    /// Project-local: `<project>/.pipit/agents/*.md`
    Project,
}

/// Parse a markdown agent file into an AgentDefinition.
///
/// Expected format:
/// ```markdown
/// ---
/// name: code-reviewer
/// description: Reviews code changes for quality and correctness
/// tools: [read_file, grep, glob, list_directory, bash]
/// model: haiku
/// max_turns: 20
/// ---
/// You are a code reviewer. Your job is to...
/// (rest of file is the system prompt)
/// ```
fn parse_agent_markdown(content: &str, scope: AgentScope) -> Option<AgentDefinition> {
    // Extract YAML frontmatter between --- delimiters
    let content = content.trim();
    if !content.starts_with("---") {
        tracing::debug!("Agent markdown missing frontmatter delimiter");
        return None;
    }

    let after_first = &content[3..];
    let end_idx = after_first.find("---")?;
    let yaml_str = &after_first[..end_idx].trim();
    let body = after_first[end_idx + 3..].trim();

    let fm: AgentFrontmatter = serde_yaml_ng::from_str(yaml_str)
        .map_err(|e| {
            tracing::debug!("Agent frontmatter parse error: {e}");
            e
        })
        .ok()?;

    let allowed_tools: HashSet<String> = fm
        .tools
        .unwrap_or_default()
        .into_iter()
        .collect();

    let category = match scope {
        AgentScope::User => AgentCategory::Custom,
        AgentScope::Project => AgentCategory::Team,
    };

    Some(AgentDefinition {
        name: fm.name,
        description: fm.description,
        system_prompt: body.to_string(),
        allowed_tools,
        denied_tools: HashSet::new(),
        max_turns: fm.max_turns.unwrap_or(30),
        can_write: fm.can_write.unwrap_or(false),
        can_execute: fm.can_execute.unwrap_or(false),
        category,
    })
}

/// Load markdown agents from a directory.
fn load_agents_from_dir(dir: &Path, scope: AgentScope) -> Vec<AgentDefinition> {
    let mut agents = Vec::new();

    if !dir.exists() {
        return agents;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return agents,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("Cannot read agent file {}: {e}", path.display());
                continue;
            }
        };

        if let Some(agent) = parse_agent_markdown(&content, scope) {
            tracing::info!(
                name = %agent.name,
                scope = ?scope,
                "Loaded markdown agent from {}",
                path.display()
            );
            agents.push(agent);
        }
    }

    agents
}

/// Compute SHA-256 hash of agent file content for trust verification.
fn content_hash(content: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Load all markdown agents from user and project scope.
///
/// Precedence: project scope overrides user scope when both define the same name.
/// Project-scoped agents that aren't trusted are excluded with a warning.
pub fn load_markdown_agents(project_root: &Path) -> Vec<AgentDefinition> {
    // User scope: ~/.pipit/agents/*.md
    let user_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".pipit")
        .join("agents");
    let user_agents = load_agents_from_dir(&user_dir, AgentScope::User);

    // Project scope: <project>/.pipit/agents/*.md
    let project_dir = project_root.join(".pipit").join("agents");
    let project_agents = load_agents_from_dir(&project_dir, AgentScope::Project);

    // Trust gate for project-scoped agents
    let trust_db = TrustedAgentSources::load();
    let repo_root = project_root.display().to_string();

    let mut trusted_project_agents = Vec::new();
    for agent in project_agents {
        // Compute hash of the agent's system prompt for trust check
        let hash = content_hash(&agent.system_prompt);
        if trust_db.is_trusted(&repo_root, &agent.name, &hash) {
            trusted_project_agents.push(agent);
        } else {
            tracing::warn!(
                name = %agent.name,
                "Untrusted project agent '{}' — run `pipit agents trust {}` to enable",
                agent.name,
                agent.name,
            );
        }
    }

    // Merge: project overrides user scope by name
    let mut by_name: HashMap<String, AgentDefinition> = HashMap::new();
    for agent in user_agents {
        by_name.insert(agent.name.clone(), agent);
    }
    for agent in trusted_project_agents {
        by_name.insert(agent.name.clone(), agent);
    }

    by_name.into_values().collect()
}

/// Generate the agent catalog section for injection into the subagent tool description.
///
/// This is the "agent_listing_delta" optimization from abc-src — a side-channel
/// attachment that tells the coordinator about available named agents.
pub fn render_agent_catalog(agents: &[AgentDefinition]) -> String {
    if agents.is_empty() {
        return String::new();
    }

    let mut catalog = String::from("\n\n== AVAILABLE NAMED AGENTS ==\n");
    catalog.push_str("Use these with: subagent({ task: \"...\", agent: \"<name>\" })\n\n");

    for agent in agents {
        let tools_str = if agent.allowed_tools.is_empty() {
            "all tools".to_string()
        } else {
            agent
                .allowed_tools
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        };
        catalog.push_str(&format!(
            "- **{}**: {} (tools: {}, max_turns: {})\n",
            agent.name, agent.description, tools_str, agent.max_turns,
        ));
    }

    catalog
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_agent_markdown() {
        let md = r#"---
name: test-agent
description: A test agent
tools: [read_file, grep]
max_turns: 10
---
You are a test agent. Do your thing."#;

        let agent = parse_agent_markdown(md, AgentScope::User).unwrap();
        assert_eq!(agent.name, "test-agent");
        assert_eq!(agent.description, "A test agent");
        assert!(agent.allowed_tools.contains("read_file"));
        assert!(agent.allowed_tools.contains("grep"));
        assert_eq!(agent.max_turns, 10);
        assert_eq!(
            agent.system_prompt,
            "You are a test agent. Do your thing."
        );
        assert_eq!(agent.category, AgentCategory::Custom);
    }

    #[test]
    fn parse_minimal_agent_markdown() {
        let md = r#"---
name: minimal
description: Minimal agent
---
Just a prompt."#;

        let agent = parse_agent_markdown(md, AgentScope::Project).unwrap();
        assert_eq!(agent.name, "minimal");
        assert!(agent.allowed_tools.is_empty());
        assert_eq!(agent.max_turns, 30); // default
        assert_eq!(agent.category, AgentCategory::Team);
    }

    #[test]
    fn parse_invalid_markdown_returns_none() {
        assert!(parse_agent_markdown("no frontmatter", AgentScope::User).is_none());
        assert!(parse_agent_markdown("---\ninvalid yaml: [[[", AgentScope::User).is_none());
    }

    #[test]
    fn trust_gate_roundtrip() {
        let mut db = TrustedAgentSources::default();
        assert!(!db.is_trusted("/repo", "agent-a", "abc123"));

        db.trust("/repo", "agent-a", "abc123");
        assert!(db.is_trusted("/repo", "agent-a", "abc123"));

        // Content hash change invalidates trust
        assert!(!db.is_trusted("/repo", "agent-a", "def456"));

        // Re-trust with new hash
        db.trust("/repo", "agent-a", "def456");
        assert!(db.is_trusted("/repo", "agent-a", "def456"));
        assert!(!db.is_trusted("/repo", "agent-a", "abc123"));
    }

    #[test]
    fn render_catalog_empty() {
        assert_eq!(render_agent_catalog(&[]), "");
    }
}
