pub mod agent_colors;
pub mod animation;
pub mod app;
pub mod budget_viz;
pub mod components;
pub mod composer;
pub mod editor_integration;
pub mod input;
pub mod math;
pub mod render;
pub mod render_engine;
pub mod spinner_verbs;
pub mod suggestions;
pub mod terminal_caps;
pub mod theme;
pub mod theme_bridge;
pub mod tui;
pub mod vim;

pub use app::{
    ActivityLine, CompletionBanner, Overlay, SharedTuiState, TabView, TuiState, UiMode, WidthClass,
    handle_mouse, handle_resize,
};
pub use composer::Composer;
pub use render::StreamingMarkdownRenderer;
pub use render_engine::SyntaxHighlighter;
pub use tui::{InteractiveApprovalHandler, PipitUi, StatusBarState, VerificationState};

use std::sync::atomic::{AtomicBool, Ordering};
use thiserror::Error;

/// Global flag: true while the ratatui alternate screen owns stderr.
/// When active, [`TuiSafeStderr`] silently discards writes so that
/// tracing output doesn't corrupt the framebuffer.
static TUI_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Mark the TUI as active (called by `app::init_terminal`).
pub fn set_tui_active(active: bool) {
    TUI_ACTIVE.store(active, Ordering::Release);
}

/// A drop-in `io::Write` wrapper around stderr that discards bytes while
/// the full-screen TUI is active.  Use as the tracing subscriber writer
/// to prevent log lines from corrupting the ratatui alternate screen.
pub struct TuiSafeStderr;

impl std::io::Write for TuiSafeStderr {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if TUI_ACTIVE.load(Ordering::Acquire) {
            Ok(buf.len()) // discard
        } else {
            std::io::stderr().write(buf)
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if TUI_ACTIVE.load(Ordering::Acquire) {
            Ok(())
        } else {
            std::io::stderr().flush()
        }
    }
}

#[derive(Debug, Error)]
pub enum IoError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Terminal error: {0}")]
    Terminal(String),
}
