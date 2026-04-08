use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Skill source priority (later wins on name collision).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkillSource {
    Builtin, // bundled in binary
    User,    // ~/.pipit/skills/
    Project, // .pipit/skills/
    CliDir,  // --add-dir
}

/// Tier 1 — always in memory. ~100 tokens per skill in system prompt.
#[derive(Debug, Clone)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub source: SkillSource,
    pub frontmatter: SkillFrontmatter,
}

/// YAML frontmatter parsed from SKILL.md header.
#[derive(Debug, Clone, Default)]
pub struct SkillFrontmatter {
    pub disable_model_invocation: bool,
    pub user_invocable: bool,
    pub allowed_tools: Option<Vec<String>>,
    pub agent: Option<AgentConfig>,
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub model: Option<String>,
    pub max_turns: Option<u32>,
}

impl SkillMetadata {
    /// Token cost of this skill in the system prompt (Tier 1).
    pub fn prompt_tokens(&self) -> usize {
        // name + description ≈ 100 tokens
        (self.name.len() + self.description.len()) / 4
    }
}
