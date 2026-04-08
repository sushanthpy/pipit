//! Feedback & status components (23–36).
//!
//! Wraps `throbber-widgets-tui`, ratatui `Gauge`/`LineGauge`/`Sparkline`,
//! and custom status displays for real-time agent activity feedback.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Gauge, Paragraph, Wrap};

// ═══════════════════════════════════════════════════════════════════════
// 23. AnimatedSpinner — rich animated spinner (10+ styles)
// ═══════════════════════════════════════════════════════════════════════

/// Spinner animation style.
#[derive(Debug, Clone, Copy)]
pub enum SpinnerStyle {
    Braille,
    Dots,
    Line,
    Arrow,
    Bounce,
    Clock,
}

impl SpinnerStyle {
    fn frames(&self) -> &'static [&'static str] {
        match self {
            Self::Braille => &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
            Self::Dots => &["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"],
            Self::Line => &["|", "/", "-", "\\"],
            Self::Arrow => &["←", "↖", "↑", "↗", "→", "↘", "↓", "↙"],
            Self::Bounce => &["⠁", "⠂", "⠄", "⡀", "⢀", "⠠", "⠐", "⠈"],
            Self::Clock => &[
                "🕐", "🕑", "🕒", "🕓", "🕔", "🕕", "🕖", "🕗", "🕘", "🕙", "🕚", "🕛",
            ],
        }
    }
}

pub struct AnimatedSpinner<'a> {
    pub label: &'a str,
    pub frame: u64,
    pub style: SpinnerStyle,
    pub color: Color,
    pub elapsed_secs: Option<u64>,
}

impl<'a> AnimatedSpinner<'a> {
    pub fn new(label: &'a str, frame: u64) -> Self {
        Self {
            label,
            frame,
            style: SpinnerStyle::Braille,
            color: Color::Cyan,
            elapsed_secs: None,
        }
    }

    pub fn spinner_style(mut self, style: SpinnerStyle) -> Self {
        self.style = style;
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    pub fn elapsed(mut self, secs: u64) -> Self {
        self.elapsed_secs = Some(secs);
        self
    }
}

impl Widget for &AnimatedSpinner<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let frames = self.style.frames();
        let idx = (self.frame / 4) as usize % frames.len();

        let mut spans = vec![
            Span::styled(
                format!(" {} ", frames[idx]),
                Style::default().fg(self.color),
            ),
            Span::styled(self.label.to_string(), Style::default().fg(Color::DarkGray)),
        ];

        if let Some(secs) = self.elapsed_secs {
            spans.push(Span::styled(
                format!(" {}s", secs),
                Style::default().fg(Color::DarkGray),
            ));
        }

        Paragraph::new(Line::from(spans)).render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 24. ProgressBar — deterministic progress with percentage
// ═══════════════════════════════════════════════════════════════════════

pub struct ProgressBar<'a> {
    pub ratio: f64, // 0.0 to 1.0
    pub label: &'a str,
    pub color: Color,
}

impl<'a> ProgressBar<'a> {
    pub fn new(ratio: f64) -> Self {
        Self {
            ratio: ratio.clamp(0.0, 1.0),
            label: "",
            color: Color::Cyan,
        }
    }

    pub fn label(mut self, label: &'a str) -> Self {
        self.label = label;
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }
}

impl Widget for &ProgressBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let label = if self.label.is_empty() {
            format!("{:.0}%", self.ratio * 100.0)
        } else {
            format!("{} ({:.0}%)", self.label, self.ratio * 100.0)
        };

        Gauge::default()
            .gauge_style(Style::default().fg(self.color))
            .ratio(self.ratio)
            .label(label)
            .render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 26. TokenCounter — live token usage with color coding
// ═══════════════════════════════════════════════════════════════════════

pub struct TokenCounter {
    pub used: u64,
    pub limit: u64,
    pub cache_hits: u64,
}

impl TokenCounter {
    pub fn new(used: u64, limit: u64) -> Self {
        Self {
            used,
            limit,
            cache_hits: 0,
        }
    }

    pub fn cache_hits(mut self, hits: u64) -> Self {
        self.cache_hits = hits;
        self
    }
}

impl Widget for &TokenCounter {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let ratio = if self.limit > 0 {
            self.used as f64 / self.limit as f64
        } else {
            0.0
        };

        let color = if ratio > 0.9 {
            Color::Red
        } else if ratio > 0.7 {
            Color::Yellow
        } else {
            Color::Green
        };

        let mut spans = vec![
            Span::styled("◆ ", Style::default().fg(color)),
            Span::styled(
                format_tokens(self.used),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("/{}", format_tokens(self.limit)),
                Style::default().fg(Color::DarkGray),
            ),
        ];

        if self.cache_hits > 0 {
            spans.push(Span::styled(
                format!(" ({}↺)", format_tokens(self.cache_hits)),
                Style::default().fg(Color::Blue),
            ));
        }

        Paragraph::new(Line::from(spans)).render(area, buf);
    }
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 27. CostDisplay — running cost in USD
// ═══════════════════════════════════════════════════════════════════════

pub struct CostDisplay {
    pub total_cost: f64,
    pub session_cost: f64,
}

impl CostDisplay {
    pub fn new(total_cost: f64) -> Self {
        Self {
            total_cost,
            session_cost: total_cost,
        }
    }

    pub fn session_cost(mut self, cost: f64) -> Self {
        self.session_cost = cost;
        self
    }
}

impl Widget for &CostDisplay {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let color = if self.total_cost > 1.0 {
            Color::Red
        } else if self.total_cost > 0.1 {
            Color::Yellow
        } else {
            Color::Green
        };

        Paragraph::new(Line::from(vec![
            Span::styled("$", Style::default().fg(color)),
            Span::styled(
                format!("{:.4}", self.total_cost),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
        ]))
        .render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 28. StatusBar — bottom bar: model, mode, branch, tokens, cost
// ═══════════════════════════════════════════════════════════════════════

pub struct StatusBar<'a> {
    pub model: &'a str,
    pub mode: &'a str,
    pub branch: Option<&'a str>,
    pub tokens_used: u64,
    pub tokens_limit: u64,
    pub cost: f64,
    pub turn: u32,
    pub max_turns: u32,
}

impl<'a> StatusBar<'a> {
    pub fn new(model: &'a str, mode: &'a str) -> Self {
        Self {
            model,
            mode,
            branch: None,
            tokens_used: 0,
            tokens_limit: 0,
            cost: 0.0,
            turn: 0,
            max_turns: 0,
        }
    }
}

impl Widget for &StatusBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let no_color = crate::app::no_color();

        let mut spans = if no_color {
            vec![
                Span::styled(
                    format!(" pipit v{} ", env!("CARGO_PKG_VERSION")),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(" - "),
                Span::raw(self.model.to_string()),
                Span::raw(" "),
                Span::styled(format!("[{}]", self.mode), Style::default()),
            ]
        } else {
            vec![
                Span::styled(
                    format!(" pipit v{} ", env!("CARGO_PKG_VERSION")),
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled("─", Style::default().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::styled(self.model.to_string(), Style::default().fg(Color::White)),
                Span::raw(" "),
                Span::styled(self.mode.to_string(), Style::default().fg(Color::Yellow)),
            ]
        };

        if let Some(branch) = self.branch {
            spans.push(Span::raw(" "));
            if no_color {
                spans.push(Span::raw(format!("[{}]", branch)));
            } else {
                spans.push(Span::styled(
                    format!("⎇ {}", branch),
                    Style::default().fg(Color::Magenta),
                ));
            }
        }

        // Right-aligned info
        let right = format!(
            " {}% ${:.4} ",
            if self.tokens_limit > 0 {
                (self.tokens_used as f64 / self.tokens_limit as f64 * 100.0) as u64
            } else {
                0
            },
            self.cost,
        );

        // Calculate padding (saturating to prevent overflow on narrow terminals)
        let left_width: usize = spans.iter().map(|s| s.content.len()).sum();
        let total_used = left_width.saturating_add(right.len());
        let padding = (area.width as usize).saturating_sub(total_used);
        if padding > 0 {
            spans.push(Span::raw(" ".repeat(padding)));
        }
        spans.push(Span::styled(
            right,
            Style::default().fg(if no_color {
                Color::Reset
            } else {
                Color::DarkGray
            }),
        ));

        let bg = if no_color {
            Style::default()
        } else {
            Style::default().bg(Color::DarkGray)
        };
        Paragraph::new(Line::from(spans))
            .style(bg)
            .render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 29. SkeletonLoader — shimmer/pulse loading placeholder
// ═══════════════════════════════════════════════════════════════════════

pub struct SkeletonLoader {
    pub lines: u16,
    pub frame: u64,
}

impl SkeletonLoader {
    pub fn new(lines: u16, frame: u64) -> Self {
        Self { lines, frame }
    }
}

impl Widget for &SkeletonLoader {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let shimmer_pos = (self.frame / 2) as u16 % area.width;
        let display_lines = self.lines.min(area.height);

        for row in 0..display_lines {
            let line_width = match row % 3 {
                0 => area.width * 3 / 4,
                1 => area.width / 2,
                _ => area.width * 2 / 3,
            };

            for col in 0..line_width {
                let x = area.x + col;
                let y = area.y + row;
                if x < area.x + area.width && y < area.y + area.height {
                    let dist = (col as i16 - shimmer_pos as i16).unsigned_abs();
                    let color = if dist < 3 {
                        Color::Gray
                    } else {
                        Color::DarkGray
                    };
                    buf[(x, y)].set_style(Style::default().fg(color));
                    buf[(x, y)].set_char('░');
                }
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 30. StreamingIndicator — animated dots showing streaming is active
// ═══════════════════════════════════════════════════════════════════════

pub struct StreamingIndicator {
    pub frame: u64,
    pub color: Color,
}

impl StreamingIndicator {
    pub fn new(frame: u64) -> Self {
        Self {
            frame,
            color: Color::Cyan,
        }
    }
}

impl Widget for &StreamingIndicator {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let n = ((self.frame / 6) % 4) as usize;
        let dots = "●".repeat(n + 1);
        let empty = "○".repeat(3 - n.min(3));

        Paragraph::new(Line::from(vec![
            Span::styled(dots, Style::default().fg(self.color)),
            Span::styled(empty, Style::default().fg(Color::DarkGray)),
        ]))
        .render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 31. ToolRunning — "Running bash: npm test..." with elapsed timer
// ═══════════════════════════════════════════════════════════════════════

pub struct ToolRunning<'a> {
    pub tool_name: &'a str,
    pub args_summary: &'a str,
    pub elapsed_secs: u64,
    pub frame: u64,
}

impl<'a> ToolRunning<'a> {
    pub fn new(tool_name: &'a str, args_summary: &'a str, elapsed: u64, frame: u64) -> Self {
        Self {
            tool_name,
            args_summary,
            elapsed_secs: elapsed,
            frame,
        }
    }
}

impl Widget for &ToolRunning<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let spinner = AnimatedSpinner::new("", self.frame)
            .spinner_style(SpinnerStyle::Braille)
            .color(Color::Yellow);

        let frames = SpinnerStyle::Braille.frames();
        let idx = (self.frame / 4) as usize % frames.len();

        let line = Line::from(vec![
            Span::styled(
                format!(" {} ", frames[idx]),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!("Running {}", self.tool_name),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(": {}", self.args_summary),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("  {}s", self.elapsed_secs),
                Style::default().fg(Color::DarkGray),
            ),
        ]);

        Paragraph::new(vec![line]).render(area, buf);
        let _ = spinner; // used for frame data only
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 33. PermissionPrompt — "Allow bash: rm -rf? [y/N]"
// ═══════════════════════════════════════════════════════════════════════

pub struct PermissionPrompt<'a> {
    pub tool_name: &'a str,
    pub command: &'a str,
    pub risk_level: &'a str,
    pub selected_yes: bool,
}

impl<'a> PermissionPrompt<'a> {
    pub fn new(tool_name: &'a str, command: &'a str) -> Self {
        Self {
            tool_name,
            command,
            risk_level: "medium",
            selected_yes: false,
        }
    }

    pub fn risk(mut self, level: &'a str) -> Self {
        self.risk_level = level;
        self
    }
}

impl Widget for &PermissionPrompt<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let risk_color = match self.risk_level {
            "high" | "critical" => Color::Red,
            "medium" => Color::Yellow,
            _ => Color::Green,
        };

        let lines = vec![
            Line::from(vec![
                Span::styled(
                    format!("⚠ Allow {} ", self.tool_name),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("[{}]", self.risk_level),
                    Style::default().fg(risk_color),
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                self.command.to_string(),
                Style::default().fg(Color::White),
            )),
            Line::from(""),
            Line::from(vec![
                if self.selected_yes {
                    Span::styled(
                        " ▸ Allow ",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::REVERSED),
                    )
                } else {
                    Span::styled("   Allow ", Style::default().fg(Color::DarkGray))
                },
                Span::raw("  "),
                if !self.selected_yes {
                    Span::styled(
                        " ▸ Deny ",
                        Style::default()
                            .fg(Color::Red)
                            .add_modifier(Modifier::REVERSED),
                    )
                } else {
                    Span::styled("   Deny ", Style::default().fg(Color::DarkGray))
                },
            ]),
        ];

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(risk_color))
            .title(Span::styled(
                " permission ",
                Style::default().fg(risk_color),
            ));

        Paragraph::new(lines).block(block).render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 34. ModeIndicator — current mode badge
// ═══════════════════════════════════════════════════════════════════════

pub struct ModeIndicator<'a> {
    pub mode: &'a str,
}

impl<'a> ModeIndicator<'a> {
    pub fn new(mode: &'a str) -> Self {
        Self { mode }
    }
}

impl Widget for &ModeIndicator<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (color, bg) = match self.mode.to_lowercase().as_str() {
            "fast" => (Color::Black, Color::Green),
            "balanced" => (Color::Black, Color::Cyan),
            "guarded" => (Color::Black, Color::Yellow),
            "full_auto" | "yolo" => (Color::White, Color::Red),
            _ => (Color::White, Color::DarkGray),
        };

        Paragraph::new(Span::styled(
            format!(" {} ", self.mode.to_uppercase()),
            Style::default()
                .fg(color)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ))
        .render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 35. VerificationBadge — PASS / FAIL / RUNNING badge
// ═══════════════════════════════════════════════════════════════════════

pub struct VerificationBadge<'a> {
    pub status: &'a str,
    pub confidence: Option<f64>,
}

impl<'a> VerificationBadge<'a> {
    pub fn new(status: &'a str) -> Self {
        Self {
            status,
            confidence: None,
        }
    }

    pub fn confidence(mut self, c: f64) -> Self {
        self.confidence = Some(c);
        self
    }
}

impl Widget for &VerificationBadge<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (icon, color) = match self.status.to_uppercase().as_str() {
            "PASS" => ("✓", Color::Green),
            "FAIL" => ("✗", Color::Red),
            "RUNNING" => ("◌", Color::Yellow),
            "REPAIRABLE" => ("↻", Color::Yellow),
            _ => ("?", Color::DarkGray),
        };

        let mut spans = vec![Span::styled(
            format!(" {} {} ", icon, self.status.to_uppercase()),
            Style::default()
                .fg(Color::Black)
                .bg(color)
                .add_modifier(Modifier::BOLD),
        )];

        if let Some(c) = self.confidence {
            spans.push(Span::styled(
                format!(" {:.0}%", c),
                Style::default().fg(color),
            ));
        }

        Paragraph::new(Line::from(spans)).render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 36. BranchIndicator — git branch name with dirty indicator
// ═══════════════════════════════════════════════════════════════════════

pub struct BranchIndicator<'a> {
    pub branch: &'a str,
    pub dirty: bool,
}

impl<'a> BranchIndicator<'a> {
    pub fn new(branch: &'a str) -> Self {
        Self {
            branch,
            dirty: false,
        }
    }

    pub fn dirty(mut self, dirty: bool) -> Self {
        self.dirty = dirty;
        self
    }
}

impl Widget for &BranchIndicator<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut spans = vec![
            Span::styled("⎇ ", Style::default().fg(Color::Magenta)),
            Span::styled(self.branch.to_string(), Style::default().fg(Color::Magenta)),
        ];

        if self.dirty {
            spans.push(Span::styled("*", Style::default().fg(Color::Yellow)));
        }

        Paragraph::new(Line::from(spans)).render(area, buf);
    }
}
