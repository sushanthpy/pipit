use crate::SkillError;
use crate::frontmatter::{
    AgentConfig, HookDeclaration, SkillFrontmatter, SkillMetadata, SkillSource,
};
use crate::loader::LoadedSkill;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Maximum chars per skill description in the prompt.
const MAX_DESCRIPTION_CHARS: usize = 250;
/// Default prompt budget when no context_window is supplied (conservative).
const DEFAULT_PROMPT_BUDGET_CHARS: usize = 3200;

// ── YAML intermediary structs (serde_yaml_ng) ────────────────────────────

/// Intermediary for serde_yaml_ng deserialization.
/// Supports both snake_case and kebab-case keys for cross-tool compatibility.
#[derive(Deserialize, Default)]
#[serde(default)]
struct RawFrontmatter {
    description: Option<String>,
    #[serde(alias = "disable-model-invocation")]
    disable_model_invocation: Option<bool>,
    #[serde(alias = "user-invocable")]
    user_invocable: Option<bool>,
    #[serde(alias = "allowed-tools")]
    allowed_tools: Option<Vec<String>>,
    agent: Option<RawAgentConfig>,
    paths: Option<Vec<String>>,
    #[serde(alias = "when-to-use")]
    when_to_use: Option<String>,
    #[serde(alias = "argument-hint")]
    argument_hint: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    hooks: Option<Vec<RawHookDeclaration>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawAgentConfig {
    model: Option<String>,
    max_turns: Option<u32>,
}

#[derive(Deserialize)]
struct RawHookDeclaration {
    event: String,
    command: String,
}

// ── Registry ─────────────────────────────────────────────────────────────

/// Skill registry — discovers, deduplicates, and indexes all available skills.
pub struct SkillRegistry {
    skills: BTreeMap<String, SkillMetadata>,
    loaded_cache: HashMap<String, LoadedSkill>,
    /// Canonical path → (name, source) of the skill already registered.
    /// Used for source-tier dedup: lower SkillSource wins.
    seen_canonical: HashMap<PathBuf, (String, SkillSource)>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            skills: BTreeMap::new(),
            loaded_cache: HashMap::new(),
            seen_canonical: HashMap::new(),
        }
    }

    /// Discover skills from search paths. All paths default to `Project` source.
    /// For explicit source control, use `discover_with_sources`.
    pub fn discover(search_paths: &[PathBuf]) -> Self {
        let pairs: Vec<_> = search_paths
            .iter()
            .map(|p| (p.clone(), SkillSource::Project))
            .collect();
        Self::discover_with_sources(&pairs)
    }

    /// Discover skills with explicit source classification per search path.
    pub fn discover_with_sources(search_paths: &[(PathBuf, SkillSource)]) -> Self {
        let mut registry = Self::new();

        for (search_path, source) in search_paths {
            if !search_path.exists() {
                continue;
            }
            registry.scan_directory(search_path, source.clone(), None);
        }

        registry
    }

    /// Recursively scan a directory for skills.
    /// `namespace` tracks parent path segments for hierarchical naming (e.g., "team:review").
    fn scan_directory(&mut self, dir: &Path, source: SkillSource, namespace: Option<&str>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("Failed to read skill directory {}: {}", dir.display(), e);
                return;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let skill_md = path.join("SKILL.md");
                if skill_md.exists() {
                    self.register_skill(&skill_md, &path, source.clone(), namespace);
                } else {
                    // No SKILL.md — recurse with namespace prefix
                    let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
                    let child_ns = match namespace {
                        Some(ns) => format!("{}:{}", ns, dir_name),
                        None => dir_name.to_string(),
                    };
                    self.scan_directory(&path, source.clone(), Some(&child_ns));
                }
            } else if path.extension().map(|e| e == "md").unwrap_or(false) {
                let parent = path.parent().unwrap_or(dir);
                self.register_skill(&path, &parent.to_path_buf(), source.clone(), namespace);
            }
        }
    }

    /// Register a single skill, deduplicating by canonical path.
    /// On canonical collision: lower SkillSource (more trusted) wins.
    fn register_skill(
        &mut self,
        file_path: &Path,
        skill_dir: &Path,
        source: SkillSource,
        namespace: Option<&str>,
    ) {
        // Resolve canonical path for dedup
        let canonical = match file_path.canonicalize() {
            Ok(p) => p,
            Err(_) => file_path.to_path_buf(),
        };

        // Source-tier dedup: if we've seen this canonical path, only replace if
        // the new source is strictly more trusted (lower Ord value).
        if let Some((existing_name, existing_source)) = self.seen_canonical.get(&canonical) {
            if source >= *existing_source {
                tracing::debug!(
                    "Skipping duplicate skill at {} (canonical: {}) — existing '{}' from {:?} takes precedence over {:?}",
                    file_path.display(),
                    canonical.display(),
                    existing_name,
                    existing_source,
                    source,
                );
                return;
            }
            // New source is more trusted — remove the old entry so we can replace it
            let old_name = existing_name.clone();
            tracing::info!(
                "Replacing skill '{}' from {:?} with more-trusted source {:?} at {}",
                old_name,
                existing_source,
                source,
                file_path.display(),
            );
            self.skills.remove(&old_name);
        }

        match parse_skill_metadata(file_path, skill_dir, source.clone(), namespace) {
            Ok(metadata) => {
                if let Some(existing) = self.skills.get(&metadata.name) {
                    tracing::info!(
                        "Skill name '{}' from {:?} overrides existing from {:?} (different paths)",
                        metadata.name,
                        source,
                        existing.source,
                    );
                }
                let name = metadata.name.clone();
                self.seen_canonical
                    .insert(canonical, (name.clone(), source));
                self.skills.insert(name, metadata);
            }
            Err(e) => {
                tracing::warn!("Failed to parse skill at {}: {}", file_path.display(), e);
            }
        }
    }

    /// Generate the available_skills section for the system prompt (Tier 1).
    ///
    /// Budget-aware: capped at ~1% of `context_window` (in chars, estimated at 4 chars/token).
    /// Falls back to names-only when descriptions don't fit.
    /// Iteration is deterministic (BTreeMap sorted by name).
    pub fn prompt_section(&self) -> String {
        self.prompt_section_with_budget(DEFAULT_PROMPT_BUDGET_CHARS)
    }

    /// Budget-parameterized variant for callers that know the context window.
    /// `budget_chars` controls the maximum size of the skill listing.
    pub fn prompt_section_with_budget(&self, budget_chars: usize) -> String {
        if self.skills.is_empty() {
            return String::new();
        }

        let header = "\n## Available Skills\n\
                       Load a skill by name when it's relevant to the task. \
                       Use /skill-name to invoke a skill.\n\n";

        // First pass: full descriptions (capped at MAX_DESCRIPTION_CHARS each)
        let mut section = String::from(header);
        let mut over_budget = false;
        for (name, meta) in &self.skills {
            if meta.frontmatter.disable_model_invocation {
                continue;
            }
            // Conditional skills are handled separately by ConditionalRegistry
            if meta.is_conditional() {
                continue;
            }
            let desc = truncate_description(&meta.description, MAX_DESCRIPTION_CHARS);
            let entry = format!("- **{}**: {}\n", name, desc);
            if section.len() + entry.len() > budget_chars {
                over_budget = true;
                break;
            }
            section.push_str(&entry);
        }

        if !over_budget {
            return section;
        }

        // Over budget — fall back to names only
        section = String::from(header);
        for (name, meta) in &self.skills {
            if meta.frontmatter.disable_model_invocation || meta.is_conditional() {
                continue;
            }
            let entry = format!("- {}\n", name);
            if section.len() + entry.len() > budget_chars {
                section.push_str("- ...(truncated)\n");
                break;
            }
            section.push_str(&entry);
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

    /// Merge another registry into this one (used by DynamicDiscovery).
    /// Existing entries are not overwritten — the original discovery takes precedence.
    pub fn merge(&mut self, other: SkillRegistry) {
        for (name, meta) in other.skills {
            self.skills.entry(name).or_insert(meta);
        }
        for (canon, entry) in other.seen_canonical {
            self.seen_canonical.entry(canon).or_insert(entry);
        }
    }

    /// Get all conditional skills (those with `paths:` declared).
    pub fn drain_conditional(&mut self) -> Vec<SkillMetadata> {
        let conditional_names: Vec<String> = self
            .skills
            .iter()
            .filter(|(_, m)| m.is_conditional())
            .map(|(n, _)| n.clone())
            .collect();

        conditional_names
            .into_iter()
            .filter_map(|n| self.skills.remove(&n))
            .collect()
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

// ── Frontmatter parsing ─────────────────────────────────────────────────

/// Parse SKILL.md frontmatter to extract metadata.
fn parse_skill_metadata(
    file_path: &Path,
    skill_dir: &Path,
    source: SkillSource,
    namespace: Option<&str>,
) -> Result<SkillMetadata, SkillError> {
    let content = std::fs::read_to_string(file_path)?;

    let (frontmatter, description) = extract_frontmatter(&content, file_path)?;

    // Derive skill name with optional namespace prefix for nested dirs
    let leaf = if skill_dir.is_dir() {
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

    let name = match namespace {
        Some(ns) => format!("{}:{}", ns, leaf),
        None => leaf,
    };

    // #14: No-fallback description policy — if frontmatter didn't provide one,
    // use a neutral synthetic label. Never promote body prose into the listing.
    let safe_description = if description.is_empty() {
        format!("[unnamed skill: {}]", name)
    } else {
        description
    };

    Ok(SkillMetadata {
        name,
        description: safe_description,
        path: skill_dir.to_path_buf(),
        source,
        frontmatter,
    })
}

/// Extract and parse YAML frontmatter using serde_yaml_ng.
/// Returns typed error with file path on parse failure instead of silent fallback.
fn extract_frontmatter(
    content: &str,
    file_path: &Path,
) -> Result<(SkillFrontmatter, String), SkillError> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        // No frontmatter block — skill is valid but has no metadata.
        // Description left empty; caller applies the no-fallback policy.
        return Ok((default_frontmatter(), String::new()));
    }

    let after_first = &trimmed[3..];
    let Some(end) = after_first.find("\n---") else {
        // Unclosed frontmatter — treat as parse error
        return Err(SkillError::FrontmatterParse {
            path: file_path.to_path_buf(),
            detail: "unclosed frontmatter block (missing closing ---)".to_string(),
        });
    };

    let yaml_block = after_first[..end].trim();
    let _remaining = &after_first[end + 4..];

    // Parse YAML via serde_yaml_ng — single-pass, populates all fields
    let raw: RawFrontmatter = serde_yaml_ng::from_str(yaml_block).map_err(|e| {
        SkillError::FrontmatterParse {
            path: file_path.to_path_buf(),
            detail: e.to_string(),
        }
    })?;

    let fm = SkillFrontmatter {
        disable_model_invocation: raw.disable_model_invocation.unwrap_or(false),
        user_invocable: raw.user_invocable.unwrap_or(true),
        allowed_tools: raw.allowed_tools,
        agent: raw.agent.map(|a| AgentConfig {
            model: a.model,
            max_turns: a.max_turns,
        }),
        paths: raw.paths,
        when_to_use: raw.when_to_use,
        argument_hint: raw.argument_hint,
        model: raw.model,
        effort: raw.effort,
        hooks: raw.hooks.map(|hv| {
            hv.into_iter()
                .map(|h| HookDeclaration {
                    event: h.event,
                    command: h.command,
                })
                .collect()
        }),
    };

    // Description comes from frontmatter YAML only — not from body text (#14).
    let description = raw.description.unwrap_or_default();

    Ok((fm, description))
}

fn default_frontmatter() -> SkillFrontmatter {
    SkillFrontmatter {
        user_invocable: true,
        ..Default::default()
    }
}

/// Truncate a description to fit within the prompt budget, preserving word boundaries.
fn truncate_description(desc: &str, max_chars: usize) -> &str {
    if desc.len() <= max_chars {
        return desc;
    }
    match desc[..max_chars].rfind(' ') {
        Some(pos) => &desc[..pos],
        None => &desc[..max_chars],
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
