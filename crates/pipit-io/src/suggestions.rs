//! T11: Prompt suggestion engine — trie-based completion for slash commands,
//! file paths, and recent inputs.
//!
//! Provides a unified completion source that the Composer widget can query.
//! The trie stores command names and descriptions; file path completion
//! delegates to the filesystem.

use std::collections::BTreeMap;

/// A single completion suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// The text to insert (e.g. "/plan", "@src/main.rs").
    pub text: String,
    /// Short description shown in the popup.
    pub description: String,
    /// Category for grouping/coloring.
    pub kind: SuggestionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionKind {
    SlashCommand,
    FilePath,
    RecentInput,
    ShellHistory,
}

/// Trie node for prefix lookup.
#[derive(Debug, Clone, Default)]
struct TrieNode {
    children: BTreeMap<char, TrieNode>,
    /// If this node marks the end of an entry, its description.
    entry: Option<String>,
}

/// Prompt suggestion engine backed by a trie for O(k) prefix lookups
/// where k = prefix length.
#[derive(Debug, Clone)]
pub struct SuggestionEngine {
    /// Trie for slash commands.
    commands: TrieNode,
    /// Recent inputs (ring buffer, most recent last).
    recent: Vec<String>,
    recent_capacity: usize,
}

impl Default for SuggestionEngine {
    fn default() -> Self {
        let mut engine = Self {
            commands: TrieNode::default(),
            recent: Vec::new(),
            recent_capacity: 50,
        };
        engine.register_builtin_commands();
        engine
    }
}

impl SuggestionEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a slash command with its description.
    pub fn register_command(&mut self, name: &str, description: &str) {
        let mut node = &mut self.commands;
        for ch in name.chars() {
            node = node.children.entry(ch).or_default();
        }
        node.entry = Some(description.to_string());
    }

    /// Add a recent input to the history.
    pub fn add_recent(&mut self, input: &str) {
        if input.is_empty() {
            return;
        }
        // Remove duplicates
        self.recent.retain(|r| r != input);
        self.recent.push(input.to_string());
        if self.recent.len() > self.recent_capacity {
            self.recent.remove(0);
        }
    }

    /// Query suggestions for the given input prefix.
    ///
    /// Routes to the appropriate completion source based on prefix:
    /// - `/` → slash commands
    /// - `@` → file paths (stub; real impl needs workspace root)
    /// - `!` → shell history placeholder
    /// - otherwise → recent inputs
    pub fn suggest(&self, prefix: &str, max: usize) -> Vec<Suggestion> {
        if prefix.starts_with('/') {
            self.suggest_commands(prefix, max)
        } else if prefix.starts_with('@') {
            self.suggest_files(&prefix[1..], max)
        } else if prefix.starts_with('!') {
            // Shell history — placeholder
            Vec::new()
        } else {
            self.suggest_recent(prefix, max)
        }
    }

    /// Suggest slash commands matching the prefix.
    fn suggest_commands(&self, prefix: &str, max: usize) -> Vec<Suggestion> {
        let mut results = Vec::new();
        // Walk the trie to the prefix node
        let mut node = &self.commands;
        for ch in prefix.chars() {
            match node.children.get(&ch) {
                Some(child) => node = child,
                None => return results,
            }
        }
        // Collect all entries under this prefix
        let mut stack: Vec<(String, &TrieNode)> = vec![(prefix.to_string(), node)];
        while let Some((path, n)) = stack.pop() {
            if results.len() >= max {
                break;
            }
            if let Some(desc) = &n.entry {
                results.push(Suggestion {
                    text: path.clone(),
                    description: desc.clone(),
                    kind: SuggestionKind::SlashCommand,
                });
            }
            for (&ch, child) in &n.children {
                stack.push((format!("{path}{ch}"), child));
            }
        }
        results.sort_by(|a, b| a.text.cmp(&b.text));
        results.truncate(max);
        results
    }

    /// Suggest file paths (basic prefix match on known paths).
    fn suggest_files(&self, prefix: &str, max: usize) -> Vec<Suggestion> {
        // In a real implementation, this would scan the workspace.
        // For now, return empty — the composer already has tab completion.
        let _ = (prefix, max);
        Vec::new()
    }

    /// Suggest from recent inputs.
    fn suggest_recent(&self, prefix: &str, max: usize) -> Vec<Suggestion> {
        let lower = prefix.to_lowercase();
        self.recent
            .iter()
            .rev()
            .filter(|r| r.to_lowercase().starts_with(&lower))
            .take(max)
            .map(|r| Suggestion {
                text: r.clone(),
                description: "recent".into(),
                kind: SuggestionKind::RecentInput,
            })
            .collect()
    }

    /// Register the built-in slash commands.
    fn register_builtin_commands(&mut self) {
        let commands = [
            ("/help", "Show available commands"),
            ("/plan", "Plan before editing"),
            ("/edit", "Edit a file"),
            ("/run", "Run a shell command"),
            ("/test", "Run tests"),
            ("/undo", "Undo last change"),
            ("/diff", "Show diff of changes"),
            ("/commit", "Commit changes"),
            ("/clear", "Clear conversation"),
            ("/model", "Switch model"),
            ("/compact", "Compact conversation"),
            ("/config", "Show configuration"),
            ("/exit", "Exit pipit"),
            ("/tools", "List available tools"),
            ("/mcp", "Manage MCP servers"),
            ("/voice", "Toggle voice input"),
            ("/bug", "Report a bug"),
            ("/cost", "Show session cost"),
            ("/tokens", "Show token usage"),
            ("/context", "Manage context files"),
        ];
        for (name, desc) in commands {
            self.register_command(name, desc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_commands_registered() {
        let engine = SuggestionEngine::new();
        let results = engine.suggest("/", 50);
        assert!(results.len() >= 15, "expected ≥15 commands, got {}", results.len());
    }

    #[test]
    fn prefix_narrows_results() {
        let engine = SuggestionEngine::new();
        let all = engine.suggest("/", 100);
        let plan = engine.suggest("/pl", 100);
        assert!(plan.len() < all.len());
        assert!(plan.iter().any(|s| s.text == "/plan"));
    }

    #[test]
    fn recent_inputs() {
        let mut engine = SuggestionEngine::new();
        engine.add_recent("fix the panic on line 42");
        engine.add_recent("refactor the auth module");
        let results = engine.suggest("fix", 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, SuggestionKind::RecentInput);
    }

    #[test]
    fn recent_deduplicates() {
        let mut engine = SuggestionEngine::new();
        engine.add_recent("hello");
        engine.add_recent("world");
        engine.add_recent("hello");
        assert_eq!(engine.recent.len(), 2);
    }

    #[test]
    fn max_limits_results() {
        let engine = SuggestionEngine::new();
        let results = engine.suggest("/", 3);
        assert!(results.len() <= 3);
    }
}
