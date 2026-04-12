//! DPDA command parser for Vim normal mode.
//!
//! Processes characters one at a time, maintaining a small state stack.
//! Stack depth is bounded by 2 (operator + pending motion/text-object).

use crate::command::{TextObject, VimCommand};
use crate::mode::VimMode;
use crate::motion::Motion;
use crate::operator::Operator;
use crate::repeat::RepeatableChange;

/// Maximum count to prevent overflow.
const MAX_VIM_COUNT: u32 = 10_000;

/// Command parse state — the DPDA stack.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandState {
    /// Waiting for a command.
    Idle,
    /// Accumulating a count prefix.
    AccumulatingCount(u32),
    /// An operator is pending, waiting for a motion or text object.
    PendingMotion { operator: Operator, count: u32 },
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
    /// Waiting for character after r.
    PendingReplace { count: u32 },
}

/// The Vim command parser.
pub struct VimParser {
    /// Current mode.
    pub mode: VimMode,
    /// Command parse state.
    pub state: CommandState,
    /// Last recorded change for dot-repeat.
    pub last_change: Option<RepeatableChange>,
    /// Last find character for ; and , repeat.
    pub last_find: Option<(char, bool, bool)>,
    /// Yank register.
    register: Option<String>,
    /// Whether the register content is linewise.
    register_linewise: bool,
}

impl VimParser {
    pub fn new() -> Self {
        Self {
            mode: VimMode::Insert,
            state: CommandState::Idle,
            last_change: None,
            last_find: None,
            register: None,
            register_linewise: false,
        }
    }

    /// Process a key in normal mode. Returns a VimCommand if the input
    /// completes a command, or None if more input is needed.
    pub fn process_key(&mut self, ch: char) -> Option<VimCommand> {
        if self.mode == VimMode::Insert {
            if ch == '\x1b' {
                self.mode = VimMode::Normal;
                self.state = CommandState::Idle;
                return Some(VimCommand::EnterNormal);
            }
            return None;
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
            CommandState::PendingReplace { count } => {
                let _count = *count;
                self.state = CommandState::Idle;
                if ch == '\x1b' {
                    return None;
                }
                Some(VimCommand::ReplaceChar(ch))
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
        self.last_change = Some(RepeatableChange {
            command,
            inserted_text,
        });
    }

    /// Human-readable pending command display.
    pub fn pending_display(&self) -> Option<String> {
        match &self.state {
            CommandState::Idle => None,
            CommandState::AccumulatingCount(n) => Some(n.to_string()),
            CommandState::PendingMotion { operator, count } => {
                let op = match operator {
                    Operator::Delete => "d",
                    Operator::Change => "c",
                    Operator::Yank => "y",
                };
                if *count > 1 {
                    Some(format!("{}{}", count, op))
                } else {
                    Some(op.to_string())
                }
            }
            CommandState::PendingFindChar {
                forward, inclusive, ..
            } => {
                let cmd = match (*forward, *inclusive) {
                    (true, true) => "f",
                    (false, true) => "F",
                    (true, false) => "t",
                    (false, false) => "T",
                };
                Some(cmd.to_string())
            }
            CommandState::PendingTextObject { inner, .. } => {
                if *inner {
                    Some("i".to_string())
                } else {
                    Some("a".to_string())
                }
            }
            CommandState::PendingReplace { .. } => Some("r".to_string()),
        }
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
            'h' => Some(VimCommand::Move {
                motion: Motion::Left,
                count: 1,
            }),
            'l' => Some(VimCommand::Move {
                motion: Motion::Right,
                count: 1,
            }),
            'j' => Some(VimCommand::Move {
                motion: Motion::Down,
                count: 1,
            }),
            'k' => Some(VimCommand::Move {
                motion: Motion::Up,
                count: 1,
            }),
            'w' => Some(VimCommand::Move {
                motion: Motion::WordForward,
                count: 1,
            }),
            'b' => Some(VimCommand::Move {
                motion: Motion::WordBackward,
                count: 1,
            }),
            'e' => Some(VimCommand::Move {
                motion: Motion::WordEnd,
                count: 1,
            }),
            'W' => Some(VimCommand::Move {
                motion: Motion::BigWordForward,
                count: 1,
            }),
            'B' => Some(VimCommand::Move {
                motion: Motion::BigWordBackward,
                count: 1,
            }),
            'E' => Some(VimCommand::Move {
                motion: Motion::BigWordEnd,
                count: 1,
            }),
            '0' => Some(VimCommand::Move {
                motion: Motion::LineStart,
                count: 1,
            }),
            '^' => Some(VimCommand::Move {
                motion: Motion::FirstNonBlank,
                count: 1,
            }),
            '$' => Some(VimCommand::Move {
                motion: Motion::LineEnd,
                count: 1,
            }),
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
            'D' => Some(VimCommand::OperatorMotion {
                operator: Operator::Delete,
                motion: Motion::LineEnd,
                count: 1,
            }),
            'C' => {
                self.mode = VimMode::Insert;
                Some(VimCommand::OperatorMotion {
                    operator: Operator::Change,
                    motion: Motion::LineEnd,
                    count: 1,
                })
            }
            'p' => Some(VimCommand::Paste { before: false }),
            'P' => Some(VimCommand::Paste { before: true }),
            '.' => Some(VimCommand::DotRepeat),
            ';' => Some(VimCommand::RepeatFind),
            ',' => Some(VimCommand::ReverseFind),
            'u' => Some(VimCommand::Undo),
            'J' => Some(VimCommand::JoinLines),
            'r' => {
                self.state = CommandState::PendingReplace { count: 1 };
                None
            }
            '~' => Some(VimCommand::ToggleCase),
            '>' => {
                self.state = CommandState::PendingMotion {
                    operator: Operator::Delete, // We'll intercept >> specially
                    count: 1,
                };
                // Actually, > waits for another > for >>. Handle via a special check.
                self.state = CommandState::Idle;
                Some(VimCommand::IndentRight { count: 1 })
            }
            '<' => {
                self.state = CommandState::Idle;
                Some(VimCommand::IndentLeft { count: 1 })
            }
            'f' => {
                self.state = CommandState::PendingFindChar {
                    operator: None,
                    count: 1,
                    forward: true,
                    inclusive: true,
                };
                None
            }
            'F' => {
                self.state = CommandState::PendingFindChar {
                    operator: None,
                    count: 1,
                    forward: false,
                    inclusive: true,
                };
                None
            }
            't' => {
                self.state = CommandState::PendingFindChar {
                    operator: None,
                    count: 1,
                    forward: true,
                    inclusive: false,
                };
                None
            }
            'T' => {
                self.state = CommandState::PendingFindChar {
                    operator: None,
                    count: 1,
                    forward: false,
                    inclusive: false,
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
        self.state = CommandState::Idle;
        let count = current;
        match ch {
            'd' => {
                self.state = CommandState::PendingMotion {
                    operator: Operator::Delete,
                    count,
                };
                None
            }
            'c' => {
                self.state = CommandState::PendingMotion {
                    operator: Operator::Change,
                    count,
                };
                None
            }
            'y' => {
                self.state = CommandState::PendingMotion {
                    operator: Operator::Yank,
                    count,
                };
                None
            }
            'h' => Some(VimCommand::Move {
                motion: Motion::Left,
                count,
            }),
            'l' => Some(VimCommand::Move {
                motion: Motion::Right,
                count,
            }),
            'j' => Some(VimCommand::Move {
                motion: Motion::Down,
                count,
            }),
            'k' => Some(VimCommand::Move {
                motion: Motion::Up,
                count,
            }),
            'w' => Some(VimCommand::Move {
                motion: Motion::WordForward,
                count,
            }),
            'b' => Some(VimCommand::Move {
                motion: Motion::WordBackward,
                count,
            }),
            'e' => Some(VimCommand::Move {
                motion: Motion::WordEnd,
                count,
            }),
            'W' => Some(VimCommand::Move {
                motion: Motion::BigWordForward,
                count,
            }),
            'B' => Some(VimCommand::Move {
                motion: Motion::BigWordBackward,
                count,
            }),
            'E' => Some(VimCommand::Move {
                motion: Motion::BigWordEnd,
                count,
            }),
            'x' => Some(VimCommand::DeleteChar { count }),
            'X' => Some(VimCommand::DeleteCharBefore { count }),
            'r' => {
                self.state = CommandState::PendingReplace { count };
                None
            }
            'f' => {
                self.state = CommandState::PendingFindChar {
                    operator: None,
                    count,
                    forward: true,
                    inclusive: true,
                };
                None
            }
            'F' => {
                self.state = CommandState::PendingFindChar {
                    operator: None,
                    count,
                    forward: false,
                    inclusive: true,
                };
                None
            }
            't' => {
                self.state = CommandState::PendingFindChar {
                    operator: None,
                    count,
                    forward: true,
                    inclusive: false,
                };
                None
            }
            'T' => {
                self.state = CommandState::PendingFindChar {
                    operator: None,
                    count,
                    forward: false,
                    inclusive: false,
                };
                None
            }
            '>' => Some(VimCommand::IndentRight { count }),
            '<' => Some(VimCommand::IndentLeft { count }),
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

        // Line-wise operator (dd, yy, cc).
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

        // Text objects (i/a prefix).
        if ch == 'i' {
            self.state = CommandState::PendingTextObject {
                operator,
                count,
                inner: true,
            };
            return None;
        }
        if ch == 'a' {
            self.state = CommandState::PendingTextObject {
                operator,
                count,
                inner: false,
            };
            return None;
        }

        // Find motions.
        if matches!(ch, 'f' | 'F' | 't' | 'T') {
            let (forward, inclusive) = match ch {
                'f' => (true, true),
                'F' => (false, true),
                't' => (true, false),
                'T' => (false, false),
                _ => unreachable!(),
            };
            self.state = CommandState::PendingFindChar {
                operator: Some(operator),
                count,
                forward,
                inclusive,
            };
            return None;
        }

        // Regular motions.
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
            _ => return None,
        };

        if operator == Operator::Change {
            self.mode = VimMode::Insert;
        }
        Some(VimCommand::OperatorMotion {
            operator,
            motion,
            count,
        })
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
                Some(VimCommand::OperatorMotion {
                    operator: op,
                    motion,
                    count,
                })
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
                if inner {
                    TextObject::InnerQuote(ch)
                } else {
                    TextObject::AQuote(ch)
                }
            }
            '(' | ')' => {
                if inner {
                    TextObject::InnerParen
                } else {
                    TextObject::AParen
                }
            }
            '{' | '}' => {
                if inner {
                    TextObject::InnerBrace
                } else {
                    TextObject::ABrace
                }
            }
            '[' | ']' => {
                if inner {
                    TextObject::InnerBracket
                } else {
                    TextObject::ABracket
                }
            }
            _ => return None,
        };

        if operator == Operator::Change {
            self.mode = VimMode::Insert;
        }
        Some(VimCommand::OperatorTextObject {
            operator,
            text_object,
            count,
        })
    }
}

impl Default for VimParser {
    fn default() -> Self {
        Self::new()
    }
}
