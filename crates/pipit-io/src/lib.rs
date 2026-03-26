pub mod app;
pub mod render;
pub mod tui;
pub mod input;

pub use app::{TuiState, SharedTuiState, ActivityLine};
pub use render::StreamingMarkdownRenderer;
pub use tui::{PipitUi, InteractiveApprovalHandler, StatusBarState, VerificationState};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum IoError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Terminal error: {0}")]
    Terminal(String),
}
