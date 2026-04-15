//! T9: Rate-limit bar & budget gauge widgets.
//!
//! Visual indicators for API rate limits, token budget, and cost.

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::theme::SemanticTheme;

// ═══════════════════════════════════════════════════════════════════════
// Rate-limit bar: shows remaining API capacity
// ═══════════════════════════════════════════════════════════════════════

/// Horizontal bar showing rate-limit remaining vs total.
pub struct RateLimitBar<'a> {
    pub remaining: u32,
    pub limit: u32,
    pub theme: &'a SemanticTheme,
    pub width: u16,
}

impl<'a> RateLimitBar<'a> {
    pub fn new(remaining: u32, limit: u32, theme: &'a SemanticTheme) -> Self {
        Self {
            remaining,
            limit,
            theme,
            width: 20,
        }
    }

    pub fn width(mut self, w: u16) -> Self {
        self.width = w;
        self
    }

    fn ratio(&self) -> f64 {
        if self.limit == 0 {
            return 1.0;
        }
        (self.remaining as f64 / self.limit as f64).clamp(0.0, 1.0)
    }

    fn fill_color(&self) -> Color {
        let r = self.ratio();
        if r > 0.5 {
            self.theme.rate_limit_fill
        } else if r > 0.2 {
            self.theme.cost_warn
        } else {
            self.theme.cost_danger
        }
    }
}

impl Widget for &RateLimitBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 3 || area.height == 0 {
            return;
        }

        let bar_width = self.width.min(area.width.saturating_sub(2)) as usize;
        let filled = (self.ratio() * bar_width as f64).round() as usize;
        let empty = bar_width.saturating_sub(filled);

        let fill_color = self.fill_color();
        let bar: String = "█".repeat(filled) + &"░".repeat(empty);

        let label = format!(" {}/{}", self.remaining, self.limit);

        let spans = vec![
            Span::styled("⟨", Style::default().fg(self.theme.muted)),
            Span::styled(
                &bar[..filled * 3], // UTF-8 █ is 3 bytes
                Style::default().fg(fill_color),
            ),
            Span::styled(
                &bar[filled * 3..],
                Style::default().fg(self.theme.rate_limit_empty),
            ),
            Span::styled("⟩", Style::default().fg(self.theme.muted)),
            Span::styled(label, Style::default().fg(self.theme.muted)),
        ];

        Paragraph::new(Line::from(spans)).render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Budget gauge: token usage + cost with color thresholds
// ═══════════════════════════════════════════════════════════════════════

/// Combined token + cost gauge.
pub struct BudgetGauge<'a> {
    pub tokens_used: u64,
    pub tokens_limit: u64,
    pub cost_usd: f64,
    pub cost_budget: f64,
    pub theme: &'a SemanticTheme,
}

impl<'a> BudgetGauge<'a> {
    pub fn new(theme: &'a SemanticTheme) -> Self {
        Self {
            tokens_used: 0,
            tokens_limit: 0,
            cost_usd: 0.0,
            cost_budget: 0.0,
            theme,
        }
    }

    pub fn tokens(mut self, used: u64, limit: u64) -> Self {
        self.tokens_used = used;
        self.tokens_limit = limit;
        self
    }

    pub fn cost(mut self, usd: f64, budget: f64) -> Self {
        self.cost_usd = usd;
        self.cost_budget = budget;
        self
    }

    fn token_color(&self) -> Color {
        if self.tokens_limit == 0 {
            return self.theme.token_ok;
        }
        let ratio = self.tokens_used as f64 / self.tokens_limit as f64;
        if ratio > 0.9 {
            self.theme.token_danger
        } else if ratio > 0.7 {
            self.theme.token_warn
        } else {
            self.theme.token_ok
        }
    }

    fn cost_color(&self) -> Color {
        if self.cost_budget <= 0.0 {
            return self.theme.cost_ok;
        }
        let ratio = self.cost_usd / self.cost_budget;
        if ratio > 0.9 {
            self.theme.cost_danger
        } else if ratio > 0.7 {
            self.theme.cost_warn
        } else {
            self.theme.cost_ok
        }
    }
}

impl Widget for &BudgetGauge<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 8 || area.height == 0 {
            return;
        }

        let tok_color = self.token_color();
        let cost_color = self.cost_color();

        let tok_str = format_compact(self.tokens_used);
        let tok_lim = format_compact(self.tokens_limit);

        let mut spans = vec![
            Span::styled("◆ ", Style::default().fg(tok_color)),
            Span::styled(
                tok_str,
                Style::default().fg(tok_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("/{tok_lim}"),
                Style::default().fg(self.theme.muted),
            ),
        ];

        if self.cost_budget > 0.0 || self.cost_usd > 0.0 {
            spans.push(Span::styled("  $", Style::default().fg(cost_color)));
            spans.push(Span::styled(
                format!("{:.4}", self.cost_usd),
                Style::default().fg(cost_color).add_modifier(Modifier::BOLD),
            ));
            if self.cost_budget > 0.0 {
                spans.push(Span::styled(
                    format!("/${:.2}", self.cost_budget),
                    Style::default().fg(self.theme.muted),
                ));
            }
        }

        Paragraph::new(Line::from(spans)).render(area, buf);
    }
}

fn format_compact(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_renders() {
        let theme = SemanticTheme::dark();
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        let widget = RateLimitBar::new(80, 100, &theme);
        (&widget).render(area, &mut buf);
        // Should contain some content
        let text: String = buf.content().iter().map(|c| c.symbol().to_string()).collect();
        assert!(text.contains("80"));
    }

    #[test]
    fn budget_gauge_renders() {
        let theme = SemanticTheme::dark();
        let area = Rect::new(0, 0, 50, 1);
        let mut buf = Buffer::empty(area);
        let widget = BudgetGauge::new(&theme)
            .tokens(50_000, 128_000)
            .cost(0.0234, 1.0);
        (&widget).render(area, &mut buf);
        let text: String = buf.content().iter().map(|c| c.symbol().to_string()).collect();
        assert!(text.contains("50.0K"));
    }

    #[test]
    fn format_compact_values() {
        assert_eq!(format_compact(500), "500");
        assert_eq!(format_compact(1_500), "1.5K");
        assert_eq!(format_compact(2_500_000), "2.5M");
    }
}
