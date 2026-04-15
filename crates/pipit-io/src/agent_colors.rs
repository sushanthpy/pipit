//! T6: Agent color identity manager.
//!
//! Assigns each agent a unique, persistent color from the semantic
//! theme's 8-color palette. Uses round-robin assignment with an
//! optional name→index map for stable colors across sessions.

use ratatui::style::Color;
use std::collections::HashMap;

use crate::theme::SemanticTheme;

/// Manages color assignment for multiple agents.
#[derive(Debug, Clone)]
pub struct AgentColorManager {
    /// Agent name → palette index.
    assignments: HashMap<String, usize>,
    /// Next index to assign.
    next_index: usize,
}

impl Default for AgentColorManager {
    fn default() -> Self {
        Self {
            assignments: HashMap::new(),
            next_index: 0,
        }
    }
}

impl AgentColorManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get (or assign) a color for the given agent name.
    pub fn color_for(&mut self, agent_name: &str, theme: &SemanticTheme) -> Color {
        let idx = self.index_for(agent_name);
        theme.agent_colors[idx % theme.agent_colors.len()]
    }

    /// Get the palette index for an agent (assigns if needed).
    pub fn index_for(&mut self, agent_name: &str) -> usize {
        if let Some(&idx) = self.assignments.get(agent_name) {
            return idx;
        }
        let idx = self.next_index % 8;
        self.assignments.insert(agent_name.to_string(), idx);
        self.next_index += 1;
        idx
    }

    /// Check if an agent already has an assigned color.
    pub fn has_assignment(&self, agent_name: &str) -> bool {
        self.assignments.contains_key(agent_name)
    }

    /// Force a specific index for an agent (for persistence/config).
    pub fn set_index(&mut self, agent_name: &str, idx: usize) {
        self.assignments.insert(agent_name.to_string(), idx);
    }

    /// Get all current assignments.
    pub fn assignments(&self) -> &HashMap<String, usize> {
        &self.assignments
    }

    /// Number of assigned agents.
    pub fn count(&self) -> usize {
        self.assignments.len()
    }

    /// Clear all assignments.
    pub fn reset(&mut self) {
        self.assignments.clear();
        self.next_index = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assigns_unique_indices() {
        let theme = SemanticTheme::dark();
        let mut mgr = AgentColorManager::new();
        let c1 = mgr.color_for("agent-a", &theme);
        let c2 = mgr.color_for("agent-b", &theme);
        assert_ne!(c1, c2);
    }

    #[test]
    fn same_agent_same_color() {
        let theme = SemanticTheme::dark();
        let mut mgr = AgentColorManager::new();
        let c1 = mgr.color_for("agent-x", &theme);
        let c2 = mgr.color_for("agent-x", &theme);
        assert_eq!(c1, c2);
    }

    #[test]
    fn wraps_around_at_8() {
        let mut mgr = AgentColorManager::new();
        for i in 0..10 {
            mgr.index_for(&format!("agent-{i}"));
        }
        // Agent 0 and agent 8 should share the same index (mod 8)
        assert_eq!(
            mgr.index_for("agent-0"),
            mgr.index_for("agent-0")
        );
        assert_eq!(mgr.count(), 10);
    }

    #[test]
    fn manual_assignment() {
        let theme = SemanticTheme::dark();
        let mut mgr = AgentColorManager::new();
        mgr.set_index("special", 3);
        assert_eq!(mgr.color_for("special", &theme), theme.agent_colors[3]);
    }

    #[test]
    fn reset_clears() {
        let mut mgr = AgentColorManager::new();
        mgr.index_for("a");
        mgr.reset();
        assert_eq!(mgr.count(), 0);
    }
}
