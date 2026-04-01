//! Modal Editor Integration — Composable Editing Contract
//!
//! Hardened editing surface that composes vim mode transitions, diff
//! presentation, approval hooks, and post-edit verification feedback
//! with the agent runtime. Pure state transition + buffered side effects.
//!
//! The editing contract is: every mutation is an EditorAction, every action
//! produces an EditorEffect, and effects are batched for the runtime.

use crate::vim::{VimMode, VimCommand, VimStateMachine};
use serde::{Deserialize, Serialize};

/// Editor integration mode — how vim interacts with the agent runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditorIntegration {
    /// Standalone editing (no agent interaction).
    Standalone,
    /// Editing within an agent turn (edits are tracked).
    AgentAssisted,
    /// Approval review mode (diff display, approve/deny).
    ApprovalReview,
    /// Read-only viewing (e.g., file preview).
    ReadOnly,
}

/// A high-level editor action produced by interpreting VimCommands
/// in the context of the agent runtime.
#[derive(Debug, Clone)]
pub enum EditorAction {
    /// Pure text edit (insert, delete, change).
    TextEdit {
        kind: EditKind,
        range: EditRange,
        text: Option<String>,
    },
    /// Accept/reject a proposed agent edit.
    ApprovalResponse {
        accepted: bool,
        file: String,
    },
    /// Request verification of recent edits.
    RequestVerification,
    /// Navigate to a specific file/position (from diff view).
    Navigate {
        file: String,
        line: Option<u32>,
    },
    /// Undo the last agent edit.
    UndoAgentEdit,
    /// Switch editor integration mode.
    SwitchMode(EditorIntegration),
    /// No action (key was consumed for mode management).
    NoOp,
}

/// Kind of text edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    Insert,
    Delete,
    Change,
    Yank,
    Paste,
    Indent,
}

/// Range specification for an edit.
#[derive(Debug, Clone)]
pub enum EditRange {
    /// Character under cursor.
    Char,
    /// N characters.
    Chars(u32),
    /// Current line.
    Line,
    /// N lines.
    Lines(u32),
    /// Word motion.
    Word { count: u32 },
    /// To end of line.
    ToLineEnd,
    /// Text object (inner/a word, quote, paren, etc.).
    TextObject { description: String },
}

/// Side effects produced by editor actions, batched for the runtime.
#[derive(Debug, Clone)]
pub enum EditorEffect {
    /// Text was modified — notify the runtime.
    TextModified {
        file: String,
        lines_changed: u32,
    },
    /// User approved an edit.
    EditApproved { file: String },
    /// User rejected an edit.
    EditRejected { file: String },
    /// Verification requested.
    VerificationRequested,
    /// Mode indicator changed (for status bar).
    ModeChanged {
        vim_mode: VimMode,
        integration: EditorIntegration,
    },
    /// Cursor position changed.
    CursorMoved { line: u32, col: u32 },
}

/// The composable editor state machine.
/// Wraps VimStateMachine with agent-runtime awareness.
pub struct EditorController {
    /// The underlying vim state machine.
    vim: VimStateMachine,
    /// Current integration mode.
    integration: EditorIntegration,
    /// Buffered effects to flush to the runtime.
    effects: Vec<EditorEffect>,
    /// Current file being edited.
    current_file: Option<String>,
    /// Whether edits are pending verification.
    has_pending_edits: bool,
    /// Whether we're in a diff/approval view.
    in_approval_view: bool,
}

impl EditorController {
    pub fn new() -> Self {
        Self {
            vim: VimStateMachine::new(),
            integration: EditorIntegration::Standalone,
            effects: Vec::new(),
            current_file: None,
            has_pending_edits: false,
            in_approval_view: false,
        }
    }

    /// Set the integration mode.
    pub fn set_integration(&mut self, mode: EditorIntegration) {
        self.integration = mode;
        self.effects.push(EditorEffect::ModeChanged {
            vim_mode: self.vim.mode,
            integration: mode,
        });
    }

    /// Set the current file being edited.
    pub fn set_file(&mut self, file: &str) {
        self.current_file = Some(file.to_string());
    }

    /// Process a key input through the vim state machine and produce
    /// editor actions appropriate for the current integration mode.
    pub fn process_key(&mut self, ch: char) -> EditorAction {
        // In approval review mode, remap keys
        if self.integration == EditorIntegration::ApprovalReview {
            return self.process_approval_key(ch);
        }

        // In read-only mode, only allow navigation
        if self.integration == EditorIntegration::ReadOnly {
            return self.process_readonly_key(ch);
        }

        // Standard vim processing
        let cmd = self.vim.process_key(ch);

        match cmd {
            Some(command) => self.translate_command(command),
            None => {
                // Key consumed for mode management or insert text
                if self.vim.mode == VimMode::Insert {
                    // In agent-assisted mode, track that edits are happening
                    if self.integration == EditorIntegration::AgentAssisted {
                        self.has_pending_edits = true;
                    }
                }
                EditorAction::NoOp
            }
        }
    }

    /// Drain buffered effects for the runtime to process.
    pub fn drain_effects(&mut self) -> Vec<EditorEffect> {
        std::mem::take(&mut self.effects)
    }

    /// Current vim mode.
    pub fn vim_mode(&self) -> VimMode {
        self.vim.mode
    }

    /// Current integration mode.
    pub fn integration_mode(&self) -> EditorIntegration {
        self.integration
    }

    /// Whether there are pending edits that need verification.
    pub fn has_pending_edits(&self) -> bool {
        self.has_pending_edits
    }

    /// Mark pending edits as verified.
    pub fn mark_verified(&mut self) {
        self.has_pending_edits = false;
    }

    // ── Internal translation ──

    fn translate_command(&mut self, command: VimCommand) -> EditorAction {
        match command {
            VimCommand::EnterInsert | VimCommand::Append | VimCommand::AppendEnd
            | VimCommand::InsertLineStart | VimCommand::OpenBelow | VimCommand::OpenAbove => {
                self.effects.push(EditorEffect::ModeChanged {
                    vim_mode: VimMode::Insert,
                    integration: self.integration,
                });
                EditorAction::NoOp
            }
            VimCommand::EnterNormal => {
                self.effects.push(EditorEffect::ModeChanged {
                    vim_mode: VimMode::Normal,
                    integration: self.integration,
                });
                // Leaving insert mode after edits → notify runtime
                if self.has_pending_edits {
                    if let Some(ref file) = self.current_file {
                        self.effects.push(EditorEffect::TextModified {
                            file: file.clone(),
                            lines_changed: 1,
                        });
                    }
                }
                EditorAction::NoOp
            }
            VimCommand::OperatorMotion { operator, motion, count } => {
                let kind = match operator {
                    crate::vim::Operator::Delete => EditKind::Delete,
                    crate::vim::Operator::Change => EditKind::Change,
                    crate::vim::Operator::Yank => EditKind::Yank,
                };
                self.has_pending_edits = true;
                EditorAction::TextEdit {
                    kind,
                    range: EditRange::Chars(count),
                    text: None,
                }
            }
            VimCommand::OperatorLine { operator, count } => {
                let kind = match operator {
                    crate::vim::Operator::Delete => EditKind::Delete,
                    crate::vim::Operator::Change => EditKind::Change,
                    crate::vim::Operator::Yank => EditKind::Yank,
                };
                self.has_pending_edits = true;
                EditorAction::TextEdit {
                    kind,
                    range: EditRange::Lines(count),
                    text: None,
                }
            }
            VimCommand::DeleteChar { count } => {
                self.has_pending_edits = true;
                EditorAction::TextEdit {
                    kind: EditKind::Delete,
                    range: EditRange::Chars(count),
                    text: None,
                }
            }
            VimCommand::Paste { .. } => {
                self.has_pending_edits = true;
                EditorAction::TextEdit {
                    kind: EditKind::Paste,
                    range: EditRange::Char,
                    text: self.vim.get_register().map(|(s, _)| s.to_string()),
                }
            }
            VimCommand::Undo => EditorAction::UndoAgentEdit,
            _ => EditorAction::NoOp,
        }
    }

    fn process_approval_key(&mut self, ch: char) -> EditorAction {
        match ch {
            'y' | 'Y' => {
                if let Some(ref file) = self.current_file {
                    self.effects.push(EditorEffect::EditApproved {
                        file: file.clone(),
                    });
                    EditorAction::ApprovalResponse {
                        accepted: true,
                        file: file.clone(),
                    }
                } else {
                    EditorAction::NoOp
                }
            }
            'n' | 'N' => {
                if let Some(ref file) = self.current_file {
                    self.effects.push(EditorEffect::EditRejected {
                        file: file.clone(),
                    });
                    EditorAction::ApprovalResponse {
                        accepted: false,
                        file: file.clone(),
                    }
                } else {
                    EditorAction::NoOp
                }
            }
            'v' => {
                self.effects.push(EditorEffect::VerificationRequested);
                EditorAction::RequestVerification
            }
            'q' | '\x1b' => {
                self.set_integration(EditorIntegration::Standalone);
                EditorAction::SwitchMode(EditorIntegration::Standalone)
            }
            _ => EditorAction::NoOp,
        }
    }

    fn process_readonly_key(&mut self, ch: char) -> EditorAction {
        // Only allow navigation in read-only mode
        match ch {
            'j' | 'k' | 'h' | 'l' | 'G' | 'g' | '\x06' | '\x02' => {
                // Let vim handle navigation
                let _ = self.vim.process_key(ch);
                EditorAction::NoOp
            }
            'q' | '\x1b' => {
                self.set_integration(EditorIntegration::Standalone);
                EditorAction::SwitchMode(EditorIntegration::Standalone)
            }
            _ => EditorAction::NoOp,
        }
    }
}

impl Default for EditorController {
    fn default() -> Self {
        Self::new()
    }
}

/// Format the mode indicator for the status bar.
pub fn mode_indicator(vim_mode: VimMode, integration: EditorIntegration) -> &'static str {
    match (vim_mode, integration) {
        (VimMode::Normal, EditorIntegration::ApprovalReview) => "[REVIEW]",
        (VimMode::Normal, EditorIntegration::ReadOnly) => "[VIEW]",
        (VimMode::Normal, EditorIntegration::AgentAssisted) => "[NORMAL·AI]",
        (VimMode::Normal, _) => "[NORMAL]",
        (VimMode::Insert, EditorIntegration::AgentAssisted) => "[INSERT·AI]",
        (VimMode::Insert, _) => "[INSERT]",
        (VimMode::Replace, _) => "[REPLACE]",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_mode_key_handling() {
        let mut ctrl = EditorController::new();
        ctrl.set_integration(EditorIntegration::ApprovalReview);
        ctrl.set_file("test.rs");

        let action = ctrl.process_key('y');
        assert!(matches!(action, EditorAction::ApprovalResponse { accepted: true, .. }));

        let effects = ctrl.drain_effects();
        assert!(effects.iter().any(|e| matches!(e, EditorEffect::EditApproved { .. })));
    }

    #[test]
    fn readonly_blocks_edits() {
        let mut ctrl = EditorController::new();
        ctrl.set_integration(EditorIntegration::ReadOnly);

        // Navigation should work
        let action = ctrl.process_key('j');
        assert!(matches!(action, EditorAction::NoOp));

        // Escape exits read-only
        let action = ctrl.process_key('q');
        assert!(matches!(action, EditorAction::SwitchMode(EditorIntegration::Standalone)));
    }

    #[test]
    fn agent_assisted_tracks_edits() {
        let mut ctrl = EditorController::new();
        ctrl.set_integration(EditorIntegration::AgentAssisted);
        ctrl.set_file("main.rs");

        // Start in insert mode (vim default)
        assert_eq!(ctrl.vim_mode(), VimMode::Insert);
        assert!(!ctrl.has_pending_edits());

        // Type something (insert mode pass-through)
        ctrl.process_key('a');
        assert!(ctrl.has_pending_edits());

        // Exit insert mode → should emit TextModified
        ctrl.process_key('\x1b');
        let effects = ctrl.drain_effects();
        assert!(effects.iter().any(|e| matches!(e, EditorEffect::TextModified { .. })));
    }

    #[test]
    fn mode_indicators() {
        assert_eq!(mode_indicator(VimMode::Normal, EditorIntegration::ApprovalReview), "[REVIEW]");
        assert_eq!(mode_indicator(VimMode::Insert, EditorIntegration::AgentAssisted), "[INSERT·AI]");
        assert_eq!(mode_indicator(VimMode::Normal, EditorIntegration::Standalone), "[NORMAL]");
    }
}
