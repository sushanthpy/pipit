//! Terminal integration components (75–80).
//!
//! Wraps `tui-term` for embedded pseudoterminal, command history,
//! completion popups, and other terminal-specific widgets.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, List, ListItem, Table, Row, Cell, Clear, Wrap};

// ═══════════════════════════════════════════════════════════════════════
// 75. EmbeddedTerminal — pseudoterminal widget for live subprocess output
// ═══════════════════════════════════════════════════════════════════════

pub struct EmbeddedTerminal<'a> {
    pub output: &'a [u8],
    pub title: &'a str,
    pub scroll_offset: u16,
}

impl<'a> EmbeddedTerminal<'a> {
    pub fn new(output: &'a [u8]) -> Self {
        Self { output, title: "terminal", scroll_offset: 0 }
    }

    pub fn title(mut self, title: &'a str) -> Self {
        self.title = title;
        self
    }

    pub fn scroll(mut self, offset: u16) -> Self {
        self.scroll_offset = offset;
        self
    }
}

impl Widget for &EmbeddedTerminal<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        use ansi_to_tui::IntoText;
        let text = self.output.into_text()
            .unwrap_or_else(|_| Text::raw(String::from_utf8_lossy(self.output).to_string()));

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(
                format!(" {} ", self.title),
                Style::default().fg(Color::Green),
            ));

        Paragraph::new(text)
            .block(block)
            .scroll((self.scroll_offset, 0))
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 76. CommandHistory — scrollable command history with search
// ═══════════════════════════════════════════════════════════════════════

pub struct CommandHistoryEntry {
    pub command: String,
    pub exit_code: Option<i32>,
    pub timestamp: String,
}

pub struct CommandHistory<'a> {
    pub entries: &'a [CommandHistoryEntry],
    pub selected: Option<usize>,
    pub filter: Option<&'a str>,
}

impl<'a> CommandHistory<'a> {
    pub fn new(entries: &'a [CommandHistoryEntry]) -> Self {
        Self { entries, selected: None, filter: None }
    }

    pub fn selected(mut self, idx: usize) -> Self {
        self.selected = Some(idx);
        self
    }

    pub fn filter(mut self, query: &'a str) -> Self {
        self.filter = Some(query);
        self
    }
}

impl Widget for &CommandHistory<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let items: Vec<ListItem> = self.entries.iter().enumerate()
            .filter(|(_, entry)| {
                self.filter.map_or(true, |f| entry.command.contains(f))
            })
            .map(|(i, entry)| {
                let exit_style = match entry.exit_code {
                    Some(0) => Style::default().fg(Color::Green),
                    Some(_) => Style::default().fg(Color::Red),
                    None => Style::default().fg(Color::DarkGray),
                };
                let exit_text = entry.exit_code
                    .map(|c| format!("[{}]", c))
                    .unwrap_or_else(|| "[-]".to_string());

                let style = if Some(i) == self.selected {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };

                ListItem::new(Line::from(vec![
                    Span::styled(exit_text, exit_style),
                    Span::styled(format!(" {} ", entry.command), style),
                    Span::styled(entry.timestamp.clone(), Style::default().fg(Color::DarkGray)),
                ]))
            })
            .collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" history ");

        super::render_widget(List::new(items).block(block), area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 77. CompletionPopup — tab-completion dropdown
// ═══════════════════════════════════════════════════════════════════════

pub struct CompletionPopup<'a> {
    pub items: &'a [String],
    pub selected: usize,
    pub anchor_x: u16,
    pub anchor_y: u16,
}

impl<'a> CompletionPopup<'a> {
    pub fn new(items: &'a [String], selected: usize) -> Self {
        Self { items, selected, anchor_x: 0, anchor_y: 0 }
    }

    pub fn anchor(mut self, x: u16, y: u16) -> Self {
        self.anchor_x = x;
        self.anchor_y = y;
        self
    }
}

impl Widget for &CompletionPopup<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if self.items.is_empty() {
            return;
        }

        let max_width = self.items.iter().map(|s| s.len()).max().unwrap_or(10) as u16 + 4;
        let height = (self.items.len() as u16 + 2).min(10);

        let popup = Rect::new(
            self.anchor_x.min(area.x + area.width - max_width),
            self.anchor_y.saturating_sub(height),
            max_width.min(area.width),
            height.min(area.height),
        );

        Clear.render(popup, buf);

        let items: Vec<ListItem> = self.items.iter().enumerate().map(|(i, item)| {
            let style = if i == self.selected {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(Span::styled(format!(" {} ", item), style))
        }).collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        super::render_widget(List::new(items).block(block), popup, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 78. KeybindingOverlay — keybinding reference (? to toggle)
// ═══════════════════════════════════════════════════════════════════════

pub struct KeybindingOverlay<'a> {
    pub bindings: &'a [(&'a str, &'a str, &'a str)], // (key, action, context)
}

impl<'a> KeybindingOverlay<'a> {
    pub fn new(bindings: &'a [(&'a str, &'a str, &'a str)]) -> Self {
        Self { bindings }
    }
}

impl Widget for &KeybindingOverlay<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Center popup
        let width = 50u16.min(area.width - 4);
        let height = (self.bindings.len() as u16 + 4).min(area.height - 2);
        let x = area.x + (area.width - width) / 2;
        let y = area.y + (area.height - height) / 2;
        let popup = Rect::new(x, y, width, height);

        // Dim background
        for py in area.y..area.y + area.height {
            for px in area.x..area.x + area.width {
                if px < buf.area().width && py < buf.area().height {
                    buf[(px, py)].set_style(Style::default().add_modifier(Modifier::DIM));
                }
            }
        }

        Clear.render(popup, buf);

        let widths = [Constraint::Length(12), Constraint::Min(15), Constraint::Length(10)];
        let rows: Vec<Row> = self.bindings.iter().map(|(key, action, ctx)| {
            Row::new(vec![
                Cell::from(Span::styled(key.to_string(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
                Cell::from(Span::styled(action.to_string(), Style::default().fg(Color::White))),
                Cell::from(Span::styled(ctx.to_string(), Style::default().fg(Color::DarkGray))),
            ])
        }).collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(
                " Keybindings ",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ));

        super::render_widget(Table::new(rows, widths).block(block), popup, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 79. VoiceIndicator — mic active indicator with audio level
// ═══════════════════════════════════════════════════════════════════════

pub struct VoiceIndicator {
    pub active: bool,
    pub level: f32, // 0.0 to 1.0
    pub frame: u64,
}

impl VoiceIndicator {
    pub fn new(active: bool, level: f32, frame: u64) -> Self {
        Self { active, level: level.clamp(0.0, 1.0), frame }
    }
}

impl Widget for &VoiceIndicator {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if !self.active {
            Paragraph::new(Span::styled(
                " 🎤 off ",
                Style::default().fg(Color::DarkGray),
            )).render(area, buf);
            return;
        }

        let bars = (self.level * 5.0) as usize;
        let bar_chars: String = "█".repeat(bars) + &"░".repeat(5 - bars);

        let pulse = if self.frame % 8 < 4 { "●" } else { "○" };

        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {} ", pulse), Style::default().fg(Color::Red)),
            Span::styled("🎤 ", Style::default().fg(Color::White)),
            Span::styled(bar_chars, Style::default().fg(Color::Green)),
        ])).render(area, buf);
    }
}
