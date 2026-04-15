//! Semantic theme token system.
//!
//! 50+ named tokens covering every visual surface in the TUI.
//! Each palette provides all tokens; unknown tokens fall back to the base set.

pub mod palettes;
pub mod tokens;

pub use palettes::ThemePalette;
pub use tokens::SemanticTheme;
