use crate::frontmatter::SkillMetadata;
use std::path::PathBuf;

/// Tier 2 — loaded on demand. Full skill instructions + references.
#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub metadata: SkillMetadata,
    pub body: String,
    pub supporting_files: Vec<(String, PathBuf)>,
}

impl LoadedSkill {
    /// Format the skill body as a message to inject into the conversation.
    pub fn as_injection(&self, user_args: &str) -> String {
        let expanded = crate::discovery::SkillRegistry::expand_arguments(&self.body, user_args);
        format!(
            "[Skill: {}]\n{}\n\nUser request: {}",
            self.metadata.name, expanded, user_args
        )
    }

    /// Get the token cost estimate for this loaded skill.
    pub fn estimated_tokens(&self) -> usize {
        self.body.split_whitespace().count() * 13 / 10 // words × 1.3
    }
}
