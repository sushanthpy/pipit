//! UI Component Extension Protocol (Task 6).
//!
//! Defines a typed protocol for plugins/extensions to contribute UI components
//! to the TUI or Web surface. Components are declared descriptively and the
//! surface renderer is responsible for actual layout.
//!
//! This decouples plugin UI from rendering: a plugin emits `UiComponent` values,
//! and each surface (TUI, Web, VSCode) renders them with native widgets.

use serde::{Deserialize, Serialize};

/// A UI component contributed by a plugin or skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiComponent {
    /// Unique identifier for this component instance.
    pub id: String,
    /// Which plugin/skill contributed this component.
    pub source: String,
    /// Where to place it in the UI.
    pub slot: UiSlot,
    /// The component type and content.
    pub kind: UiComponentKind,
    /// Priority for ordering within a slot (lower = first).
    #[serde(default = "default_priority")]
    pub priority: u32,
}

fn default_priority() -> u32 {
    100
}

/// Available slots where components can be placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiSlot {
    /// Above the message list.
    Header,
    /// Below the message list, above the input box.
    Footer,
    /// Side panel (right).
    SidePanel,
    /// Inline within the message stream (after a specific event).
    Inline,
    /// Status bar area.
    StatusBar,
    /// Overlay/modal.
    Modal,
}

/// The kind of UI component.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiComponentKind {
    /// A text label.
    Label { text: String },
    /// A progress bar or gauge.
    Progress {
        label: String,
        value: f64,
        max: f64,
    },
    /// A table of key-value pairs.
    KeyValue { entries: Vec<(String, String)> },
    /// A data table with columns and rows.
    Table {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    /// A clickable action button.
    Button {
        label: String,
        action: String,
    },
    /// A notification/alert.
    Alert {
        severity: AlertSeverity,
        message: String,
    },
    /// Raw markdown content.
    Markdown { content: String },
    /// A chart/sparkline.
    Sparkline {
        label: String,
        data: Vec<f64>,
    },
}

/// Severity level for alerts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertSeverity {
    Info,
    Warning,
    Error,
    Success,
}

/// Registry of UI components contributed by plugins.
#[derive(Debug, Default)]
pub struct UiComponentRegistry {
    components: Vec<UiComponent>,
}

impl UiComponentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new UI component.
    pub fn register(&mut self, component: UiComponent) {
        self.components.push(component);
    }

    /// Remove all components from a specific source (plugin unload).
    pub fn remove_source(&mut self, source: &str) {
        self.components.retain(|c| c.source != source);
    }

    /// Get all components for a specific slot, sorted by priority.
    pub fn for_slot(&self, slot: UiSlot) -> Vec<&UiComponent> {
        let mut slot_components: Vec<&UiComponent> = self
            .components
            .iter()
            .filter(|c| c.slot == slot)
            .collect();
        slot_components.sort_by_key(|c| c.priority);
        slot_components
    }

    /// Get all registered components.
    pub fn all(&self) -> &[UiComponent] {
        &self.components
    }

    /// Total count.
    pub fn count(&self) -> usize {
        self.components.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_query() {
        let mut reg = UiComponentRegistry::new();
        reg.register(UiComponent {
            id: "cost-gauge".into(),
            source: "cost-plugin".into(),
            slot: UiSlot::StatusBar,
            kind: UiComponentKind::Progress {
                label: "Budget".into(),
                value: 0.42,
                max: 1.0,
            },
            priority: 10,
        });
        reg.register(UiComponent {
            id: "alert-1".into(),
            source: "security-plugin".into(),
            slot: UiSlot::Header,
            kind: UiComponentKind::Alert {
                severity: AlertSeverity::Warning,
                message: "Unverified tool detected".into(),
            },
            priority: 50,
        });

        assert_eq!(reg.count(), 2);
        assert_eq!(reg.for_slot(UiSlot::StatusBar).len(), 1);
        assert_eq!(reg.for_slot(UiSlot::Header).len(), 1);
        assert_eq!(reg.for_slot(UiSlot::Footer).len(), 0);
    }

    #[test]
    fn test_remove_source() {
        let mut reg = UiComponentRegistry::new();
        reg.register(UiComponent {
            id: "a".into(),
            source: "plugin-a".into(),
            slot: UiSlot::Footer,
            kind: UiComponentKind::Label { text: "hi".into() },
            priority: 100,
        });
        reg.register(UiComponent {
            id: "b".into(),
            source: "plugin-b".into(),
            slot: UiSlot::Footer,
            kind: UiComponentKind::Label { text: "bye".into() },
            priority: 100,
        });

        assert_eq!(reg.count(), 2);
        reg.remove_source("plugin-a");
        assert_eq!(reg.count(), 1);
        assert_eq!(reg.all()[0].source, "plugin-b");
    }

    #[test]
    fn test_priority_ordering() {
        let mut reg = UiComponentRegistry::new();
        reg.register(UiComponent {
            id: "low".into(),
            source: "p".into(),
            slot: UiSlot::SidePanel,
            kind: UiComponentKind::Label { text: "low".into() },
            priority: 200,
        });
        reg.register(UiComponent {
            id: "high".into(),
            source: "p".into(),
            slot: UiSlot::SidePanel,
            kind: UiComponentKind::Label { text: "high".into() },
            priority: 10,
        });

        let items = reg.for_slot(UiSlot::SidePanel);
        assert_eq!(items[0].id, "high");
        assert_eq!(items[1].id, "low");
    }
}
