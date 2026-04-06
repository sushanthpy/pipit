//! Theming & effects components (81–86).
//!
//! Theme management, transitions, and visual effects using
//! `tachyonfx` and manual buffer manipulation for animations.

use ratatui::prelude::*;

// ═══════════════════════════════════════════════════════════════════════
// 81. Theme — light/dark/solarized/custom theme switching
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeVariant {
    Dark,
    Light,
    Solarized,
    Monokai,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub variant: ThemeVariant,
    pub bg: Color,
    pub fg: Color,
    pub accent: Color,
    pub accent2: Color,
    pub muted: Color,
    pub border: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            variant: ThemeVariant::Dark,
            bg: Color::Reset,
            fg: Color::White,
            accent: Color::Cyan,
            accent2: Color::Magenta,
            muted: Color::DarkGray,
            border: Color::DarkGray,
            success: Color::Green,
            warning: Color::Yellow,
            error: Color::Red,
        }
    }

    pub fn light() -> Self {
        Self {
            variant: ThemeVariant::Light,
            bg: Color::White,
            fg: Color::Black,
            accent: Color::Blue,
            accent2: Color::Magenta,
            muted: Color::Gray,
            border: Color::Gray,
            success: Color::Green,
            warning: Color::Yellow,
            error: Color::Red,
        }
    }

    pub fn solarized() -> Self {
        Self {
            variant: ThemeVariant::Solarized,
            bg: Color::Rgb(0, 43, 54),
            fg: Color::Rgb(131, 148, 150),
            accent: Color::Rgb(38, 139, 210),
            accent2: Color::Rgb(211, 54, 130),
            muted: Color::Rgb(88, 110, 117),
            border: Color::Rgb(88, 110, 117),
            success: Color::Rgb(133, 153, 0),
            warning: Color::Rgb(181, 137, 0),
            error: Color::Rgb(220, 50, 47),
        }
    }

    pub fn monokai() -> Self {
        Self {
            variant: ThemeVariant::Monokai,
            bg: Color::Rgb(39, 40, 34),
            fg: Color::Rgb(248, 248, 242),
            accent: Color::Rgb(102, 217, 239),
            accent2: Color::Rgb(174, 129, 255),
            muted: Color::Rgb(117, 113, 94),
            border: Color::Rgb(117, 113, 94),
            success: Color::Rgb(166, 226, 46),
            warning: Color::Rgb(253, 151, 31),
            error: Color::Rgb(249, 38, 114),
        }
    }

    /// Get the base style for the entire TUI.
    pub fn base_style(&self) -> Style {
        Style::default().fg(self.fg).bg(self.bg)
    }

    /// Get the style for borders.
    pub fn border_style(&self) -> Style {
        Style::default().fg(self.border)
    }

    /// Get the style for accented/highlighted text.
    pub fn accent_style(&self) -> Style {
        Style::default().fg(self.accent)
    }

    /// Get the style for muted/dimmed text.
    pub fn muted_style(&self) -> Style {
        Style::default().fg(self.muted)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 82. FadeTransition — fade-in/out effect for panel switches
// ═══════════════════════════════════════════════════════════════════════

pub struct FadeTransition {
    pub progress: f32, // 0.0 = fully transparent, 1.0 = fully opaque
    pub fade_in: bool,
}

impl FadeTransition {
    pub fn fade_in(progress: f32) -> Self {
        Self { progress: progress.clamp(0.0, 1.0), fade_in: true }
    }

    pub fn fade_out(progress: f32) -> Self {
        Self { progress: progress.clamp(0.0, 1.0), fade_in: false }
    }

    /// Apply the fade effect to a buffer area by dimming cells.
    pub fn apply(&self, area: Rect, buf: &mut Buffer) {
        let alpha = if self.fade_in { self.progress } else { 1.0 - self.progress };

        if alpha < 0.3 {
            // Very faded — dim everything
            for y in area.y..area.y + area.height {
                for x in area.x..area.x + area.width {
                    if x < buf.area().width && y < buf.area().height {
                        buf[(x, y)].set_style(
                            Style::default().add_modifier(Modifier::DIM)
                        );
                    }
                }
            }
        } else if alpha < 0.7 {
            // Partially faded — just dim
            for y in area.y..area.y + area.height {
                for x in area.x..area.x + area.width {
                    if x < buf.area().width && y < buf.area().height {
                        let existing = buf[(x, y)].style();
                        buf[(x, y)].set_style(
                            existing.add_modifier(Modifier::DIM)
                        );
                    }
                }
            }
        }
        // alpha >= 0.7: fully visible, no modification needed
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 83. SlideTransition — slide-in animation for popups
// ═══════════════════════════════════════════════════════════════════════

pub struct SlideTransition {
    pub progress: f32, // 0.0 = off-screen, 1.0 = final position
    pub direction: SlideDirection,
}

#[derive(Clone, Copy)]
pub enum SlideDirection {
    Left,
    Right,
    Up,
    Down,
}

impl SlideTransition {
    pub fn new(progress: f32, direction: SlideDirection) -> Self {
        Self { progress: progress.clamp(0.0, 1.0), direction }
    }

    /// Calculate the animated rect offset from the target area.
    pub fn offset_area(&self, target: Rect) -> Rect {
        let remaining = 1.0 - self.progress;
        match self.direction {
            SlideDirection::Left => Rect::new(
                target.x.saturating_sub((target.width as f32 * remaining) as u16),
                target.y,
                target.width,
                target.height,
            ),
            SlideDirection::Right => Rect::new(
                target.x + (target.width as f32 * remaining) as u16,
                target.y,
                target.width,
                target.height,
            ),
            SlideDirection::Up => Rect::new(
                target.x,
                target.y.saturating_sub((target.height as f32 * remaining) as u16),
                target.width,
                target.height,
            ),
            SlideDirection::Down => Rect::new(
                target.x,
                target.y + (target.height as f32 * remaining) as u16,
                target.width,
                target.height,
            ),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 84. PulseHighlight — pulse effect on modified file entries
// ═══════════════════════════════════════════════════════════════════════

pub struct PulseHighlight {
    pub frame: u64,
    pub color: Color,
    pub period: u64, // frames per pulse cycle
}

impl PulseHighlight {
    pub fn new(frame: u64, color: Color) -> Self {
        Self { frame, color, period: 30 }
    }

    /// Returns whether the highlight is currently "on" (visible).
    pub fn is_active(&self) -> bool {
        (self.frame % self.period) < (self.period / 2)
    }

    /// Get the style for this frame.
    pub fn style(&self) -> Style {
        if self.is_active() {
            Style::default().fg(self.color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.color)
        }
    }

    /// Apply pulse to a buffer area.
    pub fn apply(&self, area: Rect, buf: &mut Buffer) {
        if self.is_active() {
            for y in area.y..area.y + area.height {
                for x in area.x..area.x + area.width {
                    if x < buf.area().width && y < buf.area().height {
                        let existing = buf[(x, y)].style();
                        buf[(x, y)].set_style(existing.add_modifier(Modifier::BOLD));
                    }
                }
            }
        }
    }
}
