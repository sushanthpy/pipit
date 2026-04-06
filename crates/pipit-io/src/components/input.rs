//! Input control components (13–22).
//!
//! Wraps `tui-input`, `tui-textarea`, and custom prompt widgets for
//! interactive user input in the pipit TUI.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, List, ListItem, ListState, Clear};

// ═══════════════════════════════════════════════════════════════════════
// 13. CommandInput — multi-line input (wraps tui-textarea)
// ═══════════════════════════════════════════════════════════════════════

pub struct CommandInput<'a> {
    pub content: &'a str,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub placeholder: &'a str,
    pub block: Option<Block<'a>>,
}

impl<'a> CommandInput<'a> {
    pub fn new(content: &'a str) -> Self {
        Self {
            content,
            cursor_line: 0,
            cursor_col: 0,
            placeholder: "Type a message…",
            block: None,
        }
    }

    pub fn cursor(mut self, line: usize, col: usize) -> Self {
        self.cursor_line = line;
        self.cursor_col = col;
        self
    }

    pub fn placeholder(mut self, text: &'a str) -> Self {
        self.placeholder = text;
        self
    }

    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }
}

impl Widget for &CommandInput<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let display = if self.content.is_empty() {
            Paragraph::new(Span::styled(
                self.placeholder.to_string(),
                Style::default().fg(Color::DarkGray),
            ))
        } else {
            let mut lines: Vec<Line> = Vec::new();
            for (i, line) in self.content.lines().enumerate() {
                if i == self.cursor_line {
                    // Show cursor position
                    let before = &line[..self.cursor_col.min(line.len())];
                    let cursor_char = line.chars().nth(self.cursor_col).unwrap_or(' ');
                    let after_start = self.cursor_col + cursor_char.len_utf8();
                    let after = if after_start <= line.len() { &line[after_start..] } else { "" };

                    lines.push(Line::from(vec![
                        Span::raw(before.to_string()),
                        Span::styled(
                            cursor_char.to_string(),
                            Style::default().add_modifier(Modifier::REVERSED),
                        ),
                        Span::raw(after.to_string()),
                    ]));
                } else {
                    lines.push(Line::from(line.to_string()));
                }
            }
            if lines.is_empty() {
                lines.push(Line::from(Span::styled(
                    " ",
                    Style::default().add_modifier(Modifier::REVERSED),
                )));
            }
            Paragraph::new(lines)
        };

        let widget = if let Some(ref block) = self.block {
            display.block(block.clone())
        } else {
            display
        };

        widget.render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 14. SingleLineInput — single-line text input with cursor
// ═══════════════════════════════════════════════════════════════════════

pub struct SingleLineInput<'a> {
    pub value: &'a str,
    pub cursor: usize,
    pub label: &'a str,
    pub masked: bool,
}

impl<'a> SingleLineInput<'a> {
    pub fn new(value: &'a str, cursor: usize) -> Self {
        Self { value, cursor, label: "", masked: false }
    }

    pub fn label(mut self, label: &'a str) -> Self {
        self.label = label;
        self
    }

    pub fn masked(mut self) -> Self {
        self.masked = true;
        self
    }
}

impl Widget for &SingleLineInput<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let display_val: String = if self.masked {
            "•".repeat(self.value.len())
        } else {
            self.value.to_string()
        };

        let before = &display_val[..self.cursor.min(display_val.len())];
        let cursor_char = display_val.chars().nth(self.cursor).unwrap_or(' ');
        let after_pos = self.cursor + cursor_char.len_utf8();
        let after = if after_pos <= display_val.len() { &display_val[after_pos..] } else { "" };

        let mut spans = Vec::new();
        if !self.label.is_empty() {
            spans.push(Span::styled(
                format!("{}: ", self.label),
                Style::default().fg(Color::Cyan),
            ));
        }
        spans.push(Span::raw(before.to_string()));
        spans.push(Span::styled(cursor_char.to_string(), Style::default().add_modifier(Modifier::REVERSED)));
        spans.push(Span::raw(after.to_string()));

        Paragraph::new(Line::from(spans)).render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 15. ConfirmPrompt — yes/no confirmation dialog
// ═══════════════════════════════════════════════════════════════════════

pub struct ConfirmPrompt<'a> {
    pub message: &'a str,
    pub selected: bool, // true = Yes, false = No
    pub default_yes: bool,
}

impl<'a> ConfirmPrompt<'a> {
    pub fn new(message: &'a str) -> Self {
        Self { message, selected: false, default_yes: false }
    }

    pub fn default_yes(mut self) -> Self {
        self.default_yes = true;
        self.selected = true;
        self
    }
}

impl Widget for &ConfirmPrompt<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let yes_style = if self.selected {
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let no_style = if !self.selected {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let line = Line::from(vec![
            Span::styled(self.message.to_string(), Style::default().fg(Color::White)),
            Span::raw("  "),
            Span::styled(" Yes ", yes_style),
            Span::raw(" "),
            Span::styled(" No ", no_style),
        ]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));

        Paragraph::new(vec![Line::from(""), line])
            .block(block)
            .alignment(Alignment::Center)
            .render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 16. SelectPrompt — single-selection from a list
// ═══════════════════════════════════════════════════════════════════════

pub struct SelectPrompt<'a> {
    pub title: &'a str,
    pub options: &'a [&'a str],
    pub selected: usize,
}

impl<'a> SelectPrompt<'a> {
    pub fn new(title: &'a str, options: &'a [&'a str]) -> Self {
        Self { title, options, selected: 0 }
    }

    pub fn selected(mut self, idx: usize) -> Self {
        self.selected = idx;
        self
    }
}

impl Widget for &SelectPrompt<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let items: Vec<ListItem> = self.options.iter().enumerate().map(|(i, opt)| {
            let marker = if i == self.selected { "▸ " } else { "  " };
            let style = if i == self.selected {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Span::styled(format!("{}{}", marker, opt), style))
        }).collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(
                format!(" {} ", self.title),
                Style::default().fg(Color::Cyan),
            ));

        super::render_widget(List::new(items)
            .block(block)
            , area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 18. SearchInput — incremental search with result count
// ═══════════════════════════════════════════════════════════════════════

pub struct SearchInput<'a> {
    pub query: &'a str,
    pub cursor: usize,
    pub result_count: Option<usize>,
    pub current_match: Option<usize>,
}

impl<'a> SearchInput<'a> {
    pub fn new(query: &'a str, cursor: usize) -> Self {
        Self { query, cursor, result_count: None, current_match: None }
    }

    pub fn results(mut self, count: usize, current: usize) -> Self {
        self.result_count = Some(count);
        self.current_match = Some(current);
        self
    }
}

impl Widget for &SearchInput<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut spans = vec![
            Span::styled("🔍 ", Style::default().fg(Color::Yellow)),
        ];

        let before = &self.query[..self.cursor.min(self.query.len())];
        let after = &self.query[self.cursor.min(self.query.len())..];
        spans.push(Span::raw(before.to_string()));
        spans.push(Span::styled("▎", Style::default().add_modifier(Modifier::SLOW_BLINK)));
        spans.push(Span::raw(after.to_string()));

        if let (Some(count), Some(current)) = (self.result_count, self.current_match) {
            spans.push(Span::styled(
                format!("  {}/{}", current + 1, count),
                Style::default().fg(Color::DarkGray),
            ));
        }

        Paragraph::new(Line::from(spans)).render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 20. PasswordInput — masked text input for API keys
// ═══════════════════════════════════════════════════════════════════════

pub struct PasswordInput<'a> {
    pub value: &'a str,
    pub cursor: usize,
    pub label: &'a str,
}

impl<'a> PasswordInput<'a> {
    pub fn new(value: &'a str, cursor: usize) -> Self {
        Self { value, cursor, label: "Password" }
    }

    pub fn label(mut self, label: &'a str) -> Self {
        self.label = label;
        self
    }
}

impl Widget for &PasswordInput<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let input = SingleLineInput::new(self.value, self.cursor)
            .label(self.label)
            .masked();
        (&input).render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 21. PathInput — file path input with display
// ═══════════════════════════════════════════════════════════════════════

pub struct PathInput<'a> {
    pub value: &'a str,
    pub cursor: usize,
    pub completions: &'a [String],
    pub completion_selected: Option<usize>,
}

impl<'a> PathInput<'a> {
    pub fn new(value: &'a str, cursor: usize) -> Self {
        Self {
            value,
            cursor,
            completions: &[],
            completion_selected: None,
        }
    }

    pub fn completions(mut self, completions: &'a [String], selected: Option<usize>) -> Self {
        self.completions = completions;
        self.completion_selected = selected;
        self
    }
}

impl Widget for &PathInput<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let input = SingleLineInput::new(self.value, self.cursor).label("path");
        (&input).render(area, buf);

        // If there are completions and space below, render them
        if !self.completions.is_empty() && area.height > 1 {
            let popup_area = Rect {
                x: area.x,
                y: area.y + 1,
                width: area.width.min(40),
                height: (self.completions.len() as u16 + 2).min(area.height - 1),
            };

            Clear.render(popup_area, buf);

            let items: Vec<ListItem> = self.completions.iter().enumerate().map(|(i, c)| {
                let style = if Some(i) == self.completion_selected {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                ListItem::new(Span::styled(c.clone(), style))
            }).collect();

            super::render_widget(
                List::new(items)
                    .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray))),
                popup_area, buf,
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 22. ModelSelector — model selection display
// ═══════════════════════════════════════════════════════════════════════

pub struct ModelSelector<'a> {
    pub models: &'a [(&'a str, &'a str)], // (model_id, description)
    pub selected: usize,
}

impl<'a> ModelSelector<'a> {
    pub fn new(models: &'a [(&'a str, &'a str)]) -> Self {
        Self { models, selected: 0 }
    }

    pub fn selected(mut self, idx: usize) -> Self {
        self.selected = idx;
        self
    }
}

impl Widget for &ModelSelector<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let items: Vec<ListItem> = self.models.iter().enumerate().map(|(i, (id, desc))| {
            let marker = if i == self.selected { "▸ " } else { "  " };
            let style = if i == self.selected {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{}{}", marker, id), style),
                Span::styled(format!("  {}", desc), Style::default().fg(Color::DarkGray)),
            ]))
        }).collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Select Model ");

        super::render_widget(List::new(items).block(block), area, buf);
    }
}
