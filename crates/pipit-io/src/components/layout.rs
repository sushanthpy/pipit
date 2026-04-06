//! Layout & structure components (37–48).
//!
//! Wraps ratatui `Layout`, `Tabs`, `tui-scrollview`, `tui-popup`,
//! and custom structural containers for composing the pipit TUI.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Tabs, Clear, Wrap};

// ═══════════════════════════════════════════════════════════════════════
// 37. SplitPane — resizable horizontal split
// ═══════════════════════════════════════════════════════════════════════

pub struct SplitPane {
    pub ratio: u16, // left pane percentage (0-100)
    pub direction: Direction,
}

impl SplitPane {
    pub fn horizontal(ratio: u16) -> Self {
        Self { ratio: ratio.min(100), direction: Direction::Horizontal }
    }

    pub fn vertical(ratio: u16) -> Self {
        Self { ratio: ratio.min(100), direction: Direction::Vertical }
    }

    /// Returns the two areas for left/right (or top/bottom) panes.
    pub fn areas(&self, area: Rect) -> (Rect, Rect) {
        let chunks = Layout::default()
            .direction(self.direction)
            .constraints([
                Constraint::Percentage(self.ratio),
                Constraint::Percentage(100 - self.ratio),
            ])
            .split(area);
        (chunks[0], chunks[1])
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 39. TabBarView — tab navigation with active highlight
// ═══════════════════════════════════════════════════════════════════════

pub struct TabBarView<'a> {
    pub titles: &'a [&'a str],
    pub selected: usize,
}

impl<'a> TabBarView<'a> {
    pub fn new(titles: &'a [&'a str], selected: usize) -> Self {
        Self { titles, selected }
    }
}

impl Widget for &TabBarView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let titles: Vec<Line> = self.titles.iter().map(|t| Line::from(*t)).collect();
        Tabs::new(titles)
            .select(self.selected)
            .style(Style::default().fg(Color::DarkGray))
            .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD | Modifier::UNDERLINED))
            .divider("│")
            .render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 40. ScrollContainer — scrollable area for content
// ═══════════════════════════════════════════════════════════════════════

pub struct ScrollContainer<'a> {
    pub content_lines: &'a [Line<'a>],
    pub scroll_offset: u16,
    pub block: Option<Block<'a>>,
}

impl<'a> ScrollContainer<'a> {
    pub fn new(content_lines: &'a [Line<'a>], scroll_offset: u16) -> Self {
        Self { content_lines, scroll_offset, block: None }
    }

    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }
}

impl Widget for &ScrollContainer<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let inner = if let Some(ref block) = self.block {
            let inner = block.inner(area);
            block.clone().render(area, buf);
            inner
        } else {
            area
        };

        let visible_height = inner.height as usize;
        let start = self.scroll_offset as usize;
        let end = (start + visible_height).min(self.content_lines.len());

        let visible: Vec<Line> = if start < self.content_lines.len() {
            self.content_lines[start..end].to_vec()
        } else {
            Vec::new()
        };

        Paragraph::new(visible).render(inner, buf);

        // Scrollbar indicator
        if self.content_lines.len() > visible_height {
            let total = self.content_lines.len() as f64;
            let bar_pos = (start as f64 / total * inner.height as f64) as u16;
            let bar_height = ((visible_height as f64 / total) * inner.height as f64).max(1.0) as u16;

            for y in bar_pos..(bar_pos + bar_height).min(inner.height) {
                let x = inner.x + inner.width - 1;
                let y_abs = inner.y + y;
                if x < buf.area().width && y_abs < buf.area().height {
                    buf[(x, y_abs)].set_char('▐');
                    buf[(x, y_abs)].set_style(Style::default().fg(Color::DarkGray));
                }
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 41. PopupOverlay — modal overlay with background dim
// ═══════════════════════════════════════════════════════════════════════

pub struct PopupOverlay<'a> {
    pub title: &'a str,
    pub width_percent: u16,
    pub height_percent: u16,
}

impl<'a> PopupOverlay<'a> {
    pub fn new(title: &'a str) -> Self {
        Self { title, width_percent: 60, height_percent: 40 }
    }

    pub fn size(mut self, width: u16, height: u16) -> Self {
        self.width_percent = width;
        self.height_percent = height;
        self
    }

    /// Calculate the centered popup area within the given area.
    pub fn area(&self, outer: Rect) -> Rect {
        let width = outer.width * self.width_percent / 100;
        let height = outer.height * self.height_percent / 100;
        let x = outer.x + (outer.width - width) / 2;
        let y = outer.y + (outer.height - height) / 2;
        Rect::new(x, y, width, height)
    }

    /// Render the background dim and return the inner area for content.
    pub fn render_frame(&self, area: Rect, buf: &mut Buffer) -> Rect {
        // Dim background
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if x < buf.area().width && y < buf.area().height {
                    buf[(x, y)].set_style(Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM));
                }
            }
        }

        let popup = self.area(area);
        Clear.render(popup, buf);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(
                format!(" {} ", self.title),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ));

        let inner = block.inner(popup);
        block.render(popup, buf);
        inner
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 42. CollapsibleSection — expandable/collapsible content
// ═══════════════════════════════════════════════════════════════════════

pub struct CollapsibleSection<'a> {
    pub title: &'a str,
    pub expanded: bool,
    pub content_lines: &'a [Line<'a>],
}

impl<'a> CollapsibleSection<'a> {
    pub fn new(title: &'a str, expanded: bool, content: &'a [Line<'a>]) -> Self {
        Self { title, expanded, content_lines: content }
    }
}

impl Widget for &CollapsibleSection<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let icon = if self.expanded { "▾" } else { "▸" };
        let header = Line::from(vec![
            Span::styled(format!("{} ", icon), Style::default().fg(Color::Cyan)),
            Span::styled(self.title.to_string(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        ]);

        if self.expanded && !self.content_lines.is_empty() {
            let mut lines = vec![header];
            lines.extend(self.content_lines.iter().cloned());
            Paragraph::new(lines).render(area, buf);
        } else {
            Paragraph::new(vec![header]).render(area, buf);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 43. Sidebar — collapsible sidebar
// ═══════════════════════════════════════════════════════════════════════

pub struct Sidebar<'a> {
    pub title: &'a str,
    pub items: &'a [(&'a str, &'a str)], // (icon, label)
    pub selected: Option<usize>,
    pub collapsed: bool,
}

impl<'a> Sidebar<'a> {
    pub fn new(title: &'a str, items: &'a [(&'a str, &'a str)]) -> Self {
        Self { title, items, selected: None, collapsed: false }
    }

    pub fn selected(mut self, idx: usize) -> Self {
        self.selected = Some(idx);
        self
    }

    pub fn collapsed(mut self, c: bool) -> Self {
        self.collapsed = c;
        self
    }
}

impl Widget for &Sidebar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if self.collapsed {
            // Only show icons
            let lines: Vec<Line> = self.items.iter().map(|(icon, _)| {
                Line::from(Span::styled(
                    format!(" {} ", icon),
                    Style::default().fg(Color::DarkGray),
                ))
            }).collect();
            Paragraph::new(lines)
                .block(Block::default().borders(Borders::RIGHT))
                .render(area, buf);
        } else {
            let lines: Vec<Line> = self.items.iter().enumerate().map(|(i, (icon, label))| {
                let style = if Some(i) == self.selected {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                Line::from(vec![
                    Span::styled(format!(" {} ", icon), style),
                    Span::styled(label.to_string(), style),
                ])
            }).collect();

            let block = Block::default()
                .borders(Borders::RIGHT)
                .title(Span::styled(
                    format!(" {} ", self.title),
                    Style::default().fg(Color::Cyan),
                ));

            Paragraph::new(lines).block(block).render(area, buf);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 44. Breadcrumb — path/to/current/context display
// ═══════════════════════════════════════════════════════════════════════

pub struct Breadcrumb<'a> {
    pub segments: &'a [&'a str],
}

impl<'a> Breadcrumb<'a> {
    pub fn new(segments: &'a [&'a str]) -> Self {
        Self { segments }
    }
}

impl Widget for &Breadcrumb<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut spans = Vec::new();
        for (i, seg) in self.segments.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" › ", Style::default().fg(Color::DarkGray)));
            }
            let style = if i == self.segments.len() - 1 {
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            spans.push(Span::styled(seg.to_string(), style));
        }
        Paragraph::new(Line::from(spans)).render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 45. Panel — titled content panel
// ═══════════════════════════════════════════════════════════════════════

pub struct Panel<'a> {
    pub title: &'a str,
    pub border_color: Color,
}

impl<'a> Panel<'a> {
    pub fn new(title: &'a str) -> Self {
        Self { title, border_color: Color::DarkGray }
    }

    pub fn border_color(mut self, color: Color) -> Self {
        self.border_color = color;
        self
    }

    /// Returns the Block for this panel and its inner rect.
    pub fn block(&self) -> Block<'_> {
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.border_color))
            .title(Span::styled(
                format!(" {} ", self.title),
                Style::default().fg(self.border_color),
            ))
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 46. FloatingWindow — floating panel
// ═══════════════════════════════════════════════════════════════════════

pub struct FloatingWindow<'a> {
    pub title: &'a str,
    pub content: &'a [Line<'a>],
    pub width: u16,
    pub height: u16,
}

impl<'a> FloatingWindow<'a> {
    pub fn new(title: &'a str, content: &'a [Line<'a>]) -> Self {
        Self { title, content, width: 60, height: 20 }
    }

    pub fn size(mut self, w: u16, h: u16) -> Self {
        self.width = w;
        self.height = h;
        self
    }
}

impl Widget for &FloatingWindow<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let w = self.width.min(area.width);
        let h = self.height.min(area.height);
        let x = area.x + (area.width - w) / 2;
        let y = area.y + (area.height - h) / 2;
        let popup = Rect::new(x, y, w, h);

        Clear.render(popup, buf);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(
                format!(" {} ", self.title),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ));

        Paragraph::new(self.content.to_vec())
            .block(block)
            .wrap(Wrap { trim: false })
            .render(popup, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 47. Divider — separator line
// ═══════════════════════════════════════════════════════════════════════

pub struct Divider<'a> {
    pub label: Option<&'a str>,
    pub style: Style,
}

impl<'a> Divider<'a> {
    pub fn horizontal() -> Self {
        Self { label: None, style: Style::default().fg(Color::DarkGray) }
    }

    pub fn with_label(label: &'a str) -> Self {
        Self { label: Some(label), style: Style::default().fg(Color::DarkGray) }
    }
}

impl Widget for &Divider<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if let Some(label) = self.label {
            let label_len = label.len() as u16 + 2;
            let left_width = (area.width.saturating_sub(label_len)) / 2;
            let right_width = area.width.saturating_sub(left_width + label_len);

            let line = Line::from(vec![
                Span::styled("─".repeat(left_width as usize), self.style),
                Span::styled(format!(" {} ", label), self.style.add_modifier(Modifier::BOLD)),
                Span::styled("─".repeat(right_width as usize), self.style),
            ]);
            Paragraph::new(vec![line]).render(area, buf);
        } else {
            let line = Line::from(Span::styled(
                "─".repeat(area.width as usize),
                self.style,
            ));
            Paragraph::new(vec![line]).render(area, buf);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 48. Grid — 2D grid layout helper
// ═══════════════════════════════════════════════════════════════════════

pub struct Grid {
    pub cols: u16,
    pub rows: u16,
}

impl Grid {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }

    /// Return a Vec of Rects for each cell in row-major order.
    pub fn cells(&self, area: Rect) -> Vec<Rect> {
        let col_width = area.width / self.cols.max(1);
        let row_height = area.height / self.rows.max(1);

        let mut cells = Vec::with_capacity((self.cols * self.rows) as usize);
        for r in 0..self.rows {
            for c in 0..self.cols {
                cells.push(Rect::new(
                    area.x + c * col_width,
                    area.y + r * row_height,
                    col_width,
                    row_height,
                ));
            }
        }
        cells
    }
}
