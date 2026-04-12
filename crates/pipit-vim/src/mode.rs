//! Vim editing mode.

use serde::{Deserialize, Serialize};

/// The two primary editing modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VimMode {
    /// Text is inserted directly.
    Insert,
    /// Keys are commands, not text.
    Normal,
}
