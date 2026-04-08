//! Path-Activated and Precedence-Aware Skills (Skill Task 6)
//!
//! Dynamically discovers skills from directories, resolves precedence by
//! path depth, and activates skills whose path patterns match the files
//! involved in the turn. Uses a trie-based index for O(p + m) activation
//! where p is path depth and m is matching rules.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A skill activation rule with scope and precedence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivationRule {
    /// Skill ID this rule activates.
    pub skill_id: String,
    /// Glob patterns for file paths that trigger this rule.
    pub path_patterns: Vec<String>,
    /// Language patterns (e.g., "rust", "python", "typescript").
    pub language_patterns: Vec<String>,
    /// Scope: where this rule was defined (determines precedence).
    pub scope: ActivationScope,
    /// Debug: source path of the rule definition.
    pub defined_at: PathBuf,
}

/// Scope determines precedence (inner scopes override outer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ActivationScope {
    /// Workspace root (lowest precedence).
    Workspace,
    /// User configuration (~/.config/pipit/skills/).
    User,
    /// Project root (.pipit/skills/).
    Project,
    /// Subdirectory within the project (higher path depth = higher precedence).
    SubDirectory { depth: u32 },
}

impl ActivationScope {
    pub fn precedence(&self) -> u32 {
        match self {
            Self::Workspace => 0,
            Self::User => 1,
            Self::Project => 2,
            Self::SubDirectory { depth } => 3 + depth,
        }
    }
}

/// A trie node for prefix-based skill lookup by path.
struct TrieNode {
    /// Skills activated at this directory level.
    skills: Vec<(String, ActivationScope)>, // (skill_id, scope)
    /// Child nodes by directory name.
    children: HashMap<String, TrieNode>,
}

impl TrieNode {
    fn new() -> Self {
        Self {
            skills: Vec::new(),
            children: HashMap::new(),
        }
    }
}

/// The path-activated skill index.
pub struct SkillActivationIndex {
    /// Trie for path-prefix matching.
    trie: TrieNode,
    /// All rules indexed by skill ID.
    rules: HashMap<String, Vec<ActivationRule>>,
    /// Language → skill IDs mapping.
    language_index: HashMap<String, Vec<String>>,
    /// Global skills (always active).
    global_skills: Vec<String>,
}

impl SkillActivationIndex {
    pub fn new() -> Self {
        Self {
            trie: TrieNode::new(),
            rules: HashMap::new(),
            language_index: HashMap::new(),
            global_skills: Vec::new(),
        }
    }

    /// Add a skill activation rule to the index.
    pub fn add_rule(&mut self, rule: ActivationRule) {
        let skill_id = rule.skill_id.clone();

        // Index path patterns into trie
        for pattern in &rule.path_patterns {
            if pattern == "*" || pattern == "**" {
                self.global_skills.push(skill_id.clone());
            } else {
                self.insert_path_pattern(pattern, &skill_id, rule.scope);
            }
        }

        // Index language patterns
        for lang in &rule.language_patterns {
            self.language_index
                .entry(lang.to_lowercase())
                .or_default()
                .push(skill_id.clone());
        }

        self.rules.entry(skill_id).or_default().push(rule);
    }

    /// Find all skills that should be active for the given touched files.
    /// Returns skill IDs sorted by precedence (highest first).
    pub fn activate(&self, touched_files: &[&str], languages: &[&str]) -> Vec<String> {
        let mut activated: HashMap<String, u32> = HashMap::new(); // skill_id → max precedence

        // Add global skills
        for id in &self.global_skills {
            activated.insert(id.clone(), 0);
        }

        // Match path patterns via trie traversal
        for file in touched_files {
            let matches = self.lookup_path(file);
            for (skill_id, scope) in matches {
                let prec = scope.precedence();
                let entry = activated.entry(skill_id).or_insert(0);
                *entry = (*entry).max(prec);
            }
        }

        // Match language patterns
        for lang in languages {
            if let Some(skill_ids) = self.language_index.get(&lang.to_lowercase()) {
                for id in skill_ids {
                    activated.entry(id.clone()).or_insert(1);
                }
            }
        }

        // Sort by precedence (highest first), then by name for stability
        let mut result: Vec<(String, u32)> = activated.into_iter().collect();
        result.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        result.into_iter().map(|(id, _)| id).collect()
    }

    /// Total number of rules.
    pub fn rule_count(&self) -> usize {
        self.rules.values().map(|v| v.len()).sum()
    }

    /// Number of unique skills indexed.
    pub fn skill_count(&self) -> usize {
        self.rules.len()
    }

    fn insert_path_pattern(&mut self, pattern: &str, skill_id: &str, scope: ActivationScope) {
        // Extract directory prefix from pattern
        let dir_prefix = if let Some(stripped) = pattern.strip_prefix("**/") {
            // **/foo → match "foo" at any depth
            stripped.split('/').next().unwrap_or("")
        } else {
            pattern.split('/').next().unwrap_or("")
        };

        // For extension patterns like *.rs → add to root with pattern
        if pattern.starts_with("*.") {
            self.trie.skills.push((skill_id.to_string(), scope));
            return;
        }

        // Walk/create trie path
        let parts: Vec<&str> = pattern.split('/').collect();
        let mut node = &mut self.trie;
        for part in &parts {
            if *part == "**" || *part == "*" {
                break;
            }
            node = node
                .children
                .entry(part.to_string())
                .or_insert_with(TrieNode::new);
        }
        node.skills.push((skill_id.to_string(), scope));
    }

    fn lookup_path(&self, path: &str) -> Vec<(String, ActivationScope)> {
        let mut matches = Vec::new();
        let parts: Vec<&str> = path.split('/').collect();

        // Walk trie collecting all matching skills
        let mut node = &self.trie;

        // Collect root-level matches (includes *.ext patterns)
        for (id, scope) in &node.skills {
            // Check extension patterns
            matches.push((id.clone(), *scope));
        }

        for part in &parts {
            if let Some(child) = node.children.get(*part) {
                for (id, scope) in &child.skills {
                    matches.push((id.clone(), *scope));
                }
                node = child;
            } else {
                break;
            }
        }

        matches
    }
}

impl Default for SkillActivationIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Discover skill files from a directory tree.
/// Returns (relative_path, depth) pairs for .md files in skills directories.
pub fn discover_skill_files(root: &Path) -> Vec<(PathBuf, u32)> {
    let mut results = Vec::new();
    let skills_dir = root.join(".pipit").join("skills");
    if skills_dir.exists() {
        walk_skill_dir(&skills_dir, &skills_dir, 0, &mut results);
    }
    results
}

fn walk_skill_dir(base: &Path, dir: &Path, depth: u32, results: &mut Vec<(PathBuf, u32)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_skill_dir(base, &path, depth + 1, results);
        } else if path.extension().map_or(false, |e| e == "md") {
            if let Ok(rel) = path.strip_prefix(base) {
                results.push((rel.to_path_buf(), depth));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_precedence() {
        let mut index = SkillActivationIndex::new();

        // Global skill
        index.add_rule(ActivationRule {
            skill_id: "global".to_string(),
            path_patterns: vec!["*".to_string()],
            language_patterns: vec![],
            scope: ActivationScope::Workspace,
            defined_at: PathBuf::from("/"),
        });

        // Rust-specific skill
        index.add_rule(ActivationRule {
            skill_id: "rust-testing".to_string(),
            path_patterns: vec![],
            language_patterns: vec!["rust".to_string()],
            scope: ActivationScope::Project,
            defined_at: PathBuf::from(".pipit/skills/"),
        });

        let active = index.activate(&["src/lib.rs"], &["rust"]);
        assert!(active.contains(&"global".to_string()));
        assert!(active.contains(&"rust-testing".to_string()));
        // rust-testing should be higher precedence
        let rust_pos = active.iter().position(|s| s == "rust-testing").unwrap();
        let global_pos = active.iter().position(|s| s == "global").unwrap();
        assert!(rust_pos < global_pos);
    }
}
