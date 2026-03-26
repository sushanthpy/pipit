pub mod discovery;
pub mod frontmatter;
pub mod loader;

pub use discovery::SkillRegistry;
pub use frontmatter::{SkillFrontmatter, SkillMetadata, SkillSource};
pub use loader::LoadedSkill;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SkillError {
    #[error("Skill not found: {0}")]
    NotFound(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Frontmatter parse error: {0}")]
    FrontmatterParse(String),
}
