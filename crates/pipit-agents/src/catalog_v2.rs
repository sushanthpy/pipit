//! AgentCatalog — three-tier agent discovery with graceful fallback.
//!
//! Resolution order (first match wins):
//!   1. Project-scope:  <project>/.pipit/agents/*.md     (repo-controlled)
//!   2. Project-scope:  <project>/.claude/agents/*.md    (compat with abc format)
//!   3. User-scope:     ~/.pipit/agents/*.md             (user's personal agents)
//!   4. User-scope:     ~/.claude/agents/*.md            (compat)
//!   5. Built-in:       explore, plan, verify, general, guide (always available)
//!
//! The catalog NEVER returns an empty list — the five built-ins are always
//! included as fallback. This means the subagent tool ALWAYS has at least
//! five named personas to dispatch to, regardless of filesystem state.
//!
//! ## Trust model
//!
//! Project-scoped agents can contain destructive instructions (they're
//! repo-controlled code). On FIRST use of a project agent in a session, we
//! require explicit user confirmation. The confirmation is scoped to
//! (repo_root, agent_name, content_hash). Content drift invalidates trust.
//! User-scope and built-in agents don't need confirmation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

// ── Types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum AgentScope {
    /// `.pipit/agents/*.md` or `.claude/agents/*.md` in the repo.
    Project,
    /// `~/.pipit/agents/*.md` or `~/.claude/agents/*.md`.
    User,
    /// Compiled-in default agents (always available).
    BuiltIn,
}

impl AgentScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "user",
            Self::BuiltIn => "built-in",
        }
    }

    /// Precedence order — lower wins when names collide.
    fn precedence(self) -> u8 {
        match self {
            Self::Project => 0,
            Self::User => 1,
            Self::BuiltIn => 2,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CatalogAgent {
    pub name: String,
    pub description: String,
    /// If None, default research tools are used.
    pub tools: Option<Vec<String>>,
    /// If None, inherits the parent's model.
    pub model: Option<String>,
    /// System prompt body (the markdown after the YAML frontmatter).
    pub system_prompt: String,
    pub scope: AgentScope,
    /// Path the agent was loaded from (None for built-ins).
    pub source_path: Option<PathBuf>,
    /// Hash of the system_prompt. Used for trust gating on project agents.
    pub content_hash: String,
}

// ── Catalog ─────────────────────────────────────────────────────────────

pub struct AgentCatalog {
    agents: HashMap<String, CatalogAgent>,
    loaded_from: Vec<(AgentScope, PathBuf)>,
}

impl AgentCatalog {
    /// Build the catalog by walking project → user → built-in in order.
    /// The catalog is never empty — built-ins are always included.
    pub fn discover(project_root: &Path) -> Self {
        let mut agents: HashMap<String, CatalogAgent> = HashMap::new();
        let mut loaded_from = Vec::new();

        // Tier 3: Built-ins — the floor.
        for agent in builtin_agents_as_catalog_entries() {
            agents.insert(agent.name.clone(), agent);
        }

        // Tier 2: User scope (~/.pipit/agents, ~/.claude/agents)
        if let Some(home) = dirs::home_dir() {
            for dir_name in &[".pipit/agents", ".claude/agents"] {
                let dir = home.join(dir_name);
                if dir.is_dir() {
                    match load_agents_from_dir(&dir, AgentScope::User) {
                        Ok(loaded) => {
                            loaded_from.push((AgentScope::User, dir.clone()));
                            for agent in loaded {
                                agents.insert(agent.name.clone(), agent);
                            }
                        }
                        Err(e) => {
                            tracing::warn!(dir = %dir.display(), error = %e,
                                "Failed to load user-scope agents — falling back to lower tier");
                        }
                    }
                }
            }
        }

        // Tier 1: Project scope (<root>/.pipit/agents, <root>/.claude/agents)
        for dir_name in &[".pipit/agents", ".claude/agents"] {
            let dir = project_root.join(dir_name);
            if dir.is_dir() {
                match load_agents_from_dir(&dir, AgentScope::Project) {
                    Ok(loaded) => {
                        loaded_from.push((AgentScope::Project, dir.clone()));
                        for agent in loaded {
                            agents.insert(agent.name.clone(), agent);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(dir = %dir.display(), error = %e,
                            "Failed to load project-scope agents — continuing with lower tiers");
                    }
                }
            }
        }

        tracing::info!(
            agent_count = agents.len(),
            tiers_loaded = loaded_from.len(),
            "Agent catalog ready (built-ins always present as fallback)"
        );

        Self { agents, loaded_from }
    }

    /// Returns a list of all available agents, deterministically sorted.
    pub fn list(&self) -> Vec<&CatalogAgent> {
        let mut list: Vec<&CatalogAgent> = self.agents.values().collect();
        list.sort_by(|a, b| {
            a.scope
                .precedence()
                .cmp(&b.scope.precedence())
                .then_with(|| a.name.cmp(&b.name))
        });
        list
    }

    /// Look up an agent by name. Falls back to case-insensitive match.
    pub fn resolve(&self, name: &str) -> Option<&CatalogAgent> {
        if let Some(agent) = self.agents.get(name) {
            return Some(agent);
        }
        let lower = name.to_ascii_lowercase();
        self.agents
            .values()
            .find(|a| a.name.to_ascii_lowercase() == lower)
    }

    /// Resolve-or-fallback — used by SubagentTool when the model calls a
    /// non-existent agent.
    pub fn resolve_or_fallback(&self, name: &str, allow_writes: bool) -> &CatalogAgent {
        if let Some(agent) = self.resolve(name) {
            return agent;
        }
        let fallback_name = if allow_writes { "general" } else { "explore" };
        self.agents
            .get(fallback_name)
            .expect("built-in agents are always present as fallback")
    }

    /// True if the agent is trusted automatically.
    pub fn is_auto_trusted(&self, name: &str) -> bool {
        match self.resolve(name) {
            Some(a) => matches!(a.scope, AgentScope::User | AgentScope::BuiltIn),
            None => true,
        }
    }

    /// Diagnostic summary.
    pub fn diagnostic_summary(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("{} agent(s) available:\n", self.agents.len()));
        for agent in self.list() {
            out.push_str(&format!(
                "  - {:16} ({:9})  {}\n",
                agent.name,
                agent.scope.label(),
                agent.description.chars().take(80).collect::<String>()
            ));
        }
        if self.loaded_from.is_empty() {
            out.push_str("\nNo agent directories found — using built-ins only.\n");
            out.push_str("To add agents, create ~/.pipit/agents/*.md with YAML frontmatter.\n");
        } else {
            out.push_str("\nLoaded from:\n");
            for (scope, path) in &self.loaded_from {
                out.push_str(&format!("  - [{}] {}\n", scope.label(), path.display()));
            }
        }
        out
    }
}

// ── Built-in → CatalogAgent conversion ─────────────────────────────────

fn builtin_agents_as_catalog_entries() -> Vec<CatalogAgent> {
    crate::builtins::builtin_agents()
        .into_iter()
        .map(|a| CatalogAgent {
            name: a.name.clone(),
            description: a.description.clone(),
            tools: if a.allowed_tools.is_empty() {
                None
            } else {
                Some(a.allowed_tools.into_iter().collect())
            },
            model: None,
            system_prompt: a.system_prompt.clone(),
            scope: AgentScope::BuiltIn,
            source_path: None,
            content_hash: hash_content(&a.system_prompt),
        })
        .collect()
}

// ── Frontmatter parser ──────────────────────────────────────────────────

fn load_agents_from_dir(
    dir: &Path,
    scope: AgentScope,
) -> std::io::Result<Vec<CatalogAgent>> {
    let mut agents = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        match parse_agent_file(&path, scope) {
            Ok(agent) => agents.push(agent),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e,
                    "Skipping invalid agent file");
            }
        }
    }
    Ok(agents)
}

fn parse_agent_file(path: &Path, scope: AgentScope) -> Result<CatalogAgent, String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let (frontmatter, body) = split_frontmatter(&content)?;

    let fm: FrontMatter = serde_yaml_ng::from_str(frontmatter).map_err(|e| {
        format!("Invalid YAML frontmatter in {}: {}", path.display(), e)
    })?;

    let name = fm
        .name
        .ok_or_else(|| format!("{}: missing 'name' field", path.display()))?;
    let description = fm
        .description
        .unwrap_or_else(|| "(no description)".to_string());

    let tools = match fm.tools {
        Some(FlexibleTools::List(v)) if !v.is_empty() => Some(v),
        Some(FlexibleTools::Csv(s)) if !s.trim().is_empty() => Some(
            s.split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect(),
        ),
        _ => None,
    };

    let content_hash = hash_content(body);

    Ok(CatalogAgent {
        name,
        description,
        tools,
        model: fm.model,
        system_prompt: body.to_string(),
        scope,
        source_path: Some(path.to_path_buf()),
        content_hash,
    })
}

fn split_frontmatter(content: &str) -> Result<(&str, &str), String> {
    let trimmed = content.trim_start();
    if let Some(rest) = trimmed.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            return Ok((&rest[..end], &rest[end + 5..]));
        }
        if let Some(end) = rest.find("\n---\r\n") {
            return Ok((&rest[..end], &rest[end + 6..]));
        }
    }
    Err("missing YAML frontmatter".into())
}

fn hash_content(s: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[derive(serde::Deserialize)]
struct FrontMatter {
    name: Option<String>,
    description: Option<String>,
    tools: Option<FlexibleTools>,
    model: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum FlexibleTools {
    List(Vec<String>),
    Csv(String),
}

// ── Trust store ─────────────────────────────────────────────────────────

/// Persistent trust store for project-scope agents.
#[derive(Clone, Default)]
pub struct TrustStore {
    inner: Arc<RwLock<HashMap<String, u64>>>,
    path: Option<PathBuf>,
}

impl TrustStore {
    pub fn load() -> Self {
        let path = dirs::home_dir().map(|h| h.join(".pipit").join("trusted_agents.json"));
        let inner = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(RwLock::new(inner)),
            path,
        }
    }

    pub fn is_trusted(&self, project_root: &Path, agent: &CatalogAgent) -> bool {
        if agent.scope != AgentScope::Project {
            return true;
        }
        let key = trust_key(project_root, &agent.name, &agent.content_hash);
        self.inner
            .read()
            .map(|m| m.contains_key(&key))
            .unwrap_or(false)
    }

    pub fn trust(&self, project_root: &Path, agent: &CatalogAgent) -> std::io::Result<()> {
        let key = trust_key(project_root, &agent.name, &agent.content_hash);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Ok(mut m) = self.inner.write() {
            m.insert(key, now);
        }
        self.persist()
    }

    fn persist(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let snapshot = self.inner.read().map(|m| m.clone()).unwrap_or_default();
        let json = serde_json::to_string_pretty(&snapshot)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, json)
    }
}

impl std::fmt::Debug for TrustStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrustStore")
            .field("path", &self.path)
            .finish()
    }
}

fn trust_key(project_root: &Path, name: &str, hash: &str) -> String {
    format!("{}|{}|{}", project_root.display(), name, hash)
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_always_has_builtins_even_with_no_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = AgentCatalog::discover(tmp.path());

        assert!(catalog.resolve("explore").is_some());
        assert!(catalog.resolve("plan").is_some());
        assert!(catalog.resolve("verify").is_some());
        assert!(catalog.resolve("general").is_some());
        assert!(catalog.resolve("guide").is_some());
        assert!(!catalog.list().is_empty(), "catalog must never be empty");
    }

    #[test]
    fn catalog_resolves_case_insensitively() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = AgentCatalog::discover(tmp.path());
        assert!(catalog.resolve("Explore").is_some());
        assert!(catalog.resolve("EXPLORE").is_some());
    }

    #[test]
    fn resolve_or_fallback_never_panics() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = AgentCatalog::discover(tmp.path());
        let a = catalog.resolve_or_fallback("nonexistent-agent", false);
        assert_eq!(a.name, "explore");
        let b = catalog.resolve_or_fallback("also-nonexistent", true);
        assert_eq!(b.name, "general");
    }

    #[test]
    fn project_scope_overrides_user_and_builtin() {
        let tmp = tempfile::tempdir().unwrap();
        let agents_dir = tmp.path().join(".pipit/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("verify.md"),
            "---\nname: verify\ndescription: Project's own verify\n---\nCustom verify prompt",
        )
        .unwrap();

        let catalog = AgentCatalog::discover(tmp.path());
        let verify = catalog.resolve("verify").unwrap();
        assert_eq!(verify.scope, AgentScope::Project);
        assert!(verify.system_prompt.contains("Custom verify prompt"));
    }

    #[test]
    fn diagnostic_summary_reports_fallback_state() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = AgentCatalog::discover(tmp.path());
        let diag = catalog.diagnostic_summary();
        // Built-in agents are always present in the diagnostic output
        assert!(diag.contains("explore"));
        assert!(diag.contains("agent(s) available"));
    }

    #[test]
    fn auto_trust_rules_match_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = AgentCatalog::discover(tmp.path());
        assert!(catalog.is_auto_trusted("explore"));
        assert!(catalog.is_auto_trusted("nonexistent"));
    }

    #[test]
    fn invalid_frontmatter_does_not_poison_catalog() {
        let tmp = tempfile::tempdir().unwrap();
        let agents_dir = tmp.path().join(".pipit/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("broken.md"), "no frontmatter here").unwrap();
        std::fs::write(
            agents_dir.join("good.md"),
            "---\nname: good\ndescription: ok\n---\nBody",
        )
        .unwrap();

        let catalog = AgentCatalog::discover(tmp.path());
        assert!(catalog.resolve("explore").is_some());
        assert!(catalog.resolve("good").is_some());
        assert!(catalog.resolve("broken").is_none());
    }
}
