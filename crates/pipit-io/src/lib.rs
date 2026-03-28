pub mod app;
pub mod composer;
pub mod render;
pub mod tui;
pub mod input;

pub use app::{TuiState, SharedTuiState, ActivityLine};
pub use composer::Composer;
pub use render::StreamingMarkdownRenderer;
pub use tui::{PipitUi, InteractiveApprovalHandler, StatusBarState, VerificationState};

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
