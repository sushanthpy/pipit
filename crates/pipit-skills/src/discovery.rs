use crate::frontmatter::{AgentConfig, SkillFrontmatter, SkillMetadata, SkillSource};
use crate::loader::LoadedSkill;
use crate::SkillError;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Skill registry — discovers and indexes all available skills.
pub struct SkillRegistry {
    skills: HashMap<String, SkillMetadata>,
    loaded_cache: HashMap<String, LoadedSkill>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
            loaded_cache: HashMap::new(),
        }
    }

    /// Discover skills from all sources.
    pub fn discover(search_paths: &[PathBuf]) -> Self {
        let mut registry = Self::new();

        for search_path in search_paths {
            if !search_path.exists() {
                continue;
            }
            let source = if search_path.starts_with(dirs_next_home().unwrap_or_default()) {
                SkillSource::User
            } else {
                SkillSource::Project
            };

            registry.scan_directory(search_path, source);
        }

        registry
    }

    fn scan_directory(&mut self, dir: &Path, source: SkillSource) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let skill_md = path.join("SKILL.md");
                if skill_md.exists() {
                    if let Ok(metadata) = parse_skill_metadata(&skill_md, &path, source.clone()) {
                        self.skills.insert(metadata.name.clone(), metadata);
                    }
                }
            } else if path.extension().map(|e| e == "md").unwrap_or(false) {
                // Single-file skill (name.md)
                if let Ok(metadata) = parse_skill_metadata(&path, &path.parent().unwrap_or(dir).to_path_buf(), source.clone()) {
                    self.skills.insert(metadata.name.clone(), metadata);
                }
            }
        }
    }

    /// Generate the available_skills section for the system prompt (Tier 1).
    pub fn prompt_section(&self) -> String {
        if self.skills.is_empty() {
            return String::new();
        }

        let mut section = String::from("\n## Available Skills\n");
        section.push_str("Load a skill by name when it's relevant to the task. ");
        section.push_str("Use /skill-name to invoke a skill.\n\n");

        for (name, meta) in &self.skills {
            section.push_str(&format!("- **{}**: {}\n", name, meta.description));
        }

        section
    }

    /// Load a skill's full body (Tier 2). Cached after first load.
    pub fn load(&mut self, name: &str) -> Result<&LoadedSkill, SkillError> {
        if self.loaded_cache.contains_key(name) {
            return Ok(&self.loaded_cache[name]);
        }

        let metadata = self
            .skills
            .get(name)
            .ok_or_else(|| SkillError::NotFound(name.to_string()))?
            .clone();

        let skill_path = if metadata.path.is_dir() {
            metadata.path.join("SKILL.md")
        } else {
            metadata.path.clone()
        };

        let body = std::fs::read_to_string(&skill_path)?;
        // Strip YAML frontmatter from body
        let body = strip_frontmatter(&body);

        // Discover supporting files in the skill directory
        let skill_dir = if metadata.path.is_dir() {
            &metadata.path
        } else {
            metadata.path.parent().unwrap_or(Path::new("."))
        };

        let mut supporting_files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(skill_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file() && p.file_name().map(|n| n != "SKILL.md").unwrap_or(false) {
                    let ref_name = p
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    supporting_files.push((ref_name, p));
                }
            }
        }

        let loaded = LoadedSkill {
            metadata,
            body,
            supporting_files,
        };

        self.loaded_cache.insert(name.to_string(), loaded);
        Ok(&self.loaded_cache[name])
    }

    /// Expand $ARGUMENTS placeholder in skill body.
    pub fn expand_arguments(body: &str, args: &str) -> String {
        body.replace("$ARGUMENTS", args)
            .replace("${ARGUMENTS}", args)
    }

    /// Get skill by name.
    pub fn get(&self, name: &str) -> Option<&SkillMetadata> {
        self.skills.get(name)
    }

    /// Get all skill names.
    pub fn skill_names(&self) -> Vec<&str> {
        self.skills.keys().map(|s| s.as_str()).collect()
    }

    /// Check if a skill exists.
    pub fn has_skill(&self, name: &str) -> bool {
        self.skills.contains_key(name)
    }

    pub fn count(&self) -> usize {
        self.skills.len()
    }

    /// List all registered skill names.
    pub fn list(&self) -> Vec<&str> {
        self.skills.keys().map(|s| s.as_str()).collect()
    }
}

/// Parse SKILL.md frontmatter to extract metadata.
fn parse_skill_metadata(
    file_path: &Path,
    skill_dir: &Path,
    source: SkillSource,
) -> Result<SkillMetadata, SkillError> {
    let content = std::fs::read_to_string(file_path)?;

    let (frontmatter, description) = extract_frontmatter(&content);

    // Derive skill name from directory or file name
    let name = if skill_dir.is_dir() {
        skill_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    } else {
        file_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    };

    Ok(SkillMetadata {
        name,
        description,
        path: skill_dir.to_path_buf(),
        source,
        frontmatter,
    })
}

/// Extract YAML frontmatter from markdown content.
fn extract_frontmatter(content: &str) -> (SkillFrontmatter, String) {
    let mut fm = SkillFrontmatter {
        user_invocable: true,
        ..Default::default()
    };

    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        // No frontmatter — use first non-empty line as description
        let desc = content
            .lines()
            .find(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .unwrap_or("")
            .trim()
            .to_string();
        return (fm, desc);
    }

    // Find closing ---
    let after_first = &trimmed[3..];
    if let Some(end) = after_first.find("\n---") {
        let yaml_block = &after_first[..end];

        // Simple key-value parsing (avoid pulling in a full YAML parser)
        for line in yaml_block.lines() {
            let line = line.trim();
            if let Some(val) = line.strip_prefix("description:") {
                let desc = val.trim().trim_matches('"').trim_matches('\'');
                // Use the frontmatter description
                let remaining = &after_first[end + 4..];
                parse_fm_fields(yaml_block, &mut fm);
                return (fm, desc.to_string());
            }
        }

        parse_fm_fields(yaml_block, &mut fm);
        let remaining = &after_first[end + 4..];
        let desc = remaining
            .lines()
            .find(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .unwrap_or("")
            .trim()
            .to_string();
        return (fm, desc);
    }

    let desc = content
        .lines()
        .find(|l| !l.trim().is_empty() && !l.starts_with('#') && !l.starts_with("---"))
        .unwrap_or("")
        .trim()
        .to_string();
    (fm, desc)
}

fn parse_fm_fields(yaml: &str, fm: &mut SkillFrontmatter) {
    for line in yaml.lines() {
        let line = line.trim();
        if line.starts_with("disable_model_invocation:") {
            fm.disable_model_invocation = line.contains("true");
        } else if line.starts_with("user_invocable:") {
            fm.user_invocable = !line.contains("false");
        }
    }
}

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

fn dirs_next_home() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}
