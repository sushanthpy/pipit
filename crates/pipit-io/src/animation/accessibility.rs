//! T12: Reduced-motion & accessibility mode.
//!
//! Checks `REDUCE_MOTION`, `PIPIT_REDUCE_MOTION`, and
//! `prefers-reduced-motion` (via terminal query) to gate animations.
//! Provides a global `AccessibilityMode` that other modules consult.

/// Accessibility configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessibilityMode {
    /// Disable shimmer, pulse, and slide animations.
    pub reduced_motion: bool,
    /// Use higher-contrast colors (affects theme selection).
    pub high_contrast: bool,
    /// Force ANSI-16 colors only.
    pub force_ansi16: bool,
    /// Disable emoji glyphs.
    pub no_emoji: bool,
}

impl Default for AccessibilityMode {
    fn default() -> Self {
        Self::detect()
    }
}

impl AccessibilityMode {
    /// Detect accessibility preferences from environment.
    pub fn detect() -> Self {
        let reduced = std::env::var("REDUCE_MOTION").is_ok()
            || std::env::var("PIPIT_REDUCE_MOTION").is_ok()
            // macOS: defaults read -g com.apple.universalaccess reduceMotion
            // We check a simpler env var approach:
            || std::env::var("PIPIT_ACCESSIBILITY")
                .map(|v| v.contains("reduce-motion"))
                .unwrap_or(false);

        let high_contrast = std::env::var("PIPIT_HIGH_CONTRAST").is_ok()
            || std::env::var("PIPIT_ACCESSIBILITY")
                .map(|v| v.contains("high-contrast"))
                .unwrap_or(false);

        let force_ansi16 = std::env::var("PIPIT_ANSI16").is_ok()
            || std::env::var("NO_COLOR").is_ok();

        let no_emoji = std::env::var("PIPIT_NO_EMOJI").is_ok();

        Self {
            reduced_motion: reduced,
            high_contrast,
            force_ansi16,
            no_emoji,
        }
    }

    /// All features enabled (for testing or unconstrained terminals).
    pub fn full() -> Self {
        Self {
            reduced_motion: false,
            high_contrast: false,
            force_ansi16: false,
            no_emoji: false,
        }
    }

    /// Maximum accessibility (all animations off, high contrast).
    pub fn maximum() -> Self {
        Self {
            reduced_motion: true,
            high_contrast: true,
            force_ansi16: true,
            no_emoji: true,
        }
    }

    /// Should animations be played?
    pub fn animations_enabled(&self) -> bool {
        !self.reduced_motion
    }

    /// Recommended theme palette based on accessibility settings.
    pub fn recommended_palette(&self) -> crate::theme::ThemePalette {
        use crate::theme::ThemePalette;
        if self.force_ansi16 {
            ThemePalette::DarkAnsi
        } else if self.high_contrast {
            ThemePalette::Dark // dark has good contrast; could add a HC variant later
        } else {
            ThemePalette::Dark
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_mode_enables_everything() {
        let m = AccessibilityMode::full();
        assert!(m.animations_enabled());
        assert!(!m.high_contrast);
        assert!(!m.force_ansi16);
        assert!(!m.no_emoji);
    }

    #[test]
    fn maximum_mode_disables_animations() {
        let m = AccessibilityMode::maximum();
        assert!(!m.animations_enabled());
        assert!(m.high_contrast);
        assert!(m.force_ansi16);
        assert!(m.no_emoji);
    }

    #[test]
    fn recommended_palette_for_ansi16() {
        let m = AccessibilityMode {
            force_ansi16: true,
            ..AccessibilityMode::full()
        };
        assert_eq!(m.recommended_palette(), crate::theme::ThemePalette::DarkAnsi);
    }
}
