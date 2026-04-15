//! Built-in theme palettes: dark, light, solarized, monokai,
//! daltonized variants, and ANSI-16 fallbacks.

use super::tokens::SemanticTheme;
use ratatui::style::Color;

/// Named palette selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemePalette {
    Dark,
    Light,
    Solarized,
    Monokai,
    DarkDaltonized,
    LightDaltonized,
    DarkAnsi,
    LightAnsi,
}

impl ThemePalette {
    pub fn all() -> &'static [ThemePalette] {
        &[
            Self::Dark,
            Self::Light,
            Self::Solarized,
            Self::Monokai,
            Self::DarkDaltonized,
            Self::LightDaltonized,
            Self::DarkAnsi,
            Self::LightAnsi,
        ]
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::Solarized => "solarized",
            Self::Monokai => "monokai",
            Self::DarkDaltonized => "dark-daltonized",
            Self::LightDaltonized => "light-daltonized",
            Self::DarkAnsi => "dark-ansi",
            Self::LightAnsi => "light-ansi",
        }
    }

    pub fn build(&self) -> SemanticTheme {
        match self {
            Self::Dark => SemanticTheme::dark(),
            Self::Light => SemanticTheme::light(),
            Self::Solarized => SemanticTheme::solarized(),
            Self::Monokai => SemanticTheme::monokai(),
            Self::DarkDaltonized => SemanticTheme::dark_daltonized(),
            Self::LightDaltonized => SemanticTheme::light_daltonized(),
            Self::DarkAnsi => SemanticTheme::dark_ansi(),
            Self::LightAnsi => SemanticTheme::light_ansi(),
        }
    }
}

impl SemanticTheme {
    pub fn dark() -> Self {
        Self {
            bg: Color::Reset,
            fg: Color::White,
            accent: Color::Rgb(0, 200, 220),
            accent2: Color::Rgb(180, 100, 255),
            muted: Color::Rgb(100, 100, 100),
            border: Color::Rgb(60, 60, 60),
            border_active: Color::Rgb(0, 200, 220),
            success: Color::Rgb(80, 200, 80),
            warning: Color::Rgb(230, 180, 40),
            error: Color::Rgb(230, 60, 60),

            spinner_glyph: Color::Rgb(0, 200, 220),
            spinner_glyph_shimmer: Color::Rgb(40, 240, 255),
            spinner_label: Color::Rgb(140, 140, 140),
            spinner_stalled: Color::Rgb(230, 60, 60),
            streaming_cursor: Color::Rgb(0, 200, 220),

            diff_added: Color::Rgb(80, 200, 80),
            diff_removed: Color::Rgb(230, 60, 60),
            diff_context: Color::Rgb(140, 140, 140),
            diff_added_word: Color::Rgb(120, 255, 120),
            diff_removed_word: Color::Rgb(255, 100, 100),
            diff_added_bg: Color::Rgb(20, 40, 20),
            diff_removed_bg: Color::Rgb(40, 20, 20),

            user_message_bg: Color::Rgb(25, 30, 40),
            assistant_message_bg: Color::Reset,
            system_message_bg: Color::Rgb(30, 25, 35),
            bash_message_bg: Color::Rgb(20, 25, 20),
            tool_result_bg: Color::Rgb(22, 22, 30),

            agent_colors: [
                Color::Rgb(230, 80, 80),   // red
                Color::Rgb(80, 140, 230),  // blue
                Color::Rgb(80, 200, 80),   // green
                Color::Rgb(230, 200, 60),  // yellow
                Color::Rgb(180, 100, 255), // purple
                Color::Rgb(230, 140, 50),  // orange
                Color::Rgb(230, 130, 180), // pink
                Color::Rgb(0, 200, 220),   // cyan
            ],

            prompt_border: Color::Rgb(60, 60, 60),
            prompt_border_active: Color::Rgb(0, 200, 220),
            prompt_border_shimmer: Color::Rgb(40, 240, 255),
            suggestion_text: Color::Rgb(80, 80, 80),
            placeholder_text: Color::Rgb(100, 100, 100),

            phase_plan: Color::Rgb(140, 160, 200),
            phase_execute: Color::Rgb(0, 200, 220),
            phase_verify: Color::Rgb(180, 100, 255),
            phase_repair: Color::Rgb(230, 180, 40),

            rate_limit_fill: Color::Rgb(0, 200, 220),
            rate_limit_empty: Color::Rgb(40, 40, 40),
            cost_ok: Color::Rgb(80, 200, 80),
            cost_warn: Color::Rgb(230, 180, 40),
            cost_danger: Color::Rgb(230, 60, 60),
            token_ok: Color::Rgb(80, 200, 80),
            token_warn: Color::Rgb(230, 180, 40),
            token_danger: Color::Rgb(230, 60, 60),

            effort_low: Color::Rgb(100, 100, 100),
            effort_medium: Color::Rgb(0, 200, 220),
            effort_high: Color::Rgb(180, 100, 255),
            effort_max: Color::Rgb(230, 60, 60),

            reduced_motion: false,
            high_contrast: false,
        }
    }

    pub fn light() -> Self {
        Self {
            bg: Color::Rgb(255, 255, 255),
            fg: Color::Rgb(30, 30, 30),
            accent: Color::Rgb(0, 100, 180),
            accent2: Color::Rgb(140, 60, 200),
            muted: Color::Rgb(140, 140, 140),
            border: Color::Rgb(200, 200, 200),
            border_active: Color::Rgb(0, 100, 180),
            success: Color::Rgb(30, 140, 30),
            warning: Color::Rgb(180, 130, 0),
            error: Color::Rgb(200, 40, 40),

            spinner_glyph: Color::Rgb(0, 100, 180),
            spinner_glyph_shimmer: Color::Rgb(60, 160, 230),
            spinner_label: Color::Rgb(120, 120, 120),
            spinner_stalled: Color::Rgb(200, 40, 40),
            streaming_cursor: Color::Rgb(0, 100, 180),

            diff_added: Color::Rgb(30, 140, 30),
            diff_removed: Color::Rgb(200, 40, 40),
            diff_context: Color::Rgb(120, 120, 120),
            diff_added_word: Color::Rgb(0, 100, 0),
            diff_removed_word: Color::Rgb(160, 0, 0),
            diff_added_bg: Color::Rgb(220, 255, 220),
            diff_removed_bg: Color::Rgb(255, 220, 220),

            user_message_bg: Color::Rgb(235, 240, 250),
            assistant_message_bg: Color::Rgb(255, 255, 255),
            system_message_bg: Color::Rgb(245, 238, 250),
            bash_message_bg: Color::Rgb(238, 245, 238),
            tool_result_bg: Color::Rgb(240, 240, 248),

            agent_colors: [
                Color::Rgb(200, 50, 50),
                Color::Rgb(30, 100, 200),
                Color::Rgb(30, 140, 30),
                Color::Rgb(180, 150, 0),
                Color::Rgb(140, 60, 200),
                Color::Rgb(200, 110, 20),
                Color::Rgb(200, 80, 140),
                Color::Rgb(0, 140, 160),
            ],

            prompt_border: Color::Rgb(200, 200, 200),
            prompt_border_active: Color::Rgb(0, 100, 180),
            prompt_border_shimmer: Color::Rgb(60, 160, 230),
            suggestion_text: Color::Rgb(180, 180, 180),
            placeholder_text: Color::Rgb(160, 160, 160),

            phase_plan: Color::Rgb(80, 100, 150),
            phase_execute: Color::Rgb(0, 100, 180),
            phase_verify: Color::Rgb(140, 60, 200),
            phase_repair: Color::Rgb(180, 130, 0),

            rate_limit_fill: Color::Rgb(0, 100, 180),
            rate_limit_empty: Color::Rgb(220, 220, 220),
            cost_ok: Color::Rgb(30, 140, 30),
            cost_warn: Color::Rgb(180, 130, 0),
            cost_danger: Color::Rgb(200, 40, 40),
            token_ok: Color::Rgb(30, 140, 30),
            token_warn: Color::Rgb(180, 130, 0),
            token_danger: Color::Rgb(200, 40, 40),

            effort_low: Color::Rgb(160, 160, 160),
            effort_medium: Color::Rgb(0, 100, 180),
            effort_high: Color::Rgb(140, 60, 200),
            effort_max: Color::Rgb(200, 40, 40),

            reduced_motion: false,
            high_contrast: false,
        }
    }

    pub fn solarized() -> Self {
        let base03 = Color::Rgb(0, 43, 54);
        let base0 = Color::Rgb(131, 148, 150);
        let base01 = Color::Rgb(88, 110, 117);
        let blue = Color::Rgb(38, 139, 210);
        let cyan = Color::Rgb(42, 161, 152);
        let green = Color::Rgb(133, 153, 0);
        let yellow = Color::Rgb(181, 137, 0);
        let red = Color::Rgb(220, 50, 47);
        let magenta = Color::Rgb(211, 54, 130);
        let violet = Color::Rgb(108, 113, 196);
        let orange = Color::Rgb(203, 75, 22);

        let mut t = Self::dark();
        t.bg = base03;
        t.fg = base0;
        t.accent = blue;
        t.accent2 = magenta;
        t.muted = base01;
        t.border = base01;
        t.border_active = blue;
        t.success = green;
        t.warning = yellow;
        t.error = red;
        t.spinner_glyph = cyan;
        t.spinner_glyph_shimmer = SemanticTheme::shimmer(cyan);
        t.spinner_stalled = red;
        t.diff_added = green;
        t.diff_removed = red;
        t.diff_added_word = Color::Rgb(173, 193, 40);
        t.diff_removed_word = Color::Rgb(255, 90, 87);
        t.phase_plan = base01;
        t.phase_execute = blue;
        t.phase_verify = violet;
        t.phase_repair = orange;
        t.agent_colors = [red, blue, green, yellow, violet, orange, magenta, cyan];
        t
    }

    pub fn monokai() -> Self {
        let bg = Color::Rgb(39, 40, 34);
        let fg = Color::Rgb(248, 248, 242);
        let pink = Color::Rgb(249, 38, 114);
        let green = Color::Rgb(166, 226, 46);
        let yellow = Color::Rgb(230, 219, 116);
        let orange = Color::Rgb(253, 151, 31);
        let purple = Color::Rgb(174, 129, 255);
        let cyan = Color::Rgb(102, 217, 239);
        let muted = Color::Rgb(117, 113, 94);

        let mut t = Self::dark();
        t.bg = bg;
        t.fg = fg;
        t.accent = cyan;
        t.accent2 = purple;
        t.muted = muted;
        t.border = muted;
        t.border_active = cyan;
        t.success = green;
        t.warning = orange;
        t.error = pink;
        t.spinner_glyph = cyan;
        t.spinner_glyph_shimmer = SemanticTheme::shimmer(cyan);
        t.spinner_stalled = pink;
        t.diff_added = green;
        t.diff_removed = pink;
        t.diff_added_word = Color::Rgb(200, 255, 86);
        t.diff_removed_word = Color::Rgb(255, 80, 150);
        t.phase_plan = muted;
        t.phase_execute = cyan;
        t.phase_verify = purple;
        t.phase_repair = orange;
        t.agent_colors = [pink, cyan, green, yellow, purple, orange, fg, Color::Rgb(200, 200, 200)];
        t
    }

    /// Dark palette with red/green replaced by orange/blue for color-blind users.
    pub fn dark_daltonized() -> Self {
        let mut t = Self::dark();
        t.success = Color::Rgb(100, 149, 237);  // cornflower blue
        t.error = Color::Rgb(255, 165, 0);       // orange
        t.diff_added = Color::Rgb(100, 149, 237);
        t.diff_removed = Color::Rgb(255, 165, 0);
        t.diff_added_word = Color::Rgb(140, 180, 255);
        t.diff_removed_word = Color::Rgb(255, 200, 80);
        t.diff_added_bg = Color::Rgb(15, 20, 35);
        t.diff_removed_bg = Color::Rgb(40, 30, 15);
        t.spinner_stalled = Color::Rgb(255, 165, 0);
        t.cost_ok = Color::Rgb(100, 149, 237);
        t.cost_danger = Color::Rgb(255, 165, 0);
        t.token_ok = Color::Rgb(100, 149, 237);
        t.token_danger = Color::Rgb(255, 165, 0);
        t.agent_colors[0] = Color::Rgb(255, 165, 0);  // orange instead of red
        t.agent_colors[2] = Color::Rgb(100, 149, 237); // blue instead of green
        t
    }

    /// Light palette with daltonized colors.
    pub fn light_daltonized() -> Self {
        let mut t = Self::light();
        t.success = Color::Rgb(50, 100, 200);
        t.error = Color::Rgb(200, 120, 0);
        t.diff_added = Color::Rgb(50, 100, 200);
        t.diff_removed = Color::Rgb(200, 120, 0);
        t.diff_added_word = Color::Rgb(30, 70, 160);
        t.diff_removed_word = Color::Rgb(180, 100, 0);
        t.diff_added_bg = Color::Rgb(220, 230, 250);
        t.diff_removed_bg = Color::Rgb(255, 235, 210);
        t.spinner_stalled = Color::Rgb(200, 120, 0);
        t
    }

    /// Dark palette using only ANSI-16 colors (for restricted terminals/SSH).
    pub fn dark_ansi() -> Self {
        Self {
            bg: Color::Reset,
            fg: Color::White,
            accent: Color::Cyan,
            accent2: Color::Magenta,
            muted: Color::DarkGray,
            border: Color::DarkGray,
            border_active: Color::Cyan,
            success: Color::Green,
            warning: Color::Yellow,
            error: Color::Red,

            spinner_glyph: Color::Cyan,
            spinner_glyph_shimmer: Color::White,
            spinner_label: Color::DarkGray,
            spinner_stalled: Color::Red,
            streaming_cursor: Color::Cyan,

            diff_added: Color::Green,
            diff_removed: Color::Red,
            diff_context: Color::DarkGray,
            diff_added_word: Color::Green,
            diff_removed_word: Color::Red,
            diff_added_bg: Color::Reset,
            diff_removed_bg: Color::Reset,

            user_message_bg: Color::Reset,
            assistant_message_bg: Color::Reset,
            system_message_bg: Color::Reset,
            bash_message_bg: Color::Reset,
            tool_result_bg: Color::Reset,

            agent_colors: [
                Color::Red,
                Color::Blue,
                Color::Green,
                Color::Yellow,
                Color::Magenta,
                Color::Cyan,
                Color::White,
                Color::DarkGray,
            ],

            prompt_border: Color::DarkGray,
            prompt_border_active: Color::Cyan,
            prompt_border_shimmer: Color::White,
            suggestion_text: Color::DarkGray,
            placeholder_text: Color::DarkGray,

            phase_plan: Color::DarkGray,
            phase_execute: Color::Cyan,
            phase_verify: Color::Magenta,
            phase_repair: Color::Yellow,

            rate_limit_fill: Color::Cyan,
            rate_limit_empty: Color::DarkGray,
            cost_ok: Color::Green,
            cost_warn: Color::Yellow,
            cost_danger: Color::Red,
            token_ok: Color::Green,
            token_warn: Color::Yellow,
            token_danger: Color::Red,

            effort_low: Color::DarkGray,
            effort_medium: Color::Cyan,
            effort_high: Color::Magenta,
            effort_max: Color::Red,

            reduced_motion: false,
            high_contrast: false,
        }
    }

    /// Light palette using only ANSI-16 colors.
    pub fn light_ansi() -> Self {
        let mut t = Self::dark_ansi();
        t.bg = Color::White;
        t.fg = Color::Black;
        t.accent = Color::Blue;
        t.muted = Color::Gray;
        t.border = Color::Gray;
        t.border_active = Color::Blue;
        t.prompt_border = Color::Gray;
        t.prompt_border_active = Color::Blue;
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_palettes_build() {
        for p in ThemePalette::all() {
            let _ = p.build();
        }
    }

    #[test]
    fn palette_names_unique() {
        let names: Vec<&str> = ThemePalette::all().iter().map(|p| p.name()).collect();
        let mut deduped = names.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(names.len(), deduped.len());
    }

    #[test]
    fn dark_has_8_agent_colors() {
        let t = SemanticTheme::dark();
        assert_eq!(t.agent_colors.len(), 8);
    }

    #[test]
    fn daltonized_avoids_red_green() {
        let t = SemanticTheme::dark_daltonized();
        // Success should not be green
        assert_ne!(t.success, Color::Green);
        assert_ne!(t.success, Color::Rgb(80, 200, 80));
    }
}
