//! Parsed Vim commands ready for execution.

use crate::motion::Motion;
use crate::operator::Operator;

/// Text object targets for operator+text-object commands (diw, ci", etc.).
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

/// A fully parsed Vim command ready for execution.
#[derive(Debug, Clone, PartialEq)]
pub enum VimCommand {
    /// Enter insert mode at cursor.
    EnterInsert,
    /// Enter insert mode after cursor.
    Append,
    /// Enter insert mode at end of line.
    AppendEnd,
    /// Enter insert mode at first non-blank.
    InsertLineStart,
    /// Return to normal mode.
    EnterNormal,
    /// Simple motion.
    Move { motion: Motion, count: u32 },
    /// Operator + motion.
    OperatorMotion {
        operator: Operator,
        motion: Motion,
        count: u32,
    },
    /// Operator on whole line (dd, yy, cc).
    OperatorLine { operator: Operator, count: u32 },
    /// Operator + text object (diw, ci", etc.).
    OperatorTextObject {
        operator: Operator,
        text_object: TextObject,
        count: u32,
    },
    /// Delete char under cursor (x).
    DeleteChar { count: u32 },
    /// Delete char before cursor (X).
    DeleteCharBefore { count: u32 },
    /// Open line below and enter insert.
    OpenBelow,
    /// Open line above and enter insert.
    OpenAbove,
    /// Paste from register.
    Paste { before: bool },
    /// Dot-repeat last change.
    DotRepeat,
    /// Repeat last find (;).
    RepeatFind,
    /// Reverse repeat last find (,).
    ReverseFind,
    /// Undo.
    Undo,
    /// Redo.
    Redo,
    /// Join current line with next.
    JoinLines,
    /// Replace char under cursor (r<char>).
    ReplaceChar(char),
    /// Toggle case of char under cursor (~).
    ToggleCase,
    /// Indent line right (>>).
    IndentRight { count: u32 },
    /// Indent line left (<<).
    IndentLeft { count: u32 },
}
