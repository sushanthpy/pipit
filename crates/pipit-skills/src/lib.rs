pub mod conditional;
pub mod discovery;
pub mod dynamic;
pub mod eval;
pub mod frontmatter;
pub mod loader;
pub mod manifest;
pub mod policy;
pub mod telemetry;

pub use conditional::ConditionalRegistry;
pub use discovery::SkillRegistry;
pub use dynamic::DynamicDiscovery;
pub use frontmatter::{SkillFrontmatter, SkillMetadata, SkillSource};
pub use loader::LoadedSkill;
pub use manifest::{SkillManifest, SkillPackage, TrustTier};
pub use policy::{PolicyDecision, SkillPolicyEngine};
pub use telemetry::{SkillExecutionRecord, SkillTelemetryStore};

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SkillError {
    #[error("Skill not found: {0}")]
    NotFound(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Frontmatter parse error in {path}: {detail}")]
    FrontmatterParse {
        path: PathBuf,
        detail: String,
    },
}
