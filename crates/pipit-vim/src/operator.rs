//! Vim operators — the "verb" in operator-motion composition.

/// An operator that acts on a range defined by a motion or text object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Delete,
    Change,
    Yank,
}
