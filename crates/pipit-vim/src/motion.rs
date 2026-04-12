//! Vim motions — the "noun" in operator-motion composition.

/// A motion that moves the cursor.
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
