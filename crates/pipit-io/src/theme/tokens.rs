//! Semantic color tokens for every TUI surface.
//!
//! Each token names a specific visual role. Shimmer companions are
//! auto-derived by brightening the base color (+40 per channel, clamped).

use ratatui::style::Color;

/// All semantic color tokens for the TUI.
#[derive(Debug, Clone)]
pub struct SemanticTheme {
    // ── Core ───────────────────────────────────────────
    pub bg: Color,
    pub fg: Color,
    pub accent: Color,
    pub accent2: Color,
    pub muted: Color,
    pub border: Color,
    pub border_active: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,

    // ── Spinner & streaming ───────────────────────────
    pub spinner_glyph: Color,
    pub spinner_glyph_shimmer: Color,
    pub spinner_label: Color,
    pub spinner_stalled: Color,
    pub streaming_cursor: Color,

    // ── Diff (word-level) ─────────────────────────────
    pub diff_added: Color,
    pub diff_removed: Color,
    pub diff_context: Color,
    pub diff_added_word: Color,
    pub diff_removed_word: Color,
    pub diff_added_bg: Color,
    pub diff_removed_bg: Color,

    // ── Messages ──────────────────────────────────────
    pub user_message_bg: Color,
    pub assistant_message_bg: Color,
    pub system_message_bg: Color,
    pub bash_message_bg: Color,
    pub tool_result_bg: Color,

    // ── Agent identity (8 cycling colors) ─────────────
    pub agent_colors: [Color; 8],

    // ── Input ─────────────────────────────────────────
    pub prompt_border: Color,
    pub prompt_border_active: Color,
    pub prompt_border_shimmer: Color,
    pub suggestion_text: Color,
    pub placeholder_text: Color,

    // ── PEV phases ────────────────────────────────────
    pub phase_plan: Color,
    pub phase_execute: Color,
    pub phase_verify: Color,
    pub phase_repair: Color,

    // ── Status / budget ───────────────────────────────
    pub rate_limit_fill: Color,
    pub rate_limit_empty: Color,
    pub cost_ok: Color,
    pub cost_warn: Color,
    pub cost_danger: Color,
    pub token_ok: Color,
    pub token_warn: Color,
    pub token_danger: Color,

    // ── Effort / thinking ─────────────────────────────
    pub effort_low: Color,
    pub effort_medium: Color,
    pub effort_high: Color,
    pub effort_max: Color,

    // ── Accessibility flags ───────────────────────────
    pub reduced_motion: bool,
    pub high_contrast: bool,
}

impl SemanticTheme {
    /// Auto-derive shimmer color: brighten by +40 per channel.
    pub fn shimmer(color: Color) -> Color {
        match color {
            Color::Rgb(r, g, b) => Color::Rgb(
                r.saturating_add(40),
                g.saturating_add(40),
                b.saturating_add(40),
            ),
            // For indexed/named colors, just return White as shimmer
            _ => Color::White,
        }
    }

    /// Daltonize a color for deuteranopia (Brettel 1997 simplified).
    /// Replaces red/green confusion colors with orange/blue.
    pub fn daltonize(color: Color) -> Color {
        match color {
            Color::Rgb(r, g, b) => {
                // Brettel simulation: shift red→orange, green→blue
                let rf = r as f32 / 255.0;
                let gf = g as f32 / 255.0;
                let bf = b as f32 / 255.0;

                // Simplified deuteranopia matrix
                let nr = 0.625 * rf + 0.375 * gf;
                let ng = 0.7 * gf + 0.3 * bf;
                let nb = 0.3 * gf + 0.7 * bf;

                Color::Rgb(
                    (nr.clamp(0.0, 1.0) * 255.0) as u8,
                    (ng.clamp(0.0, 1.0) * 255.0) as u8,
                    (nb.clamp(0.0, 1.0) * 255.0) as u8,
                )
            }
            Color::Red => Color::Rgb(255, 165, 0),    // orange
            Color::Green => Color::Rgb(100, 149, 237), // cornflower blue
            _ => color,
        }
    }

    /// Compute WCAG relative luminance.
    pub fn luminance(color: Color) -> f32 {
        match color {
            Color::Rgb(r, g, b) => {
                let srgb = |c: u8| {
                    let c = c as f32 / 255.0;
                    if c <= 0.03928 {
                        c / 12.92
                    } else {
                        ((c + 0.055) / 1.055).powf(2.4)
                    }
                };
                0.2126 * srgb(r) + 0.7152 * srgb(g) + 0.0722 * srgb(b)
            }
            _ => 0.5, // fallback for named colors
        }
    }

    /// WCAG contrast ratio between two colors.
    pub fn contrast_ratio(c1: Color, c2: Color) -> f32 {
        let l1 = Self::luminance(c1);
        let l2 = Self::luminance(c2);
        let (lighter, darker) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
        (lighter + 0.05) / (darker + 0.05)
    }

    /// Lerp between two RGB colors. Falls back to `to` for non-RGB.
    pub fn lerp(from: Color, to: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        match (from, to) {
            (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => Color::Rgb(
                (r1 as f32 + (r2 as f32 - r1 as f32) * t) as u8,
                (g1 as f32 + (g2 as f32 - g1 as f32) * t) as u8,
                (b1 as f32 + (b2 as f32 - b1 as f32) * t) as u8,
            ),
            _ => if t > 0.5 { to } else { from },
        }
    }
}

impl Default for SemanticTheme {
    fn default() -> Self {
        Self::dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shimmer_brightens() {
        assert_eq!(
            SemanticTheme::shimmer(Color::Rgb(100, 100, 100)),
            Color::Rgb(140, 140, 140)
        );
    }

    #[test]
    fn shimmer_clamps_at_255() {
        assert_eq!(
            SemanticTheme::shimmer(Color::Rgb(240, 250, 230)),
            Color::Rgb(255, 255, 255)
        );
    }

    #[test]
    fn contrast_ratio_black_white() {
        let cr = SemanticTheme::contrast_ratio(
            Color::Rgb(0, 0, 0),
            Color::Rgb(255, 255, 255),
        );
        assert!(cr > 20.0); // Should be ~21:1
    }

    #[test]
    fn lerp_midpoint() {
        let mid = SemanticTheme::lerp(
            Color::Rgb(0, 0, 0),
            Color::Rgb(200, 100, 50),
            0.5,
        );
        assert_eq!(mid, Color::Rgb(100, 50, 25));
    }

    #[test]
    fn daltonize_red_to_orange() {
        let d = SemanticTheme::daltonize(Color::Red);
        assert_eq!(d, Color::Rgb(255, 165, 0));
    }
}
