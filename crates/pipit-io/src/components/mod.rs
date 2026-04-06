//! Pipit TUI Component Library — 86 components across 8 categories.
//!
//! Built on ratatui 0.29 + ecosystem crates. Each component is a struct
//! implementing `Widget` or `StatefulWidget`, composed via Cassowary
//! constraint layout.
//!
//! # Architecture
//!
//! Unlike Ink's React virtual-DOM reconciliation (O(n) per frame with GC
//! pauses), ratatui's immediate-mode rendering sends only changed cells
//! to the terminal — O(Δ) per frame, zero garbage collection.

use ratatui::prelude::*;

/// Helper to disambiguate `Widget::render` vs `StatefulWidget::render`
/// for types like `List` and `Table` that implement both traits.
#[inline]
pub(crate) fn render_widget<W: ratatui::widgets::Widget>(widget: W, area: Rect, buf: &mut Buffer) {
    ratatui::widgets::Widget::render(widget, area, buf);
}

pub mod text;
pub mod input;
pub mod feedback;
pub mod layout;
pub mod data;
pub mod agent;
pub mod terminal;
pub mod effects;

// ── Re-exports for ergonomic use ────────────────────────────────────────

// Text display (1-12)
pub use text::{
    MarkdownView, CodeBlock, DiffView, TextWrap, BigTextView, AnsiText,
    JsonTreeView, ErrorDisplay, HelpText, ThinkingBlock, Citation,
};

// Input controls (13-22)
pub use input::{
    CommandInput, SingleLineInput, ConfirmPrompt, SelectPrompt,
    SearchInput, PasswordInput, PathInput, ModelSelector,
};

// Feedback & status (23-36)
pub use feedback::{
    AnimatedSpinner, SpinnerStyle, ProgressBar, TokenCounter, CostDisplay,
    StatusBar as ComponentStatusBar, SkeletonLoader, StreamingIndicator,
    ToolRunning, PermissionPrompt, ModeIndicator, VerificationBadge,
    BranchIndicator,
};

// Layout & structure (37-48)
pub use layout::{
    SplitPane, TabBarView, ScrollContainer, PopupOverlay,
    CollapsibleSection, Sidebar, Breadcrumb, Panel, FloatingWindow,
    Divider, Grid,
};

// Data display (49-62)
pub use data::{
    DataTable, FileTree, VirtualList, KeyValueTable, SparklineView,
    DepGraph, TimelineView, MetricCard, Badge,
};

// Agent-specific (63-74)
pub use agent::{
    ToolCallDisplay, AgentOutput, ToolApprovalCard, FileEditPreview,
    TaskListView, TodoListView, MemoryView, SessionSummary,
    AgentTree, SkillBrowser, McpServerStatus,
};

// Terminal integration (75-80)
pub use terminal::{
    EmbeddedTerminal, CommandHistory, CompletionPopup,
    KeybindingOverlay, VoiceIndicator,
};

// Effects (81-86)
pub use effects::{
    Theme, FadeTransition, SlideTransition, PulseHighlight,
};
