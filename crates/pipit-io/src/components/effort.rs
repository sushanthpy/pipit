//! T7: Effort indicator & thinking-intensity display.
//!
//! Shows the current PEV phase with distinct icons and a thinking
//! intensity bar that reflects reasoning effort (low → max).

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::spinner_verbs::AgentPhase;
use crate::theme::SemanticTheme;

/// Thinking effort level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EffortLevel {
    Low,
    Medium,
    High,
    Max,
}

impl EffortLevel {
    /// Number of filled bars (out of 4).
    pub fn bars(&self) -> usize {
        match self {
            Self::Low => 1,
            Self::Medium => 2,
            Self::High => 3,
            Self::Max => 4,
        }
    }

    /// Pick effort from a 0.0..=1.0 intensity value.
    pub fn from_intensity(t: f32) -> Self {
        if t < 0.25 {
            Self::Low
        } else if t < 0.5 {
            Self::Medium
        } else if t < 0.75 {
            Self::High
        } else {
            Self::Max
        }
    }

    /// Map to theme color.
    pub fn color(&self, theme: &SemanticTheme) -> Color {
        match self {
            Self::Low => theme.effort_low,
            Self::Medium => theme.effort_medium,
            Self::High => theme.effort_high,
            Self::Max => theme.effort_max,
        }
    }
}

/// Phase icon + color from the semantic theme.
pub fn phase_icon(phase: AgentPhase) -> &'static str {
    match phase {
        AgentPhase::Plan => "◇",    // diamond outline
        AgentPhase::Execute => "▶",  // play
        AgentPhase::Verify => "✓",   // check
        AgentPhase::Repair => "⚡",  // lightning
        AgentPhase::Idle => "○",     // empty circle
    }
}

/// Phase color from the semantic theme.
pub fn phase_color(phase: AgentPhase, theme: &SemanticTheme) -> Color {
    match phase {
        AgentPhase::Plan => theme.phase_plan,
        AgentPhase::Execute => theme.phase_execute,
        AgentPhase::Verify => theme.phase_verify,
        AgentPhase::Repair => theme.phase_repair,
        AgentPhase::Idle => theme.muted,
    }
}

/// Effort indicator widget: phase icon + intensity bar.
pub struct EffortIndicator<'a> {
    pub phase: AgentPhase,
    pub effort: EffortLevel,
    pub theme: &'a SemanticTheme,
    /// Optional label (e.g. "Thinking…" from spinner verbs).
    pub label: Option<&'a str>,
}

impl<'a> EffortIndicator<'a> {
    pub fn new(phase: AgentPhase, effort: EffortLevel, theme: &'a SemanticTheme) -> Self {
        Self {
            phase,
            effort,
            theme,
            label: None,
        }
    }

    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }
}

impl Widget for &EffortIndicator<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 4 || area.height == 0 {
            return;
        }

        let icon = phase_icon(self.phase);
        let icon_color = phase_color(self.phase, self.theme);
        let bar_color = self.effort.color(self.theme);

        // Build intensity bar: ████░░░░ (filled + empty)
        let filled = self.effort.bars();
        let empty = 4 - filled;
        let bar: String = "█".repeat(filled) + &"░".repeat(empty);

        let mut spans = vec![
            Span::styled(
                format!("{icon} "),
                Style::default().fg(icon_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(bar, Style::default().fg(bar_color)),
        ];

        if let Some(lbl) = self.label {
            spans.push(Span::styled(
                format!(" {lbl}"),
                Style::default().fg(self.theme.spinner_label),
            ));
        }

        Paragraph::new(Line::from(spans)).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::SemanticTheme;

    #[test]
    fn effort_from_intensity() {
        assert_eq!(EffortLevel::from_intensity(0.0), EffortLevel::Low);
        assert_eq!(EffortLevel::from_intensity(0.3), EffortLevel::Medium);
        assert_eq!(EffortLevel::from_intensity(0.6), EffortLevel::High);
        assert_eq!(EffortLevel::from_intensity(0.9), EffortLevel::Max);
    }

    #[test]
    fn bars_count() {
        assert_eq!(EffortLevel::Low.bars(), 1);
        assert_eq!(EffortLevel::Max.bars(), 4);
    }

    #[test]
    fn all_phases_have_icons() {
        for phase in &[AgentPhase::Plan, AgentPhase::Execute, AgentPhase::Verify, AgentPhase::Repair, AgentPhase::Idle] {
            assert!(!phase_icon(*phase).is_empty());
        }
    }

    #[test]
    fn effort_indicator_renders() {
        let theme = SemanticTheme::dark();
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        let widget = EffortIndicator::new(AgentPhase::Execute, EffortLevel::High, &theme)
            .label("Coding…");
        (&widget).render(area, &mut buf);
        let content: String = buf.content().iter().map(|c| c.symbol().to_string()).collect();
        assert!(content.contains('▶'));
    }
}
