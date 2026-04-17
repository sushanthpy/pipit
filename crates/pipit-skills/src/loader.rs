use crate::frontmatter::SkillMetadata;
use std::path::{Path, PathBuf};

/// Tier 2 — loaded on demand. Full skill instructions + references.
#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub metadata: SkillMetadata,
    pub body: String,
    pub supporting_files: Vec<(String, PathBuf)>,
}

impl LoadedSkill {
    /// Format the skill body as a message to inject into the conversation.
    /// Expands `$ARGUMENTS`, `${ARGUMENTS}`, `${PIPIT_SKILL_DIR}`, and `${PIPIT_SESSION_ID}`.
    /// Path separators are normalized to `/` on all platforms so downstream consumers
    /// don't encounter unintended escape sequences.
    pub fn as_injection(&self, user_args: &str, session_id: Option<&str>) -> String {
        let skill_dir = if self.metadata.path.is_dir() {
            self.metadata.path.display().to_string()
        } else {
            self.metadata
                .path
                .parent()
                .unwrap_or(Path::new("."))
                .display()
                .to_string()
        };

        // Normalize path separators for Windows compatibility
        let skill_dir_normalized = skill_dir.replace('\\', "/");

        let mut expanded = self
            .body
            .replace("$ARGUMENTS", user_args)
            .replace("${ARGUMENTS}", user_args)
            .replace("${PIPIT_SKILL_DIR}", &skill_dir_normalized);

        if let Some(sid) = session_id {
            expanded = expanded.replace("${PIPIT_SESSION_ID}", sid);
        }

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
