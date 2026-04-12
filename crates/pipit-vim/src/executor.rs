//! The result of processing a key event.

/// What happened when a key was processed.
#[derive(Debug, Clone)]
pub struct HandleResult {
    /// Whether the key was consumed by the Vim engine.
    pub consumed: bool,
    /// Whether the mode changed (for UI refresh).
    pub mode_changed: bool,
}

impl HandleResult {
    pub fn consumed() -> Self {
        Self {
            consumed: true,
            mode_changed: false,
        }
    }

    pub fn consumed_mode_changed() -> Self {
        Self {
            consumed: true,
            mode_changed: true,
        }
    }

    pub fn ignored() -> Self {
        Self {
            consumed: false,
            mode_changed: false,
        }
    }
}
