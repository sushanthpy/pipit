//! Dot-repeat change recording.

use crate::command::VimCommand;

/// A recorded change for dot-repeat.
#[derive(Debug, Clone)]
pub struct RepeatableChange {
    /// The command that initiated the change.
    pub command: VimCommand,
    /// Text inserted during an insert session (if any).
    pub inserted_text: Option<String>,
}
