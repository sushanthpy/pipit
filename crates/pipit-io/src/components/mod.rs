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

pub mod agent;
pub mod data;
pub mod effects;
pub mod feedback;
pub mod input;
pub mod layout;
pub mod terminal;
pub mod text;

// ── Re-exports for ergonomic use ────────────────────────────────────────

// Text display (1-12)
pub use text::{
    AnsiText, BigTextView, Citation, CodeBlock, DiffView, ErrorDisplay, HelpText, JsonTreeView,
    MarkdownView, TextWrap, ThinkingBlock,
};

// Input controls (13-22)
pub use input::{
    CommandInput, ConfirmPrompt, ModelSelector, PasswordInput, PathInput, SearchInput,
    SelectPrompt, SingleLineInput,
};

// Feedback & status (23-36)
pub use feedback::{
    AnimatedSpinner, BranchIndicator, CostDisplay, ModeIndicator, PermissionPrompt, ProgressBar,
    SkeletonLoader, SpinnerStyle, StatusBar as ComponentStatusBar, StreamingIndicator,
    TokenCounter, ToolRunning, VerificationBadge,
};

// Layout & structure (37-48)
pub use layout::{
    Breadcrumb, CollapsibleSection, Divider, FloatingWindow, Grid, Panel, PopupOverlay,
    ScrollContainer, Sidebar, SplitPane, TabBarView,
};

// Data display (49-62)
pub use data::{
    Badge, DataTable, DepGraph, FileTree, KeyValueTable, MetricCard, SparklineView, TimelineView,
    VirtualList,
};

// Agent-specific (63-74)
pub use agent::{
    AgentOutput, AgentTree, FileEditPreview, McpServerStatus, MemoryView, SessionSummary,
    SkillBrowser, TaskListView, TodoListView, ToolApprovalCard, ToolCallDisplay,
};

// Terminal integration (75-80)
pub use terminal::{
    CommandHistory, CompletionPopup, EmbeddedTerminal, KeybindingOverlay, VoiceIndicator,
};

// Effects (81-86)
pub use effects::{FadeTransition, PulseHighlight, SlideTransition, Theme};
