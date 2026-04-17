//! Task #2: Rule activation via path-prefix trie.
//!
//! Reuses the same trie-based activation semantics as `skill_activation.rs`
//! but for rules. Scope-based precedence: inner directories override outer.

use crate::rule::{Rule, RuleId};
use pipit_core::skill_activation::ActivationScope;
use std::collections::HashMap;

/// A trie node for prefix-based rule lookup by path.
struct TrieNode {
    rules: Vec<(RuleId, ActivationScope)>,
    children: HashMap<String, TrieNode>,
}

impl TrieNode {
    fn new() -> Self {
        Self {
            rules: Vec::new(),
            children: HashMap::new(),
        }
    }
}

/// Index for path-conditional rule activation.
/// Mirrors `SkillActivationIndex` but for rules.
pub struct RuleActivationIndex {
    trie: TrieNode,
    language_index: HashMap<String, Vec<RuleId>>,
    global_rules: Vec<RuleId>,
}

impl RuleActivationIndex {
    pub fn new() -> Self {
        Self {
            trie: TrieNode::new(),
            language_index: HashMap::new(),
            global_rules: Vec::new(),
        }
    }

    /// Add a rule to the activation index.
    pub fn add_rule(&mut self, rule: &Rule) {
        let id = rule.id.clone();

        for pattern in &rule.path_patterns {
            if pattern == "*" || pattern == "**" {
                self.global_rules.push(id.clone());
            } else {
                self.insert_path_pattern(pattern, &id, rule.scope);
            }
        }

        for lang in &rule.language_patterns {
            self.language_index
                .entry(lang.to_lowercase())
                .or_default()
                .push(id.clone());
        }
    }

    fn insert_path_pattern(&mut self, pattern: &str, rule_id: &RuleId, scope: ActivationScope) {
        let segments: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
        let mut node = &mut self.trie;
        for seg in &segments {
            node = node
                .children
                .entry(seg.to_string())
                .or_insert_with(TrieNode::new);
        }
        node.rules.push((rule_id.clone(), scope));
    }

    /// Find all rules that should activate for the given touched files.
    /// Returns RuleIds sorted by precedence (highest first).
    pub fn activate(&self, touched_files: &[&str], languages: &[&str]) -> Vec<RuleId> {
        let mut activated: HashMap<RuleId, u32> = HashMap::new();

        // Global rules always activate.
        for id in &self.global_rules {
            activated.insert(id.clone(), 0);
        }

        // Walk trie for each touched file.
        for file in touched_files {
            let matches = self.lookup_path(file);
            for (rule_id, scope) in matches {
                let prec = scope.precedence();
                let entry = activated.entry(rule_id).or_insert(0);
                *entry = (*entry).max(prec);
            }
        }

        // Language-based activation.
        for lang in languages {
            if let Some(rule_ids) = self.language_index.get(&lang.to_lowercase()) {
                for id in rule_ids {
                    activated.entry(id.clone()).or_insert(1);
                }
            }
        }

        let mut result: Vec<(RuleId, u32)> = activated.into_iter().collect();
        result.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        result.into_iter().map(|(id, _)| id).collect()
    }

    fn lookup_path(&self, path: &str) -> Vec<(RuleId, ActivationScope)> {
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut results = Vec::new();
        let mut node = &self.trie;

        // Collect rules at each level of the trie traversal.
        results.extend(node.rules.iter().cloned());

        for seg in &segments {
            match node.children.get(*seg) {
                Some(child) => {
                    node = child;
                    results.extend(node.rules.iter().cloned());
                }
                None => break,
            }
        }

        results
    }

    /// Number of indexed rules.
    pub fn rule_count(&self) -> usize {
        self.global_rules.len() + self.count_trie_rules(&self.trie)
    }

    fn count_trie_rules(&self, node: &TrieNode) -> usize {
        let mut count = node.rules.len();
        for child in node.children.values() {
            count += self.count_trie_rules(child);
        }
        count
    }
}

impl Default for RuleActivationIndex {
    fn default() -> Self {
        Self::new()
    }
}
