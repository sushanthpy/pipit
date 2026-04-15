//! T3: Stalled-stream detection with exponential red fade.
//!
//! Monitors token flow and detects when an LLM stream appears stalled.
//! After a configurable quiet period, the spinner color fades from its
//! normal value toward `spinner_stalled` (red) on an exponential curve.

use ratatui::style::Color;
use std::time::{Duration, Instant};

/// Detects stalled streams and provides a fade progress toward the stalled color.
#[derive(Debug, Clone)]
pub struct StalledDetector {
    /// Last time a token was received.
    last_token_at: Instant,
    /// Smoothed tokens-per-second (EMA).
    tokens_per_sec: f32,
    /// Duration of quiet before considering the stream stalled.
    stall_threshold: Duration,
    /// Duration over which the red fade reaches full intensity.
    fade_duration: Duration,
    /// Total token count (for rate tracking).
    total_tokens: u64,
}

impl Default for StalledDetector {
    fn default() -> Self {
        Self {
            last_token_at: Instant::now(),
            tokens_per_sec: 0.0,
            stall_threshold: Duration::from_secs(5),
            fade_duration: Duration::from_secs(10),
            total_tokens: 0,
        }
    }
}

impl StalledDetector {
    pub fn new(stall_threshold: Duration, fade_duration: Duration) -> Self {
        Self {
            stall_threshold,
            fade_duration,
            ..Default::default()
        }
    }

    /// Record that tokens were received. Call this for each chunk/token batch.
    pub fn record_tokens(&mut self, count: u64) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_token_at).as_secs_f32();
        if elapsed > 0.01 {
            let instant_rate = count as f32 / elapsed;
            // Exponential moving average (α = 0.3)
            self.tokens_per_sec = 0.3 * instant_rate + 0.7 * self.tokens_per_sec;
        }
        self.last_token_at = now;
        self.total_tokens += count;
    }

    /// Reset the detector (e.g. when a new stream starts).
    pub fn reset(&mut self) {
        self.last_token_at = Instant::now();
        self.tokens_per_sec = 0.0;
        self.total_tokens = 0;
    }

    /// How long since the last token was received.
    pub fn quiet_duration(&self) -> Duration {
        Instant::now().duration_since(self.last_token_at)
    }

    /// Whether the stream is considered stalled.
    pub fn is_stalled(&self) -> bool {
        self.quiet_duration() > self.stall_threshold
    }

    /// Stalled fade progress: 0.0 = normal, 1.0 = fully stalled.
    ///
    /// Uses an exponential ease-in curve: `(t/fade_duration)^2` where
    /// t is time past the stall threshold.
    pub fn fade_progress(&self) -> f32 {
        let quiet = self.quiet_duration();
        if quiet <= self.stall_threshold {
            return 0.0;
        }
        let past_threshold = (quiet - self.stall_threshold).as_secs_f32();
        let max = self.fade_duration.as_secs_f32();
        let t = (past_threshold / max).min(1.0);
        // Exponential ease-in
        t * t
    }

    /// Compute the current spinner color, fading from `normal` to `stalled`.
    pub fn spinner_color(&self, normal: Color, stalled: Color) -> Color {
        let progress = self.fade_progress();
        if progress <= 0.001 {
            return normal;
        }
        crate::theme::SemanticTheme::lerp(normal, stalled, progress)
    }

    /// Current smoothed tokens-per-second rate.
    pub fn rate(&self) -> f32 {
        self.tokens_per_sec
    }

    /// Total tokens recorded.
    pub fn total(&self) -> u64 {
        self.total_tokens
    }

    /// Human-readable status label.
    pub fn status_label(&self) -> &'static str {
        if self.is_stalled() {
            let p = self.fade_progress();
            if p > 0.7 {
                "stalled"
            } else {
                "slow..."
            }
        } else {
            "streaming"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn not_stalled_initially() {
        let d = StalledDetector::default();
        assert!(!d.is_stalled());
        assert!((d.fade_progress() - 0.0).abs() < 0.01);
    }

    #[test]
    fn stalled_after_threshold() {
        let d = StalledDetector::new(Duration::from_millis(50), Duration::from_secs(10));
        thread::sleep(Duration::from_millis(80));
        assert!(d.is_stalled());
        assert!(d.fade_progress() > 0.0);
    }

    #[test]
    fn recording_resets_stall() {
        let mut d = StalledDetector::new(Duration::from_millis(50), Duration::from_secs(10));
        thread::sleep(Duration::from_millis(80));
        assert!(d.is_stalled());
        d.record_tokens(1);
        assert!(!d.is_stalled());
    }

    #[test]
    fn fade_clamps_at_one() {
        let d = StalledDetector::new(Duration::from_millis(1), Duration::from_millis(1));
        thread::sleep(Duration::from_millis(50));
        assert!(d.fade_progress() <= 1.0);
    }

    #[test]
    fn rate_tracking() {
        let mut d = StalledDetector::default();
        thread::sleep(Duration::from_millis(20));
        d.record_tokens(100);
        assert!(d.rate() > 0.0);
        assert_eq!(d.total(), 100);
    }
}
