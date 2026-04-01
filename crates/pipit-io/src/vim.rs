//! Vim Input Mode State Machine (Task 4.1)
//!
//! A Vim normal-mode emulator for the TUI input composer.
//! Supports operator-motion composition, dot-repeat, and text objects.
//!
//! The command parser is a deterministic pushdown automaton (DPDA) with
//! stack depth bounded by 2 (operator + pending motion/text-object).

use serde::{Deserialize, Serialize};

/// Maximum count to prevent overflow from malicious input.
const MAX_VIM_COUNT: u32 = 10000;

/// Vim editing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VimMode {
    Insert,
    Normal,
    Replace,
}

/// Vim operator (the verb in operator-motion composition).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Delete,
    Change,
    Yank,
}

/// Vim motion (the noun in operator-motion composition).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordForward,
    WordBackward,
    WordEnd,
    BigWordForward,
    BigWordBackward,
    BigWordEnd,
    LineStart,
    FirstNonBlank,
    LineEnd,
    FindChar(char),
    FindCharReverse(char),
    TilChar(char),
    TilCharReverse(char),
}

/// Vim text object (for di", ci(, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextObject {
    InnerWord,
    AWord,
    InnerQuote(char),
    AQuote(char),
    InnerParen,
    AParen,
    InnerBrace,
    ABrace,
    InnerBracket,
    ABracket,
}

/// Command parse state — the DPDA stack.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandState {
    /// Waiting for a command.
    Idle,
    /// Accumulating a count prefix.
    AccumulatingCount(u32),
    /// An operator is pending, waiting for a motion or text object.
    PendingMotion {
        operator: Operator,
        count: u32,
    },
    /// Waiting for the char argument of f/F/t/T.
    PendingFindChar {
        operator: Option<Operator>,
        count: u32,
        forward: bool,
        inclusive: bool,
    },
    /// Waiting for text object character after i/a.
    PendingTextObject {
        operator: Operator,
        count: u32,
        inner: bool,
    },
}

/// A fully parsed Vim command ready for execution.
#[derive(Debug, Clone, PartialEq)]
pub enum VimCommand {
    /// Enter insert mode.
    EnterInsert,
    /// Enter insert mode at end of line.
    AppendEnd,
    /// Enter insert mode after cursor.
    Append,
    /// Enter insert mode at start of line.
    InsertLineStart,
    /// Enter normal mode.
    EnterNormal,
    /// Delete with operator-motion.
    OperatorMotion {
        operator: Operator,
        motion: Motion,
        count: u32,
    },
    /// Delete/change/yank with text object.
    OperatorTextObject {
        operator: Operator,
        text_object: TextObject,
        count: u32,
    },
    /// Simple motion without operator.
    Move { motion: Motion, count: u32 },
    /// Delete character under cursor.
    DeleteChar { count: u32 },
    /// Delete character before cursor.
    DeleteCharBefore { count: u32 },
    /// Operator on whole line (dd, yy, cc).
    OperatorLine { operator: Operator, count: u32 },
    /// Paste from register.
    Paste { before: bool },
    /// Dot repeat last change.
    DotRepeat,
    /// Repeat last find (;).
    RepeatFind,
    /// Reverse repeat last find (,).
    ReverseFind,
    /// Undo.
    Undo,
    /// Redo.
    Redo,
    /// Join lines.
    JoinLines,
    /// Replace single character.
    ReplaceChar(char),
    /// Toggle case of character under cursor.
    ToggleCase,
    /// Indent line right.
    IndentRight { count: u32 },
    /// Indent line left.
    IndentLeft { count: u32 },
    /// Open line below.
    OpenBelow,
    /// Open line above.
    OpenAbove,
}

/// A minimal record of a change for dot-repeat.
#[derive(Debug, Clone)]
pub struct RecordedChange {
    pub command: VimCommand,
    /// Text inserted (if any) during the change.
    pub inserted_text: Option<String>,
}

/// The Vim state machine.
pub struct VimStateMachine {
    /// Current mode.
    pub mode: VimMode,
    /// Command parse state.
    state: CommandState,
    /// Last recorded change for dot-repeat.
    last_change: Option<RecordedChange>,
    /// Last find character for ; and , repeat.
    last_find: Option<(char, bool, bool)>, // (char, forward, inclusive)
    /// Yank register.
    register: Option<String>,
    /// Whether the register content is linewise.
    register_linewise: bool,
}

impl VimStateMachine {
    pub fn new() -> Self {
        Self {
            mode: VimMode::Insert, // Start in insert mode (like VS Code)
            state: CommandState::Idle,
            last_change: None,
            last_find: None,
            register: None,
            register_linewise: false,
        }
    }

    /// Process a key input. Returns a VimCommand if the input completes a command,
    /// or None if more input is needed.
    pub fn process_key(&mut self, ch: char) -> Option<VimCommand> {
        if self.mode == VimMode::Insert {
            if ch == '\x1b' {
                // Escape → Normal mode
                self.mode = VimMode::Normal;
                self.state = CommandState::Idle;
                return Some(VimCommand::EnterNormal);
            }
            return None; // Pass through to text input
        }

        match &self.state {
            CommandState::Idle => self.process_idle(ch),
            CommandState::AccumulatingCount(count) => {
                let count = *count;
                self.process_count(ch, count)
            }
            CommandState::PendingMotion { operator, count } => {
                let op = *operator;
                let count = *count;
                self.process_pending_motion(ch, op, count)
            }
            CommandState::PendingFindChar {
                operator,
                count,
                forward,
                inclusive,
            } => {
                let op = *operator;
                let count = *count;
                let fwd = *forward;
                let inc = *inclusive;
                self.process_find_char(ch, op, count, fwd, inc)
            }
            CommandState::PendingTextObject {
                operator,
                count,
                inner,
            } => {
                let op = *operator;
                let count = *count;
                let inner = *inner;
                self.process_text_object(ch, op, count, inner)
            }
        }
    }

    /// Set the yank register content.
    pub fn set_register(&mut self, content: String, linewise: bool) {
        self.register = Some(content);
        self.register_linewise = linewise;
    }

    /// Get the yank register content.
    pub fn get_register(&self) -> Option<(&str, bool)> {
        self.register
            .as_deref()
            .map(|r| (r, self.register_linewise))
    }

    /// Record a change for dot-repeat.
    pub fn record_change(&mut self, command: VimCommand, inserted_text: Option<String>) {
        self.last_change = Some(RecordedChange {
            command,
            inserted_text,
        });
    }

    fn process_idle(&mut self, ch: char) -> Option<VimCommand> {
        match ch {
            '1'..='9' => {
                self.state = CommandState::AccumulatingCount((ch as u32) - ('0' as u32));
                None
            }
            'i' => {
                self.mode = VimMode::Insert;
                Some(VimCommand::EnterInsert)
            }
            'a' => {
                self.mode = VimMode::Insert;
                Some(VimCommand::Append)
            }
            'A' => {
                self.mode = VimMode::Insert;
                Some(VimCommand::AppendEnd)
            }
            'I' => {
                self.mode = VimMode::Insert;
                Some(VimCommand::InsertLineStart)
            }
            'o' => {
                self.mode = VimMode::Insert;
                Some(VimCommand::OpenBelow)
            }
            'O' => {
                self.mode = VimMode::Insert;
                Some(VimCommand::OpenAbove)
            }
            'h' => Some(VimCommand::Move { motion: Motion::Left, count: 1 }),
            'l' => Some(VimCommand::Move { motion: Motion::Right, count: 1 }),
            'j' => Some(VimCommand::Move { motion: Motion::Down, count: 1 }),
            'k' => Some(VimCommand::Move { motion: Motion::Up, count: 1 }),
            'w' => Some(VimCommand::Move { motion: Motion::WordForward, count: 1 }),
            'b' => Some(VimCommand::Move { motion: Motion::WordBackward, count: 1 }),
            'e' => Some(VimCommand::Move { motion: Motion::WordEnd, count: 1 }),
            'W' => Some(VimCommand::Move { motion: Motion::BigWordForward, count: 1 }),
            'B' => Some(VimCommand::Move { motion: Motion::BigWordBackward, count: 1 }),
            'E' => Some(VimCommand::Move { motion: Motion::BigWordEnd, count: 1 }),
            '0' => Some(VimCommand::Move { motion: Motion::LineStart, count: 1 }),
            '^' => Some(VimCommand::Move { motion: Motion::FirstNonBlank, count: 1 }),
            '$' => Some(VimCommand::Move { motion: Motion::LineEnd, count: 1 }),
            'x' => Some(VimCommand::DeleteChar { count: 1 }),
            'X' => Some(VimCommand::DeleteCharBefore { count: 1 }),
            'd' => {
                self.state = CommandState::PendingMotion {
                    operator: Operator::Delete,
                    count: 1,
                };
                None
            }
            'c' => {
                self.state = CommandState::PendingMotion {
                    operator: Operator::Change,
                    count: 1,
                };
                None
            }
            'y' => {
                self.state = CommandState::PendingMotion {
                    operator: Operator::Yank,
                    count: 1,
                };
                None
            }
            'p' => Some(VimCommand::Paste { before: false }),
            'P' => Some(VimCommand::Paste { before: true }),
            '.' => Some(VimCommand::DotRepeat),
            ';' => Some(VimCommand::RepeatFind),
            ',' => Some(VimCommand::ReverseFind),
            'u' => Some(VimCommand::Undo),
            'J' => Some(VimCommand::JoinLines),
            '~' => Some(VimCommand::ToggleCase),
            '>' => Some(VimCommand::IndentRight { count: 1 }),
            '<' => Some(VimCommand::IndentLeft { count: 1 }),
            'f' => {
                self.state = CommandState::PendingFindChar {
                    operator: None, count: 1, forward: true, inclusive: true,
                };
                None
            }
            'F' => {
                self.state = CommandState::PendingFindChar {
                    operator: None, count: 1, forward: false, inclusive: true,
                };
                None
            }
            't' => {
                self.state = CommandState::PendingFindChar {
                    operator: None, count: 1, forward: true, inclusive: false,
                };
                None
            }
            'T' => {
                self.state = CommandState::PendingFindChar {
                    operator: None, count: 1, forward: false, inclusive: false,
                };
                None
            }
            '\x1b' => {
                self.state = CommandState::Idle;
                None
            }
            _ => {
                self.state = CommandState::Idle;
                None
            }
        }
    }

    fn process_count(&mut self, ch: char, current: u32) -> Option<VimCommand> {
        if ch.is_ascii_digit() {
            let new_count = current * 10 + (ch as u32 - '0' as u32);
            self.state = CommandState::AccumulatingCount(new_count.min(MAX_VIM_COUNT));
            return None;
        }
        // Count is complete — process the command with count
        self.state = CommandState::Idle;
        let count = current;
        match ch {
            'd' => {
                self.state = CommandState::PendingMotion { operator: Operator::Delete, count };
                None
            }
            'c' => {
                self.state = CommandState::PendingMotion { operator: Operator::Change, count };
                None
            }
            'y' => {
                self.state = CommandState::PendingMotion { operator: Operator::Yank, count };
                None
            }
            'h' => Some(VimCommand::Move { motion: Motion::Left, count }),
            'l' => Some(VimCommand::Move { motion: Motion::Right, count }),
            'j' => Some(VimCommand::Move { motion: Motion::Down, count }),
            'k' => Some(VimCommand::Move { motion: Motion::Up, count }),
            'w' => Some(VimCommand::Move { motion: Motion::WordForward, count }),
            'b' => Some(VimCommand::Move { motion: Motion::WordBackward, count }),
            'x' => Some(VimCommand::DeleteChar { count }),
            _ => None,
        }
    }

    fn process_pending_motion(
        &mut self,
        ch: char,
        operator: Operator,
        count: u32,
    ) -> Option<VimCommand> {
        self.state = CommandState::Idle;

        // Check for line-wise operator (dd, yy, cc)
        let line_op = match operator {
            Operator::Delete if ch == 'd' => true,
            Operator::Change if ch == 'c' => true,
            Operator::Yank if ch == 'y' => true,
            _ => false,
        };
        if line_op {
            if operator == Operator::Change {
                self.mode = VimMode::Insert;
            }
            return Some(VimCommand::OperatorLine { operator, count });
        }

        // Check for text objects (i/a prefix)
        if ch == 'i' {
            self.state = CommandState::PendingTextObject { operator, count, inner: true };
            return None;
        }
        if ch == 'a' {
            self.state = CommandState::PendingTextObject { operator, count, inner: false };
            return None;
        }

        // Check for find motions
        if matches!(ch, 'f' | 'F' | 't' | 'T') {
            let (forward, inclusive) = match ch {
                'f' => (true, true),
                'F' => (false, true),
                't' => (true, false),
                'T' => (false, false),
                _ => unreachable!(),
            };
            self.state = CommandState::PendingFindChar {
                operator: Some(operator), count, forward, inclusive,
            };
            return None;
        }

        // Regular motions
        let motion = match ch {
            'h' => Motion::Left,
            'l' => Motion::Right,
            'j' => Motion::Down,
            'k' => Motion::Up,
            'w' => Motion::WordForward,
            'b' => Motion::WordBackward,
            'e' => Motion::WordEnd,
            'W' => Motion::BigWordForward,
            'B' => Motion::BigWordBackward,
            'E' => Motion::BigWordEnd,
            '0' => Motion::LineStart,
            '^' => Motion::FirstNonBlank,
            '$' => Motion::LineEnd,
            _ => return None, // Unknown motion — cancel
        };

        if operator == Operator::Change {
            self.mode = VimMode::Insert;
        }
        Some(VimCommand::OperatorMotion { operator, motion, count })
    }

    fn process_find_char(
        &mut self,
        ch: char,
        operator: Option<Operator>,
        count: u32,
        forward: bool,
        inclusive: bool,
    ) -> Option<VimCommand> {
        self.state = CommandState::Idle;
        self.last_find = Some((ch, forward, inclusive));

        let motion = match (forward, inclusive) {
            (true, true) => Motion::FindChar(ch),
            (false, true) => Motion::FindCharReverse(ch),
            (true, false) => Motion::TilChar(ch),
            (false, false) => Motion::TilCharReverse(ch),
        };

        match operator {
            Some(op) => {
                if op == Operator::Change {
                    self.mode = VimMode::Insert;
                }
                Some(VimCommand::OperatorMotion { operator: op, motion, count })
            }
            None => Some(VimCommand::Move { motion, count }),
        }
    }

    fn process_text_object(
        &mut self,
        ch: char,
        operator: Operator,
        count: u32,
        inner: bool,
    ) -> Option<VimCommand> {
        self.state = CommandState::Idle;

        let text_object = match ch {
            'w' if inner => TextObject::InnerWord,
            'w' => TextObject::AWord,
            '"' | '\'' | '`' => {
                if inner { TextObject::InnerQuote(ch) } else { TextObject::AQuote(ch) }
            }
            '(' | ')' => {
                if inner { TextObject::InnerParen } else { TextObject::AParen }
            }
            '{' | '}' => {
                if inner { TextObject::InnerBrace } else { TextObject::ABrace }
            }
            '[' | ']' => {
                if inner { TextObject::InnerBracket } else { TextObject::ABracket }
            }
            _ => return None,
        };

        if operator == Operator::Change {
            self.mode = VimMode::Insert;
        }
        Some(VimCommand::OperatorTextObject { operator, text_object, count })
    }
}

impl Default for VimStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_enters_normal_mode() {
        let mut sm = VimStateMachine::new();
        assert_eq!(sm.mode, VimMode::Insert);
        let cmd = sm.process_key('\x1b');
        assert_eq!(sm.mode, VimMode::Normal);
        assert!(matches!(cmd, Some(VimCommand::EnterNormal)));
    }

    #[test]
    fn dd_deletes_line() {
        let mut sm = VimStateMachine::new();
        sm.mode = VimMode::Normal;
        assert!(sm.process_key('d').is_none());
        let cmd = sm.process_key('d');
        assert!(matches!(cmd, Some(VimCommand::OperatorLine { operator: Operator::Delete, count: 1 })));
    }

    #[test]
    fn count_motion() {
        let mut sm = VimStateMachine::new();
        sm.mode = VimMode::Normal;
        sm.process_key('3');
        let cmd = sm.process_key('w');
        assert!(matches!(cmd, Some(VimCommand::Move { motion: Motion::WordForward, count: 3 })));
    }

    #[test]
    fn diw_text_object() {
        let mut sm = VimStateMachine::new();
        sm.mode = VimMode::Normal;
        sm.process_key('d');
        sm.process_key('i');
        let cmd = sm.process_key('w');
        assert!(matches!(cmd, Some(VimCommand::OperatorTextObject {
            operator: Operator::Delete,
            text_object: TextObject::InnerWord,
            count: 1,
        })));
    }

    #[test]
    fn ci_quote() {
        let mut sm = VimStateMachine::new();
        sm.mode = VimMode::Normal;
        sm.process_key('c');
        sm.process_key('i');
        let cmd = sm.process_key('"');
        assert!(matches!(cmd, Some(VimCommand::OperatorTextObject {
            operator: Operator::Change,
            text_object: TextObject::InnerQuote('"'),
            ..
        })));
        assert_eq!(sm.mode, VimMode::Insert);
    }
}
