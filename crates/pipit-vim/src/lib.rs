//! pipit-vim — A reusable Vim modal editing engine.
//!
//! Provides a self-contained Vim emulator with:
//! - INSERT / NORMAL mode switching
//! - Operator-motion composition (d, c, y + motion)
//! - Text objects (iw, aw, i", a(, etc.)
//! - Counts (3w, 2dd, etc.)
//! - Dot-repeat
//! - Character-safe multiline text buffer
//! - crossterm key event integration
//!
//! # Architecture
//!
//! ```text
//! KeyEvent → Parser (DPDA) → VimCommand → Executor → TextBuffer mutation
//! ```
//!
//! The engine is ratatui-agnostic. Consumers retrieve mode/cursor/text
//! for rendering.

mod buffer;
mod command;
mod executor;
mod mode;
mod motion;
mod operator;
mod parser;
mod repeat;

pub use buffer::TextBuffer;
pub use command::VimCommand;
pub use executor::HandleResult;
pub use mode::VimMode;
pub use motion::Motion;
pub use operator::Operator;
pub use parser::{CommandState, VimParser};
pub use repeat::RepeatableChange;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// The top-level Vim editor state machine.
///
/// Owns a [`TextBuffer`], [`VimParser`], and change-tracking state.
/// Feed it `crossterm::event::KeyEvent`s via [`handle_key`](VimEditor::handle_key)
/// and read back mode, text, and cursor for rendering.
pub struct VimEditor {
    pub buffer: TextBuffer,
    pub parser: VimParser,
    /// Text inserted during the current insert session (for dot-repeat).
    insert_session_text: String,
    /// Whether we're recording an insert session for repeat.
    in_insert_session: bool,
    /// The command that entered the current insert session (for dot-repeat).
    insert_entry_command: Option<VimCommand>,
}

impl std::fmt::Debug for VimEditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VimEditor")
            .field("mode", &self.parser.mode)
            .field("in_insert_session", &self.in_insert_session)
            .finish_non_exhaustive()
    }
}

impl VimEditor {
    pub fn new() -> Self {
        Self {
            buffer: TextBuffer::new(),
            parser: VimParser::new(),
            insert_session_text: String::new(),
            in_insert_session: false,
            insert_entry_command: None,
        }
    }

    /// Create an editor pre-loaded with text.
    pub fn with_text(text: &str) -> Self {
        let mut editor = Self::new();
        editor.buffer.set_text(text);
        editor
    }

    /// Current editing mode.
    pub fn mode(&self) -> VimMode {
        self.parser.mode
    }

    /// Status text for the mode indicator (e.g. "-- INSERT --").
    pub fn mode_status(&self) -> &'static str {
        match self.parser.mode {
            VimMode::Insert => "-- INSERT --",
            VimMode::Normal => "-- NORMAL --",
        }
    }

    /// Pending command preview (e.g. "d", "2d", "c").
    pub fn pending_display(&self) -> Option<String> {
        self.parser.pending_display()
    }

    /// Process a crossterm key event. Returns a [`HandleResult`].
    pub fn handle_key(&mut self, key: KeyEvent) -> HandleResult {
        if key.kind == KeyEventKind::Release {
            return HandleResult::ignored();
        }

        if self.parser.mode == VimMode::Insert {
            return self.handle_insert_key(key);
        }

        self.handle_normal_key(key)
    }

    fn handle_insert_key(&mut self, key: KeyEvent) -> HandleResult {
        match key.code {
            KeyCode::Esc => {
                // Finalize insert session for dot-repeat.
                self.finalize_insert_session();
                self.parser.mode = VimMode::Normal;
                // In Vim, cursor moves left on Esc (unless at col 0).
                let (row, col) = self.buffer.cursor();
                let line_len = self.buffer.line_len(row);
                if col > 0 && (line_len == 0 || col >= line_len) {
                    self.buffer.set_cursor(row, col.saturating_sub(1));
                }
                HandleResult::consumed_mode_changed()
            }

            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.buffer.insert_char(c);
                if self.in_insert_session {
                    self.insert_session_text.push(c);
                }
                HandleResult::consumed()
            }

            KeyCode::Enter => {
                self.buffer.insert_newline();
                if self.in_insert_session {
                    self.insert_session_text.push('\n');
                }
                HandleResult::consumed()
            }

            KeyCode::Backspace => {
                self.buffer.backspace();
                if self.in_insert_session && !self.insert_session_text.is_empty() {
                    self.insert_session_text.pop();
                }
                HandleResult::consumed()
            }

            KeyCode::Delete => {
                self.buffer.delete_forward();
                HandleResult::consumed()
            }

            KeyCode::Left => {
                self.buffer.move_left(1);
                HandleResult::consumed()
            }
            KeyCode::Right => {
                self.buffer.move_right(1);
                HandleResult::consumed()
            }
            KeyCode::Up => {
                self.buffer.move_up(1);
                HandleResult::consumed()
            }
            KeyCode::Down => {
                self.buffer.move_down(1);
                HandleResult::consumed()
            }
            KeyCode::Home => {
                let row = self.buffer.cursor().0;
                self.buffer.set_cursor(row, 0);
                HandleResult::consumed()
            }
            KeyCode::End => {
                let row = self.buffer.cursor().0;
                let len = self.buffer.line_len(row);
                self.buffer.set_cursor(row, len);
                HandleResult::consumed()
            }

            // Pass through Ctrl/Alt combos to the host (e.g. Ctrl-J for newline in composer)
            _ => HandleResult::ignored(),
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> HandleResult {
        // Extract char for the parser.
        let ch = match key.code {
            KeyCode::Char(c) => c,
            KeyCode::Esc => '\x1b',
            KeyCode::Enter => '\r',
            // Arrow keys in normal mode.
            KeyCode::Left => {
                self.buffer.move_left(1);
                return HandleResult::consumed();
            }
            KeyCode::Right => {
                self.buffer.move_right(1);
                return HandleResult::consumed();
            }
            KeyCode::Up => {
                self.buffer.move_up(1);
                return HandleResult::consumed();
            }
            KeyCode::Down => {
                self.buffer.move_down(1);
                return HandleResult::consumed();
            }
            KeyCode::Home => {
                let row = self.buffer.cursor().0;
                self.buffer.set_cursor(row, 0);
                return HandleResult::consumed();
            }
            KeyCode::End => {
                let row = self.buffer.cursor().0;
                let len = self.buffer.line_len(row);
                self.buffer.set_cursor(row, len.saturating_sub(1).max(0));
                return HandleResult::consumed();
            }
            KeyCode::Backspace => 'X', // treat as X in normal mode
            _ => return HandleResult::ignored(),
        };

        let Some(cmd) = self.parser.process_key(ch) else {
            // Parser needs more input.
            return HandleResult::consumed();
        };

        self.execute_command(cmd)
    }

    fn execute_command(&mut self, cmd: VimCommand) -> HandleResult {
        use VimCommand::*;

        let mode_before = self.parser.mode;

        match cmd.clone() {
            EnterInsert => {
                self.begin_insert_session(cmd);
            }
            Append => {
                self.buffer.move_right(1);
                self.begin_insert_session(cmd);
            }
            AppendEnd => {
                let row = self.buffer.cursor().0;
                let len = self.buffer.line_len(row);
                self.buffer.set_cursor(row, len);
                self.begin_insert_session(cmd);
            }
            InsertLineStart => {
                let row = self.buffer.cursor().0;
                let col = self.buffer.first_non_blank(row);
                self.buffer.set_cursor(row, col);
                self.begin_insert_session(cmd);
            }
            EnterNormal => {
                // Already handled by parser mode switch.
            }
            Move { motion, count } => {
                for _ in 0..count {
                    self.buffer.apply_motion(&motion);
                }
            }
            DeleteChar { count } => {
                for _ in 0..count {
                    self.buffer.delete_forward();
                }
                self.clamp_cursor_normal();
                self.parser.record_change(cmd.clone(), None);
            }
            DeleteCharBefore { count } => {
                for _ in 0..count {
                    self.buffer.backspace();
                }
                self.parser.record_change(cmd.clone(), None);
            }
            OperatorMotion {
                operator,
                motion,
                count,
            } => {
                self.execute_operator_motion(operator, &motion, count);
                if operator == Operator::Change {
                    self.begin_insert_session(cmd.clone());
                } else {
                    self.parser.record_change(cmd.clone(), None);
                }
            }
            OperatorLine { operator, count } => {
                self.execute_operator_line(operator, count);
                if operator == Operator::Change {
                    self.begin_insert_session(cmd.clone());
                } else {
                    self.parser.record_change(cmd.clone(), None);
                }
            }
            OperatorTextObject {
                operator,
                text_object,
                count,
            } => {
                self.execute_operator_text_object(operator, &text_object, count);
                if operator == Operator::Change {
                    self.begin_insert_session(cmd.clone());
                } else {
                    self.parser.record_change(cmd.clone(), None);
                }
            }
            OpenBelow => {
                self.buffer.open_line_below();
                self.begin_insert_session(cmd);
            }
            OpenAbove => {
                self.buffer.open_line_above();
                self.begin_insert_session(cmd);
            }
            Paste { before } => {
                if let Some((text, linewise)) = self.parser.get_register() {
                    let text = text.to_string();
                    if linewise {
                        if before {
                            self.buffer.paste_line_above(&text);
                        } else {
                            self.buffer.paste_line_below(&text);
                        }
                    } else {
                        if !before {
                            self.buffer.move_right(1);
                        }
                        self.buffer.insert_str(&text);
                        // Move cursor to end of paste minus 1.
                        let pasted_chars = text.chars().count();
                        if pasted_chars > 0 {
                            self.buffer.move_left(1);
                        }
                    }
                }
                self.parser.record_change(cmd.clone(), None);
            }
            DotRepeat => {
                if let Some(change) = self.parser.last_change.clone() {
                    let result = self.execute_command(change.command.clone());
                    if let Some(ref text) = change.inserted_text {
                        for c in text.chars() {
                            if c == '\n' {
                                self.buffer.insert_newline();
                            } else {
                                self.buffer.insert_char(c);
                            }
                        }
                        self.finalize_insert_session();
                        self.parser.mode = VimMode::Normal;
                        let (row, col) = self.buffer.cursor();
                        if col > 0 {
                            let line_len = self.buffer.line_len(row);
                            if col >= line_len {
                                self.buffer.set_cursor(row, col.saturating_sub(1));
                            }
                        }
                    }
                    return result;
                }
            }
            RepeatFind => {
                if let Some((ch, forward, inclusive)) = self.parser.last_find {
                    let motion = match (forward, inclusive) {
                        (true, true) => Motion::FindChar(ch),
                        (false, true) => Motion::FindCharReverse(ch),
                        (true, false) => Motion::TilChar(ch),
                        (false, false) => Motion::TilCharReverse(ch),
                    };
                    self.buffer.apply_motion(&motion);
                }
            }
            ReverseFind => {
                if let Some((ch, forward, inclusive)) = self.parser.last_find {
                    let motion = match (!forward, inclusive) {
                        (true, true) => Motion::FindChar(ch),
                        (false, true) => Motion::FindCharReverse(ch),
                        (true, false) => Motion::TilChar(ch),
                        (false, false) => Motion::TilCharReverse(ch),
                    };
                    self.buffer.apply_motion(&motion);
                }
            }
            Undo => {
                self.buffer.undo();
            }
            Redo => {
                // Not implemented yet.
            }
            JoinLines => {
                self.buffer.join_lines();
                self.parser.record_change(cmd.clone(), None);
            }
            ReplaceChar(ch) => {
                self.buffer.replace_char(ch);
                self.parser.record_change(cmd.clone(), None);
            }
            ToggleCase => {
                self.buffer.toggle_case();
                self.parser.record_change(cmd.clone(), None);
            }
            IndentRight { count } => {
                for _ in 0..count {
                    self.buffer.indent_right();
                }
                self.parser.record_change(cmd.clone(), None);
            }
            IndentLeft { count } => {
                for _ in 0..count {
                    self.buffer.indent_left();
                }
                self.parser.record_change(cmd.clone(), None);
            }
        }

        let mode_changed = self.parser.mode != mode_before;
        if mode_changed {
            HandleResult::consumed_mode_changed()
        } else {
            HandleResult::consumed()
        }
    }

    fn execute_operator_motion(&mut self, op: Operator, motion: &Motion, count: u32) {
        let (start_row, start_col) = self.buffer.cursor();
        for _ in 0..count {
            self.buffer.apply_motion(motion);
        }
        let (end_row, end_col) = self.buffer.cursor();

        // Determine the range (may be backwards for backward motions).
        let (from_row, from_col, to_row, to_col) = if (start_row, start_col) <= (end_row, end_col)
        {
            (start_row, start_col, end_row, end_col)
        } else {
            (end_row, end_col, start_row, start_col)
        };

        // Restore cursor to start of range.
        self.buffer.set_cursor(from_row, from_col);

        let deleted = self.buffer.delete_range(from_row, from_col, to_row, to_col);
        if op == Operator::Delete || op == Operator::Change {
            self.parser.set_register(deleted, false);
        } else if op == Operator::Yank {
            self.parser.set_register(deleted.clone(), false);
            // Yank doesn't modify buffer — re-insert.
            self.buffer.insert_str(&deleted);
            self.buffer.set_cursor(from_row, from_col);
        }

        self.clamp_cursor_normal();
    }

    fn execute_operator_line(&mut self, op: Operator, count: u32) {
        let start_row = self.buffer.cursor().0;
        let end_row = (start_row + count as usize - 1).min(self.buffer.line_count() - 1);
        let deleted = self.buffer.delete_lines(start_row, end_row);
        if op == Operator::Yank {
            self.parser.set_register(deleted.clone(), true);
            // Yank doesn't delete — re-insert.
            for (i, line) in deleted.lines().enumerate() {
                self.buffer.lines.insert(start_row + i, line.to_string());
            }
            self.buffer.set_cursor(start_row, self.buffer.cursor().1);
        } else {
            self.parser.set_register(deleted, true);
        }
        self.clamp_cursor_normal();
    }

    fn execute_operator_text_object(
        &mut self,
        op: Operator,
        text_object: &command::TextObject,
        _count: u32,
    ) {
        if let Some((start_row, start_col, end_row, end_col)) =
            self.buffer.find_text_object(text_object)
        {
            self.buffer.set_cursor(start_row, start_col);
            let deleted = self.buffer.delete_range(start_row, start_col, end_row, end_col);
            if op == Operator::Yank {
                self.parser.set_register(deleted.clone(), false);
                self.buffer.insert_str(&deleted);
                self.buffer.set_cursor(start_row, start_col);
            } else {
                self.parser.set_register(deleted, false);
            }
            self.clamp_cursor_normal();
        }
    }

    fn begin_insert_session(&mut self, cmd: VimCommand) {
        self.in_insert_session = true;
        self.insert_session_text.clear();
        self.insert_entry_command = Some(cmd);
        self.parser.mode = VimMode::Insert;
    }

    fn finalize_insert_session(&mut self) {
        if self.in_insert_session {
            let text = if self.insert_session_text.is_empty() {
                None
            } else {
                Some(self.insert_session_text.clone())
            };
            if let Some(cmd) = self.insert_entry_command.take() {
                self.parser.record_change(cmd, text);
            }
            self.in_insert_session = false;
            self.insert_session_text.clear();
        }
    }

    /// In normal mode, cursor must not be past end of line.
    fn clamp_cursor_normal(&mut self) {
        if self.parser.mode == VimMode::Normal {
            let (row, col) = self.buffer.cursor();
            let line_len = self.buffer.line_len(row);
            if line_len > 0 && col >= line_len {
                self.buffer.set_cursor(row, line_len - 1);
            }
        }
    }
}

impl Default for VimEditor {
    fn default() -> Self {
        Self::new()
    }
}
