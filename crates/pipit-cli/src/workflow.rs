use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct WorkflowAssets {
    pub skill_dirs: Vec<PathBuf>,
    pub command_dirs: Vec<PathBuf>,
    pub agent_dirs: Vec<PathBuf>,
    pub rule_dirs: Vec<PathBuf>,
    pub hook_files: Vec<PathBuf>,
    pub mcp_files: Vec<PathBuf>,
    pub instruction_files: Vec<PathBuf>,
}

impl WorkflowAssets {
    pub fn discover(project_root: &Path) -> Self {
        let mut assets = Self::default();

        // Skills
        for dir in [
            project_root.join(".pipit").join("skills"),
            project_root.join(".github").join("skills"),
        ] {
            if dir.exists() {
                assets.skill_dirs.push(dir);
            }
        }

        // Commands (markdown files that define slash commands)
        for dir in [
            project_root.join(".pipit").join("commands"),
            project_root.join(".github").join("commands"),
        ] {
            if dir.exists() {
                assets.command_dirs.push(dir);
            }
        }

        // Agents (markdown files with tool/model declarations)
        for dir in [
            project_root.join(".pipit").join("agents"),
            project_root.join(".github").join("agents"),
        ] {
            if dir.exists() {
                assets.agent_dirs.push(dir);
            }
        }

        // Rules (markdown files concatenated into system prompt)
        for dir in [
            project_root.join(".pipit").join("rules"),
            project_root.join(".github").join("rules"),
        ] {
            if dir.exists() {
                assets.rule_dirs.push(dir);
            }
        }

        // Hooks
        for hooks_dir in [
            project_root.join(".pipit").join("hooks"),
            project_root.join(".github").join("hooks"),
        ] {
            assets.hook_files.extend(json_files_in(&hooks_dir));
        }

        // MCP configs
        for file in [
            project_root.join(".pipit").join("mcp.json"),
            project_root.join(".github").join("mcp.json"),
            project_root.join("mcp.json"),
        ] {
            if file.exists() {
                assets.mcp_files.push(file);
            }
        }

        // Instruction files
        for file in [
            project_root.join("AGENTS.md"),
            project_root.join("PIPIT.md"),
            project_root.join(".github").join("AGENTS.md"),
            project_root.join(".github").join("copilot-instructions.md"),
            project_root.join(".pipit").join("CONVENTIONS.md"),
        ] {
            if file.exists() {
                assets.instruction_files.push(file);
            }
        }

        assets
    }

    pub fn skill_search_paths(&self) -> Vec<PathBuf> {
        self.skill_dirs.clone()
    }

    /// Discover custom commands from commands/ directories.
    /// Returns (name, description, file_path) tuples.
    pub fn discover_commands(&self) -> Vec<(String, String, PathBuf)> {
        let mut commands = Vec::new();
        for dir in &self.command_dirs {
            for entry in md_files_in(dir) {
                let name = entry
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let description = first_description_line(&entry);
                commands.push((name, description, entry));
            }
        }
        commands.sort_by(|a, b| a.0.cmp(&b.0));
        commands
    }

    /// Discover agent definitions from agents/ directories.
    /// Returns (name, description, model, tools, file_path) tuples.
    pub fn discover_agents(&self) -> Vec<AgentDefinition> {
        let mut agents = Vec::new();
        for dir in &self.agent_dirs {
            for entry in md_files_in(dir) {
                if let Some(agent) = parse_agent_definition(&entry) {
                    agents.push(agent);
                }
            }
        }
        agents.sort_by(|a, b| a.name.cmp(&b.name));
        agents
    }

    /// Load all rules from rules/ directories, concatenated for system prompt injection.
    pub fn load_rules(&self) -> String {
        let mut rules = String::new();
        for dir in &self.rule_dirs {
            let mut files = md_files_in(dir);
            files.sort(); // deterministic order
            for file in files {
                if let Ok(content) = std::fs::read_to_string(&file) {
                    let name = file
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy();
                    if !rules.is_empty() {
                        rules.push_str("\n\n");
                    }
                    rules.push_str(&format!("### Rule: {}\n", name));
                    rules.push_str(&strip_frontmatter(&content));
                }
            }
        }
        rules
    }

    pub fn ui_summary(&self, skill_count: usize, command_count: usize, agent_count: usize) -> Option<String> {
        let has_any = skill_count > 0
            || command_count > 0
            || agent_count > 0
            || !self.hook_files.is_empty()
            || !self.mcp_files.is_empty()
            || !self.instruction_files.is_empty()
            || !self.rule_dirs.is_empty();
        if !has_any {
            return None;
        }

        let mut parts = Vec::new();
        if skill_count > 0 { parts.push(format!("{} skills", skill_count)); }
        if command_count > 0 { parts.push(format!("{} commands", command_count)); }
        if agent_count > 0 { parts.push(format!("{} agents", agent_count)); }
        if !self.hook_files.is_empty() { parts.push(format!("{} hooks", self.hook_files.len())); }
        if !self.rule_dirs.is_empty() { parts.push("rules".to_string()); }
        if !self.mcp_files.is_empty() { parts.push(format!("{} mcp", self.mcp_files.len())); }
        if !self.instruction_files.is_empty() { parts.push(format!("{} instructions", self.instruction_files.len())); }

        Some(format!("workflow: {}", parts.join(" | ")))
    }

    pub fn status_lines(&self, skill_count: usize) -> Vec<String> {
        let command_count = self.discover_commands().len();
        let agent_count = self.discover_agents().len();
        let mut lines = Vec::new();
        if let Some(summary) = self.ui_summary(skill_count, command_count, agent_count) {
            lines.push(summary);
        }

        if !self.skill_dirs.is_empty() {
            lines.push(format!("skill dirs: {}", join_paths(&self.skill_dirs)));
        }
        if !self.command_dirs.is_empty() {
            lines.push(format!("command dirs: {}", join_paths(&self.command_dirs)));
        }
        if !self.agent_dirs.is_empty() {
            lines.push(format!("agent dirs: {}", join_paths(&self.agent_dirs)));
        }
        if !self.rule_dirs.is_empty() {
            lines.push(format!("rule dirs: {}", join_paths(&self.rule_dirs)));
        }
        if !self.hook_files.is_empty() {
            lines.push(format!("hooks: {}", join_paths(&self.hook_files)));
        }
        if !self.mcp_files.is_empty() {
            lines.push(format!("mcp: {}", join_paths(&self.mcp_files)));
        }
        if !self.instruction_files.is_empty() {
            lines.push(format!(
                "instructions: {}",
                join_paths(&self.instruction_files)
            ));
        }

        lines
    }

    pub fn prompt_section(&self) -> String {
        let mut section = String::new();

        // Rules injection (highest priority — project conventions)
        let rules = self.load_rules();
        if !rules.is_empty() {
            section.push_str("\n## Project Rules\n");
            section.push_str(&rules);
            section.push('\n');
        }

        // Command listing
        let commands = self.discover_commands();
        if !commands.is_empty() {
            section.push_str("\n## Custom Commands\n");
            section.push_str("The user can invoke these with /command-name:\n");
            for (name, desc, _) in &commands {
                section.push_str(&format!("- **{}**: {}\n", name, desc));
            }
        }

        // Agent listing
        let agents = self.discover_agents();
        if !agents.is_empty() {
            section.push_str("\n## Available Agents\n");
            section.push_str("Delegate tasks to specialized agents via the subagent tool:\n");
            for agent in &agents {
                section.push_str(&format!("- **{}**: {}\n", agent.name, agent.description));
            }
        }

        // Instruction files
        for file in &self.instruction_files {
            if let Ok(content) = std::fs::read_to_string(file) {
                let name = file.file_name().unwrap_or_default().to_string_lossy();
                section.push_str(&format!("\n## {}\n", name));
                section.push_str(&content);
                section.push('\n');
            }
        }

        section
    }
}

// ─── Agent definition ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AgentDefinition {
    pub name: String,
    pub description: String,
    pub model: Option<String>,
    pub tools: Vec<String>,
    pub path: PathBuf,
}

fn parse_agent_definition(path: &Path) -> Option<AgentDefinition> {
    let content = std::fs::read_to_string(path).ok()?;
    let name = path.file_stem()?.to_string_lossy().to_string();

    let mut description = String::new();
    let mut model = None;
    let mut tools = Vec::new();

    // Simple YAML frontmatter parsing
    let trimmed = content.trim_start();
    if trimmed.starts_with("---") {
        let after = &trimmed[3..];
        if let Some(end) = after.find("\n---") {
            let yaml = &after[..end];
            for line in yaml.lines() {
                let line = line.trim();
                if let Some(val) = line.strip_prefix("description:") {
                    description = val.trim().trim_matches('"').trim_matches('\'').to_string();
                }
                if let Some(val) = line.strip_prefix("model:") {
                    model = Some(val.trim().to_string());
                }
                if let Some(val) = line.strip_prefix("tools:") {
                    // Parse YAML inline array: ["Read", "Write"]
                    let val = val.trim();
                    if val.starts_with('[') {
                        tools = val
                            .trim_matches(|c| c == '[' || c == ']')
                            .split(',')
                            .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                    }
                }
            }
        }
    }

    if description.is_empty() {
        description = first_description_line(path);
    }

    Some(AgentDefinition {
        name,
        description,
        model,
        tools,
        path: path.to_path_buf(),
    })
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn json_files_in(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return files;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            files.push(path);
        }
    }

    files.sort();
    files
}

fn md_files_in(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return files;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            files.push(path);
        }
    }

    files.sort();
    files
}

fn join_paths(paths: &[PathBuf]) -> String {
    paths.iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Get the first non-empty, non-heading line from a markdown file as a description.
fn first_description_line(path: &Path) -> String {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let body = strip_frontmatter(&content);
    body.lines()
        .find(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('#') && !t.starts_with("---")
        })
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Strip YAML frontmatter from markdown content (public for command loading).
pub fn strip_command_frontmatter(content: &str) -> String {
    strip_frontmatter(content)
}

/// Strip YAML frontmatter from markdown content.
fn strip_frontmatter(content: &str) -> String {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return content.to_string();
    }
    let after = &trimmed[3..];
    if let Some(end) = after.find("\n---") {
        after[end + 4..].to_string()
    } else {
        content.to_string()
    }
}