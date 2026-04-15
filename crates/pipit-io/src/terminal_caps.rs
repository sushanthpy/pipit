//! T10: Terminal capability detection & glyph selection.
//!
//! Detects the terminal's color depth and Unicode support via
//! environment variables and adapts glyphs/colors accordingly.

/// Detected terminal capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCaps {
    pub color_depth: ColorDepth,
    pub unicode_support: UnicodeSupport,
    pub is_ssh: bool,
    pub is_tmux: bool,
    pub is_screen: bool,
}

/// Color depth tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColorDepth {
    /// No color (NO_COLOR set, or dumb terminal).
    None,
    /// ANSI 16 colors.
    Ansi16,
    /// 256-color palette.
    Ansi256,
    /// Full 24-bit truecolor.
    TrueColor,
}

/// Unicode capability tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnicodeSupport {
    /// ASCII only (e.g. LANG=C, dumb terminal).
    Ascii,
    /// Basic Unicode but limited emoji/combining chars.
    Basic,
    /// Full Unicode including emoji.
    Full,
}

impl Default for TerminalCaps {
    fn default() -> Self {
        Self::detect()
    }
}

impl TerminalCaps {
    /// Detect terminal capabilities from environment variables.
    pub fn detect() -> Self {
        let term = std::env::var("TERM").unwrap_or_default();
        let colorterm = std::env::var("COLORTERM").unwrap_or_default();
        let no_color = std::env::var("NO_COLOR").is_ok();
        let lang = std::env::var("LANG").unwrap_or_default();
        let ssh = std::env::var("SSH_CONNECTION").is_ok() || std::env::var("SSH_TTY").is_ok();
        let tmux = std::env::var("TMUX").is_ok();
        let screen = term.contains("screen");

        let color_depth = if no_color || term == "dumb" {
            ColorDepth::None
        } else if colorterm == "truecolor" || colorterm == "24bit" || term.contains("256color") {
            // Many modern terminals
            if colorterm == "truecolor" || colorterm == "24bit" {
                ColorDepth::TrueColor
            } else {
                ColorDepth::Ansi256
            }
        } else if term.starts_with("xterm") || term.starts_with("rxvt") || tmux {
            ColorDepth::Ansi256
        } else if !term.is_empty() && term != "dumb" {
            ColorDepth::Ansi16
        } else {
            ColorDepth::None
        };

        let unicode_support = if lang.contains("UTF-8") || lang.contains("utf-8") || lang.contains("UTF8") {
            if ssh || screen {
                UnicodeSupport::Basic
            } else {
                UnicodeSupport::Full
            }
        } else if term == "dumb" || term.is_empty() {
            UnicodeSupport::Ascii
        } else {
            UnicodeSupport::Basic
        };

        Self {
            color_depth,
            unicode_support,
            is_ssh: ssh,
            is_tmux: tmux,
            is_screen: screen,
        }
    }

    /// Whether we can use RGB colors.
    pub fn has_truecolor(&self) -> bool {
        self.color_depth >= ColorDepth::TrueColor
    }

    /// Whether we can use 256 colors.
    pub fn has_256_colors(&self) -> bool {
        self.color_depth >= ColorDepth::Ansi256
    }

    /// Whether Unicode box-drawing / braille is safe.
    pub fn has_unicode(&self) -> bool {
        self.unicode_support >= UnicodeSupport::Basic
    }

    /// Whether emoji are safe.
    pub fn has_emoji(&self) -> bool {
        self.unicode_support >= UnicodeSupport::Full
    }
}

/// Glyph set adapted to terminal capabilities.
#[derive(Debug, Clone)]
pub struct GlyphSet {
    pub spinner_frames: &'static [&'static str],
    pub check: &'static str,
    pub cross: &'static str,
    pub arrow_right: &'static str,
    pub bullet: &'static str,
    pub border_h: &'static str,
    pub border_v: &'static str,
    pub corner_tl: &'static str,
    pub corner_tr: &'static str,
    pub corner_bl: &'static str,
    pub corner_br: &'static str,
    pub diff_add: &'static str,
    pub diff_del: &'static str,
    pub ellipsis: &'static str,
    pub bar_filled: &'static str,
    pub bar_empty: &'static str,
}

impl GlyphSet {
    /// Select glyphs based on terminal capabilities.
    pub fn for_caps(caps: &TerminalCaps) -> Self {
        if caps.has_unicode() {
            Self::unicode()
        } else {
            Self::ascii()
        }
    }

    /// Full Unicode glyph set.
    pub fn unicode() -> Self {
        Self {
            spinner_frames: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
            check: "✓",
            cross: "✗",
            arrow_right: "→",
            bullet: "•",
            border_h: "─",
            border_v: "│",
            corner_tl: "╭",
            corner_tr: "╮",
            corner_bl: "╰",
            corner_br: "╯",
            diff_add: "+",
            diff_del: "-",
            ellipsis: "…",
            bar_filled: "█",
            bar_empty: "░",
        }
    }

    /// ASCII-only glyph set.
    pub fn ascii() -> Self {
        Self {
            spinner_frames: &["|", "/", "-", "\\"],
            check: "[ok]",
            cross: "[x]",
            arrow_right: "->",
            bullet: "*",
            border_h: "-",
            border_v: "|",
            corner_tl: "+",
            corner_tr: "+",
            corner_bl: "+",
            corner_br: "+",
            diff_add: "+",
            diff_del: "-",
            ellipsis: "...",
            bar_filled: "#",
            bar_empty: ".",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_glyph_set_uses_ascii_only() {
        let gs = GlyphSet::ascii();
        assert!(gs.check.is_ascii());
        assert!(gs.cross.is_ascii());
        assert!(gs.border_h.is_ascii());
    }

    #[test]
    fn unicode_glyph_set_has_special_chars() {
        let gs = GlyphSet::unicode();
        assert_eq!(gs.check, "✓");
        assert_eq!(gs.ellipsis, "…");
    }

    #[test]
    fn color_depth_ordering() {
        assert!(ColorDepth::TrueColor > ColorDepth::Ansi256);
        assert!(ColorDepth::Ansi256 > ColorDepth::Ansi16);
        assert!(ColorDepth::Ansi16 > ColorDepth::None);
    }
}
