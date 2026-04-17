use crate::frontmatter::SkillMetadata;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::collections::BTreeMap;
use std::path::Path;

/// Holds skills with `paths:` declarations, promoting them to active on file-touch match.
/// Activation is monotone: once promoted, a skill stays active for the session.
pub struct ConditionalRegistry {
    /// Dormant skills awaiting activation.
    dormant: BTreeMap<String, ConditionalEntry>,
    /// Skills that have been activated this session.
    activated: BTreeMap<String, SkillMetadata>,
}

struct ConditionalEntry {
    metadata: SkillMetadata,
    matcher: Gitignore,
}

impl ConditionalRegistry {
    /// Build from skills drained out of the main registry (those with `paths:` declared).
    pub fn new(conditional_skills: Vec<SkillMetadata>) -> Self {
        let mut dormant = BTreeMap::new();

        for meta in conditional_skills {
            let patterns = match &meta.frontmatter.paths {
                Some(p) if !p.is_empty() => p,
                _ => continue,
            };

            // Build an ignore-style matcher from the paths patterns.
            // Patterns are evaluated relative to the working directory.
            let mut builder = GitignoreBuilder::new("");
            for pattern in patterns {
                builder.add_line(None, pattern).ok();
            }

            match builder.build() {
                Ok(matcher) => {
                    dormant.insert(
                        meta.name.clone(),
                        ConditionalEntry {
                            metadata: meta,
                            matcher,
                        },
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to compile path patterns for skill '{}': {}",
                        meta.name,
                        e
                    );
                }
            }
        }

        Self {
            dormant,
            activated: BTreeMap::new(),
        }
    }

    /// Empty conditional registry (when no conditional skills exist).
    pub fn empty() -> Self {
        Self {
            dormant: BTreeMap::new(),
            activated: BTreeMap::new(),
        }
    }

    /// Check a set of file paths against dormant skills.
    /// Newly activated skills are moved from dormant to activated (monotone — no deactivation).
    /// Returns the names of skills activated by this call.
    pub fn activate_for_paths(&mut self, file_paths: &[&Path], cwd: &Path) -> Vec<String> {
        let mut newly_activated = Vec::new();

        // Collect names to activate to avoid borrow conflict
        let to_activate: Vec<String> = self
            .dormant
            .iter()
            .filter(|(_, entry)| {
                file_paths.iter().any(|fp| {
                    let rel = fp.strip_prefix(cwd).unwrap_or(fp);
                    entry
                        .matcher
                        .matched_path_or_any_parents(rel, fp.is_dir())
                        .is_ignore()
                })
            })
            .map(|(name, _)| name.clone())
            .collect();

        for name in to_activate {
            if let Some(entry) = self.dormant.remove(&name) {
                tracing::info!("Activating conditional skill: {}", name);
                newly_activated.push(name.clone());
                self.activated.insert(name, entry.metadata);
            }
        }

        newly_activated
    }

    /// Get the active conditional skills for inclusion in prompts.
    pub fn active_skills(&self) -> impl Iterator<Item = (&str, &SkillMetadata)> {
        self.activated.iter().map(|(n, m)| (n.as_str(), m))
    }

    /// Number of dormant (not yet activated) conditional skills.
    pub fn dormant_count(&self) -> usize {
        self.dormant.len()
    }

    /// Number of activated conditional skills.
    pub fn active_count(&self) -> usize {
        self.activated.len()
    }

    /// Total conditional skills (dormant + active).
    pub fn total_count(&self) -> usize {
        self.dormant.len() + self.activated.len()
    }

    /// Check if a specific skill is active.
    pub fn is_active(&self, name: &str) -> bool {
        self.activated.contains_key(name)
    }

    /// Get an activated skill by name.
    pub fn get_active(&self, name: &str) -> Option<&SkillMetadata> {
        self.activated.get(name)
    }
}
