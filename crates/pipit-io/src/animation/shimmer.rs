//! T2: Shimmer animation — traveling highlight across spinner/streaming text.
//!
//! A sine-based "wave" of brightness sweeps left-to-right across a
//! span of cells, creating a subtle shimmer effect. Reduced-motion
//! mode degrades to a simple static highlight.

use ratatui::style::Color;

/// Shimmer engine: computes per-character brightness multipliers.
///
/// # Usage
///
/// ```ignore
/// let engine = ShimmerEngine::new(0.8, 6); // 80% speed, 6-char window
/// let factors = engine.factors(frame, text_len);
/// // factors[i] is 0.0..=1.0 brightness boost for character i
/// ```
#[derive(Debug, Clone)]
pub struct ShimmerEngine {
    /// Speed multiplier (1.0 = normal, lower = slower).
    pub speed: f32,
    /// Width of the shimmer wave in characters.
    pub window: usize,
    /// Whether shimmer is disabled (reduced motion).
    pub disabled: bool,
}

impl Default for ShimmerEngine {
    fn default() -> Self {
        Self {
            speed: 1.0,
            window: 6,
            disabled: false,
        }
    }
}

impl ShimmerEngine {
    pub fn new(speed: f32, window: usize) -> Self {
        Self {
            speed,
            window: window.max(1),
            disabled: false,
        }
    }

    /// Compute per-character shimmer factors for a given frame and text length.
    ///
    /// Returns a `Vec<f32>` of length `text_len`, where each value is in [0.0, 1.0].
    /// 0.0 = no shimmer, 1.0 = full shimmer brightness.
    pub fn factors(&self, frame: u64, text_len: usize) -> Vec<f32> {
        if self.disabled || text_len == 0 {
            return vec![0.0; text_len];
        }

        let total = text_len + self.window;
        // Position of the wave center, advancing with each frame
        let pos = ((frame as f32) * self.speed) % (total as f32);

        (0..text_len)
            .map(|i| {
                let dist = (i as f32 - pos).abs();
                let half_w = self.window as f32 / 2.0;
                if dist < half_w {
                    // Cosine falloff from center of wave
                    let t = dist / half_w;
                    0.5 * (1.0 + (std::f32::consts::PI * t).cos())
                } else {
                    0.0
                }
            })
            .collect()
    }

    /// Apply shimmer to a base color at a given factor (0.0..=1.0).
    ///
    /// Blends toward a brighter version of the color by `factor * boost`.
    pub fn apply_color(base: Color, shimmer_target: Color, factor: f32) -> Color {
        if factor <= 0.001 {
            return base;
        }
        crate::theme::SemanticTheme::lerp(base, shimmer_target, factor)
    }

    /// Convenience: apply shimmer to a text string, returning per-char colors.
    pub fn shimmer_colors(
        &self,
        frame: u64,
        text: &str,
        base: Color,
        shimmer_target: Color,
    ) -> Vec<Color> {
        let char_count = text.chars().count();
        self.factors(frame, char_count)
            .into_iter()
            .map(|f| Self::apply_color(base, shimmer_target, f))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factors_length_matches_text() {
        let eng = ShimmerEngine::default();
        let f = eng.factors(0, 10);
        assert_eq!(f.len(), 10);
    }

    #[test]
    fn disabled_returns_zeros() {
        let mut eng = ShimmerEngine::default();
        eng.disabled = true;
        let f = eng.factors(42, 5);
        assert!(f.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn factors_in_range() {
        let eng = ShimmerEngine::new(1.0, 4);
        for frame in 0..100 {
            for f in eng.factors(frame, 20) {
                assert!((0.0..=1.0).contains(&f), "factor out of range: {f}");
            }
        }
    }

    #[test]
    fn shimmer_moves_over_time() {
        let eng = ShimmerEngine::new(1.0, 4);
        let f0 = eng.factors(0, 20);
        let f10 = eng.factors(10, 20);
        // The peak should be at different positions
        let peak0 = f0.iter().enumerate().max_by(|a, b| a.1.partial_cmp(b.1).unwrap()).unwrap().0;
        let peak10 = f10.iter().enumerate().max_by(|a, b| a.1.partial_cmp(b.1).unwrap()).unwrap().0;
        assert_ne!(peak0, peak10);
    }

    #[test]
    fn apply_color_identity_at_zero() {
        let base = Color::Rgb(100, 100, 100);
        let target = Color::Rgb(200, 200, 200);
        let result = ShimmerEngine::apply_color(base, target, 0.0);
        assert_eq!(result, base);
    }
}
