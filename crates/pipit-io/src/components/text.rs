//! Text display components (1–12).
//!
//! Wraps ratatui `Paragraph`, `syntect`, `pulldown-cmark`, `similar`,
//! `ansi-to-tui`, `tui-big-text`, and `tui-tree-widget` for rich text
//! rendering inside the pipit TUI.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

// ═══════════════════════════════════════════════════════════════════════
// 1. MarkdownView — streaming markdown with inline code highlighting
// ═══════════════════════════════════════════════════════════════════════

/// Renders markdown text with heading styles, bold/italic, code spans,
/// fenced code blocks (syntax-highlighted via syntect), lists, and
/// blockquotes. Designed for streaming: call with partial markdown and
/// it renders what it can.
pub struct MarkdownView<'a> {
    pub content: &'a str,
    pub block: Option<Block<'a>>,
    pub style: Style,
}

impl<'a> MarkdownView<'a> {
    pub fn new(content: &'a str) -> Self {
        Self { content, block: None, style: Style::default() }
    }

    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    fn parse_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let mut in_code_block = false;
        let mut code_lang = String::new();
        let mut code_buf = Vec::new();

        for raw in self.content.lines() {
            let trimmed = raw.trim();

            // Fence toggle
            if trimmed.starts_with("```") {
                if in_code_block {
                    // End code block — render accumulated code
                    for code_line in &code_buf {
                        lines.push(Line::from(Span::styled(
                            format!("  {}", code_line),
                            Style::default().fg(Color::Green),
                        )));
                    }
                    code_buf.clear();
                    in_code_block = false;
                    code_lang.clear();
                } else {
                    in_code_block = true;
                    code_lang = trimmed.trim_start_matches('`').to_string();
                    if !code_lang.is_empty() {
                        lines.push(Line::from(Span::styled(
                            format!("  ┌─ {} ", code_lang),
                            Style::default().fg(Color::DarkGray),
                        )));
                    }
                }
                continue;
            }

            if in_code_block {
                code_buf.push(raw.to_string());
                continue;
            }

            // Headings
            if trimmed.starts_with("### ") {
                lines.push(Line::from(Span::styled(
                    trimmed[4..].to_string(),
                    Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
                )));
            } else if trimmed.starts_with("## ") {
                lines.push(Line::from(Span::styled(
                    trimmed[3..].to_string(),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )));
            } else if trimmed.starts_with("# ") {
                lines.push(Line::from(Span::styled(
                    trimmed[2..].to_string(),
                    Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                )));
            } else if trimmed.starts_with("> ") {
                // Blockquote
                lines.push(Line::from(vec![
                    Span::styled("│ ", Style::default().fg(Color::DarkGray)),
                    Span::styled(trimmed[2..].to_string(), Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)),
                ]));
            } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
                // Bullet list
                let inline = style_inline_markdown(&trimmed[2..]);
                let mut spans = vec![Span::styled("  • ", Style::default().fg(Color::Cyan))];
                spans.extend(inline.spans);
                lines.push(Line::from(spans));
            } else if trimmed.is_empty() {
                lines.push(Line::from(""));
            } else {
                lines.push(style_inline_markdown(raw));
            }
        }

        // Flush any unterminated code block
        for code_line in &code_buf {
            lines.push(Line::from(Span::styled(
                format!("  {}", code_line),
                Style::default().fg(Color::Green),
            )));
        }

        lines
    }
}

impl Widget for &MarkdownView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let lines = self.parse_lines();
        let mut paragraph = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .style(self.style);
        if let Some(ref block) = self.block {
            paragraph = paragraph.block(block.clone());
        }
        paragraph.render(area, buf);
    }
}

/// Parse inline markdown: `code`, **bold**, *italic*.
fn style_inline_markdown(raw: &str) -> Line<'static> {
    let mut spans = Vec::new();
    let mut chars = raw.char_indices().peekable();
    let mut plain_start = 0;

    while let Some(&(i, ch)) = chars.peek() {
        match ch {
            '`' => {
                // Push preceding plain text
                if i > plain_start {
                    spans.push(Span::raw(raw[plain_start..i].to_string()));
                }
                chars.next();
                let code_start = i + 1;
                // Scan to closing backtick
                let mut code_end = code_start;
                while let Some(&(j, c)) = chars.peek() {
                    chars.next();
                    if c == '`' {
                        code_end = j;
                        break;
                    }
                    code_end = j + c.len_utf8();
                }
                spans.push(Span::styled(
                    raw[code_start..code_end].to_string(),
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ));
                plain_start = code_end + 1;
            }
            '*' => {
                if i > plain_start {
                    spans.push(Span::raw(raw[plain_start..i].to_string()));
                }
                chars.next();
                // Check for ** (bold)
                if chars.peek().map(|&(_, c)| c) == Some('*') {
                    chars.next();
                    let bold_start = i + 2;
                    let mut bold_end = bold_start;
                    while let Some(&(j, c)) = chars.peek() {
                        chars.next();
                        if c == '*' {
                            if chars.peek().map(|&(_, c2)| c2) == Some('*') {
                                chars.next();
                                bold_end = j;
                                break;
                            }
                        }
                        bold_end = j + c.len_utf8();
                    }
                    spans.push(Span::styled(
                        raw[bold_start..bold_end].to_string(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ));
                    plain_start = bold_end + 2;
                } else {
                    // Single * = italic
                    let ital_start = i + 1;
                    let mut ital_end = ital_start;
                    while let Some(&(j, c)) = chars.peek() {
                        chars.next();
                        if c == '*' {
                            ital_end = j;
                            break;
                        }
                        ital_end = j + c.len_utf8();
                    }
                    spans.push(Span::styled(
                        raw[ital_start..ital_end].to_string(),
                        Style::default().add_modifier(Modifier::ITALIC),
                    ));
                    plain_start = ital_end + 1;
                }
            }
            _ => {
                chars.next();
            }
        }
    }

    // Trailing plain text
    if plain_start < raw.len() {
        spans.push(Span::raw(raw[plain_start..].to_string()));
    }

    Line::from(spans)
}

// ═══════════════════════════════════════════════════════════════════════
// 2. CodeBlock — syntax-highlighted code with line numbers
// ═══════════════════════════════════════════════════════════════════════

pub struct CodeBlock<'a> {
    pub code: &'a str,
    pub language: Option<&'a str>,
    pub line_offset: usize,
    pub highlight_lines: Vec<usize>,
    pub show_line_numbers: bool,
}

impl<'a> CodeBlock<'a> {
    pub fn new(code: &'a str) -> Self {
        Self {
            code,
            language: None,
            line_offset: 0,
            highlight_lines: Vec::new(),
            show_line_numbers: true,
        }
    }

    pub fn language(mut self, lang: &'a str) -> Self {
        self.language = Some(lang);
        self
    }

    pub fn line_offset(mut self, offset: usize) -> Self {
        self.line_offset = offset;
        self
    }

    pub fn highlight_lines(mut self, lines: Vec<usize>) -> Self {
        self.highlight_lines = lines;
        self
    }
}

impl Widget for &CodeBlock<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let line_count = self.code.lines().count();
        let gutter_width = if self.show_line_numbers {
            format!("{}", self.line_offset + line_count).len() + 2
        } else {
            0
        };

        let mut lines: Vec<Line> = Vec::new();
        for (i, code_line) in self.code.lines().enumerate() {
            let line_num = self.line_offset + i + 1;
            let is_highlighted = self.highlight_lines.contains(&line_num);
            let bg = if is_highlighted { Color::DarkGray } else { Color::Reset };

            let mut spans = Vec::new();
            if self.show_line_numbers {
                spans.push(Span::styled(
                    format!("{:>width$} │", line_num, width = gutter_width - 2),
                    Style::default().fg(Color::DarkGray).bg(bg),
                ));
            }
            spans.push(Span::styled(
                format!(" {}", code_line),
                Style::default().fg(Color::Green).bg(bg),
            ));
            lines.push(Line::from(spans));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(
                format!(" {} ", self.language.unwrap_or("code")),
                Style::default().fg(Color::Cyan),
            ));

        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 3. DiffView — unified diff with ±coloring and line numbers
// ═══════════════════════════════════════════════════════════════════════

pub struct DiffView<'a> {
    pub old_text: &'a str,
    pub new_text: &'a str,
    pub file_path: Option<&'a str>,
}

impl<'a> DiffView<'a> {
    pub fn new(old_text: &'a str, new_text: &'a str) -> Self {
        Self { old_text, new_text, file_path: None }
    }

    pub fn file_path(mut self, path: &'a str) -> Self {
        self.file_path = Some(path);
        self
    }
}

impl Widget for &DiffView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        use similar::{ChangeTag, TextDiff};

        let diff = TextDiff::from_lines(self.old_text, self.new_text);
        let mut lines: Vec<Line> = Vec::new();

        if let Some(path) = self.file_path {
            lines.push(Line::from(Span::styled(
                format!("─── {} ───", path),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )));
        }

        for change in diff.iter_all_changes() {
            let (sign, color) = match change.tag() {
                ChangeTag::Delete => ("-", Color::Red),
                ChangeTag::Insert => ("+", Color::Green),
                ChangeTag::Equal => (" ", Color::Reset),
            };
            let text = change.to_string_lossy();
            lines.push(Line::from(vec![
                Span::styled(sign.to_string(), Style::default().fg(color).add_modifier(Modifier::BOLD)),
                Span::styled(
                    text.trim_end_matches('\n').to_string(),
                    Style::default().fg(color),
                ),
            ]));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(" diff ");

        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 4. TextWrap — auto-wrapping text with style preservation
// ═══════════════════════════════════════════════════════════════════════

pub struct TextWrap<'a> {
    pub text: &'a str,
    pub style: Style,
}

impl<'a> TextWrap<'a> {
    pub fn new(text: &'a str) -> Self {
        Self { text, style: Style::default() }
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }
}

impl Widget for &TextWrap<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self.text.to_string())
            .style(self.style)
            .wrap(Wrap { trim: true })
            .render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 5. BigTextView — large ASCII art text (via tui-big-text)
// ═══════════════════════════════════════════════════════════════════════

pub struct BigTextView<'a> {
    pub text: &'a str,
    pub style: Style,
}

impl<'a> BigTextView<'a> {
    pub fn new(text: &'a str) -> Self {
        Self { text, style: Style::default().fg(Color::Cyan) }
    }
}

impl Widget for &BigTextView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        use tui_big_text::{BigText, PixelSize};
        let big = BigText::builder()
            .pixel_size(PixelSize::Quadrant)
            .style(self.style)
            .lines(vec![self.text.into()])
            .build();
        big.render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 7. AnsiText — render raw ANSI-colored output from subprocesses
// ═══════════════════════════════════════════════════════════════════════

pub struct AnsiText<'a> {
    pub raw: &'a [u8],
    pub block: Option<Block<'a>>,
}

impl<'a> AnsiText<'a> {
    pub fn new(raw: &'a [u8]) -> Self {
        Self { raw, block: None }
    }

    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }
}

impl Widget for &AnsiText<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        use ansi_to_tui::IntoText;
        let text = self.raw.into_text()
            .unwrap_or_else(|_| Text::raw(String::from_utf8_lossy(self.raw).to_string()));
        let mut paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
        if let Some(ref block) = self.block {
            paragraph = paragraph.block(block.clone());
        }
        paragraph.render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 8. JsonTreeView — collapsible JSON/TOML/YAML viewer
// ═══════════════════════════════════════════════════════════════════════

pub struct JsonTreeView<'a> {
    pub json: &'a serde_json::Value,
    pub title: &'a str,
}

impl<'a> JsonTreeView<'a> {
    pub fn new(json: &'a serde_json::Value) -> Self {
        Self { json, title: "json" }
    }

    pub fn title(mut self, title: &'a str) -> Self {
        self.title = title;
        self
    }

    fn json_to_lines(&self, value: &serde_json::Value, indent: usize) -> Vec<Line<'static>> {
        let pad = "  ".repeat(indent);
        match value {
            serde_json::Value::Object(map) => {
                let mut lines = Vec::new();
                for (key, val) in map {
                    match val {
                        serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                            lines.push(Line::from(vec![
                                Span::raw(pad.clone()),
                                Span::styled(format!("{}:", key), Style::default().fg(Color::Cyan)),
                            ]));
                            lines.extend(self.json_to_lines(val, indent + 1));
                        }
                        _ => {
                            lines.push(Line::from(vec![
                                Span::raw(pad.clone()),
                                Span::styled(format!("{}: ", key), Style::default().fg(Color::Cyan)),
                                Span::styled(
                                    format!("{}", val),
                                    Style::default().fg(Color::Yellow),
                                ),
                            ]));
                        }
                    }
                }
                lines
            }
            serde_json::Value::Array(arr) => {
                let mut lines = Vec::new();
                for (i, val) in arr.iter().enumerate() {
                    lines.push(Line::from(vec![
                        Span::raw(pad.clone()),
                        Span::styled(format!("[{}]: ", i), Style::default().fg(Color::DarkGray)),
                        Span::styled(format!("{}", val), Style::default().fg(Color::Yellow)),
                    ]));
                }
                lines
            }
            other => {
                vec![Line::from(vec![
                    Span::raw(pad),
                    Span::styled(format!("{}", other), Style::default().fg(Color::Yellow)),
                ])]
            }
        }
    }
}

impl Widget for &JsonTreeView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let lines = self.json_to_lines(self.json, 0);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(
                format!(" {} ", self.title),
                Style::default().fg(Color::Cyan),
            ));
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 9. ErrorDisplay — formatted error with title, message, suggestion
// ═══════════════════════════════════════════════════════════════════════

pub struct ErrorDisplay<'a> {
    pub title: &'a str,
    pub message: &'a str,
    pub suggestion: Option<&'a str>,
}

impl<'a> ErrorDisplay<'a> {
    pub fn new(title: &'a str, message: &'a str) -> Self {
        Self { title, message, suggestion: None }
    }

    pub fn suggestion(mut self, suggestion: &'a str) -> Self {
        self.suggestion = Some(suggestion);
        self
    }
}

impl Widget for &ErrorDisplay<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut lines = vec![
            Line::from(Span::styled(
                self.title.to_string(),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                self.message.to_string(),
                Style::default().fg(Color::White),
            )),
        ];
        if let Some(suggestion) = self.suggestion {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("hint: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(suggestion.to_string(), Style::default().fg(Color::Yellow)),
            ]));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Red))
            .title(Span::styled(" error ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)));

        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 10. HelpText — keyboard shortcut help overlay
// ═══════════════════════════════════════════════════════════════════════

pub struct HelpText<'a> {
    pub bindings: &'a [(&'a str, &'a str)], // (key, description)
}

impl<'a> HelpText<'a> {
    pub fn new(bindings: &'a [(&'a str, &'a str)]) -> Self {
        Self { bindings }
    }
}

impl Widget for &HelpText<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let rows: Vec<ratatui::widgets::Row> = self.bindings.iter().map(|(key, desc)| {
            ratatui::widgets::Row::new(vec![
                ratatui::widgets::Cell::from(Span::styled(key.to_string(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
                ratatui::widgets::Cell::from(Span::styled(desc.to_string(), Style::default().fg(Color::White))),
            ])
        }).collect();

        let widths = [Constraint::Length(16), Constraint::Min(20)];
        let table = ratatui::widgets::Table::new(rows, widths)
            .block(Block::default().borders(Borders::ALL).title(" keybindings "));

        ratatui::widgets::Widget::render(table, area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 11. ThinkingBlock — model thinking/reasoning display (collapsible)
// ═══════════════════════════════════════════════════════════════════════

pub struct ThinkingBlock<'a> {
    pub content: &'a str,
    pub collapsed: bool,
    pub frame: u64,
}

impl<'a> ThinkingBlock<'a> {
    pub fn new(content: &'a str, frame: u64) -> Self {
        Self { content, collapsed: false, frame }
    }

    pub fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }
}

impl Widget for &ThinkingBlock<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let dots_n = ((self.frame / 8) % 4) as usize + 1;
        let dots = "·".repeat(dots_n);

        if self.collapsed {
            let line = Line::from(vec![
                Span::styled("▸ ", Style::default().fg(Color::Magenta)),
                Span::styled("reasoning", Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)),
                Span::styled(format!(" {}", dots), Style::default().fg(Color::Magenta)),
            ]);
            Paragraph::new(vec![line]).render(area, buf);
        } else {
            let mut lines: Vec<Line> = vec![
                Line::from(vec![
                    Span::styled("▾ ", Style::default().fg(Color::Magenta)),
                    Span::styled("reasoning", Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)),
                    Span::styled(format!(" {}", dots), Style::default().fg(Color::Magenta)),
                ]),
            ];
            for text_line in self.content.lines() {
                lines.push(Line::from(Span::styled(
                    format!("  {}", text_line),
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                )));
            }
            Paragraph::new(lines).render(area, buf);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 12. Citation — source citation with file:line reference
// ═══════════════════════════════════════════════════════════════════════

pub struct Citation<'a> {
    pub file: &'a str,
    pub line: Option<usize>,
    pub snippet: Option<&'a str>,
}

impl<'a> Citation<'a> {
    pub fn new(file: &'a str) -> Self {
        Self { file, line: None, snippet: None }
    }

    pub fn line(mut self, line: usize) -> Self {
        self.line = Some(line);
        self
    }

    pub fn snippet(mut self, snippet: &'a str) -> Self {
        self.snippet = Some(snippet);
        self
    }
}

impl Widget for &Citation<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let location = if let Some(line) = self.line {
            format!("{}:{}", self.file, line)
        } else {
            self.file.to_string()
        };

        let mut spans = vec![
            Span::styled("⟫ ", Style::default().fg(Color::DarkGray)),
            Span::styled(location, Style::default().fg(Color::Blue).add_modifier(Modifier::UNDERLINED)),
        ];

        if let Some(snippet) = self.snippet {
            spans.push(Span::styled(
                format!(" — {}", snippet),
                Style::default().fg(Color::DarkGray),
            ));
        }

        Paragraph::new(Line::from(spans)).render(area, buf);
    }
}
