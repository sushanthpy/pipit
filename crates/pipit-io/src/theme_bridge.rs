//! Theme bridge — convenience style functions inspired by clawdesk-tui's `theme.rs`.
//!
//! Wraps pipit's `SemanticTheme` into simple `style_*()` functions that
//! rendering code can call instead of hardcoding `Color::Rgb(...)`.
//! This enables consistent theming and future palette switching.

use crate::theme::SemanticTheme;
use ratatui::style::{Color, Modifier, Style};
use std::sync::OnceLock;

// ── Global theme instance ────────────────────────────────────────────────────

static THEME: OnceLock<SemanticTheme> = OnceLock::new();

/// Initialize the global theme. Call once at startup.
pub fn init(theme: SemanticTheme) {
    let _ = THEME.set(theme);
}

/// Get the current theme (falls back to dark if not initialized).
pub fn current() -> &'static SemanticTheme {
    THEME.get_or_init(SemanticTheme::dark)
}

// ── Core style factories (mirrors clawdesk pattern) ──────────────────────────

pub fn style_default() -> Style {
    let t = current();
    Style::default().fg(t.fg).bg(t.bg)
}

pub fn style_header() -> Style {
    let t = current();
    Style::default()
        .fg(t.fg)
        .bg(Color::Rgb(30, 30, 40))
        .add_modifier(Modifier::BOLD)
}

pub fn style_success() -> Style {
    Style::default().fg(current().success)
}

pub fn style_error() -> Style {
    Style::default().fg(current().error)
}

pub fn style_warning() -> Style {
    Style::default().fg(current().warning)
}

pub fn style_info() -> Style {
    Style::default().fg(current().accent)
}

pub fn style_muted() -> Style {
    Style::default().fg(current().muted)
}

pub fn style_border() -> Style {
    Style::default().fg(current().border)
}

pub fn style_border_active() -> Style {
    Style::default().fg(current().border_active)
}

pub fn style_highlight() -> Style {
    let t = current();
    Style::default().bg(Color::Rgb(49, 50, 68)).fg(t.fg)
}

pub fn style_table_header() -> Style {
    Style::default()
        .fg(current().accent)
        .add_modifier(Modifier::BOLD)
}

// ── Semantic style factories ─────────────────────────────────────────────────

pub fn style_accent() -> Style {
    Style::default().fg(current().accent)
}

pub fn style_accent_bold() -> Style {
    Style::default()
        .fg(current().accent)
        .add_modifier(Modifier::BOLD)
}

pub fn style_accent2() -> Style {
    Style::default().fg(current().accent2)
}

pub fn style_spinner() -> Style {
    Style::default()
        .fg(current().spinner_glyph)
        .add_modifier(Modifier::BOLD)
}

pub fn style_phase_plan() -> Style {
    Style::default().fg(current().phase_plan)
}

pub fn style_phase_execute() -> Style {
    Style::default().fg(current().phase_execute)
}

pub fn style_phase_verify() -> Style {
    Style::default().fg(current().phase_verify)
}

pub fn style_phase_repair() -> Style {
    Style::default().fg(current().phase_repair)
}

// ── Token / cost budget styles ───────────────────────────────────────────────

pub fn style_token_bar(pct: u64) -> Style {
    let t = current();
    let color = match pct {
        0..=50 => t.token_ok,
        51..=80 => t.token_warn,
        _ => t.token_danger,
    };
    Style::default().fg(color)
}

pub fn style_cost(cost: f64) -> Style {
    let t = current();
    let color = if cost < 0.10 {
        t.cost_ok
    } else if cost < 1.0 {
        t.cost_warn
    } else {
        t.cost_danger
    };
    Style::default().fg(color)
}

// ── Diff styles ──────────────────────────────────────────────────────────────

pub fn style_diff_added() -> Style {
    Style::default().fg(current().diff_added)
}

pub fn style_diff_removed() -> Style {
    Style::default().fg(current().diff_removed)
}

pub fn style_diff_context() -> Style {
    Style::default().fg(current().diff_context)
}

// ── Convenience color accessors ──────────────────────────────────────────────

pub fn color_accent() -> Color {
    current().accent
}

pub fn color_success() -> Color {
    current().success
}

pub fn color_error() -> Color {
    current().error
}

pub fn color_warning() -> Color {
    current().warning
}

pub fn color_muted() -> Color {
    current().muted
}

pub fn color_border() -> Color {
    current().border
}

pub fn color_bg() -> Color {
    current().bg
}

pub fn color_fg() -> Color {
    current().fg
}
