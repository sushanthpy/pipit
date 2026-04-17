use std::path::PathBuf;

/// Skill source tier — explicit ordering for precedence and trust decisions.
/// Builtin < User < Project < CliDir < Policy.
/// When the dedup pass encounters two canonical-path matches,
/// resolution picks the *lower* (more trusted) source tier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SkillSource {
    Builtin, // bundled in binary
    User,    // ~/.pipit/skills/
    Project, // .pipit/skills/
    CliDir,  // --add-dir
    Policy,  // enterprise policy-managed directory
}

impl SkillSource {
    /// Numeric precedence for deterministic ordering.
    /// Lower value = higher trust, wins on dedup collision.
    fn precedence(&self) -> u8 {
        match self {
            Self::Builtin => 0,
            Self::User => 1,
            Self::Project => 2,
            Self::CliDir => 3,
            Self::Policy => 4,
        }
    }
}

impl PartialOrd for SkillSource {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SkillSource {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.precedence().cmp(&other.precedence())
    }
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
/// Bijective with the on-disk YAML schema — every documented field is represented.
#[derive(Debug, Clone, Default)]
pub struct SkillFrontmatter {
    pub disable_model_invocation: bool,
    pub user_invocable: bool,
    pub allowed_tools: Option<Vec<String>>,
    pub agent: Option<AgentConfig>,
    /// Gitignore-style path patterns — skill is dormant until a matching file is touched.
    pub paths: Option<Vec<String>>,
    /// Short sentence describing when the model should use this skill.
    pub when_to_use: Option<String>,
    /// Hint shown to the user for expected arguments.
    pub argument_hint: Option<String>,
    /// Model override for this skill's execution.
    pub model: Option<String>,
    /// Reasoning effort level override (e.g., "low", "medium", "high").
    pub effort: Option<String>,
    /// Lifecycle hooks declared by the skill.
    pub hooks: Option<Vec<HookDeclaration>>,
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub model: Option<String>,
    pub max_turns: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct HookDeclaration {
    pub event: String,
    pub command: String,
}

impl SkillMetadata {
    /// Token cost of this skill in the system prompt (Tier 1).
    pub fn prompt_tokens(&self) -> usize {
        // name + description ≈ 100 tokens
        (self.name.len() + self.description.len()) / 4
    }

    /// Whether this skill is path-conditional (has `paths:` declared).
    pub fn is_conditional(&self) -> bool {
        self.frontmatter
            .paths
            .as_ref()
            .is_some_and(|p| !p.is_empty())
    }
}
