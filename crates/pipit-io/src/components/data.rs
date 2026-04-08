//! Data display components (49–62).
//!
//! Wraps ratatui `Table`, `List`, `Sparkline`, `Chart`, `BarChart`, and
//! ecosystem crates for structured data visualization.

use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Cell, List, ListItem, Paragraph, Row, Sparkline, Table, Wrap,
};

// ═══════════════════════════════════════════════════════════════════════
// 49. DataTable — sortable, scrollable table with column resize
// ═══════════════════════════════════════════════════════════════════════

pub struct DataTable<'a> {
    pub headers: &'a [&'a str],
    pub rows: &'a [Vec<String>],
    pub selected: Option<usize>,
    pub widths: Vec<Constraint>,
}

impl<'a> DataTable<'a> {
    pub fn new(headers: &'a [&'a str], rows: &'a [Vec<String>]) -> Self {
        let widths = headers.iter().map(|_| Constraint::Min(8)).collect();
        Self {
            headers,
            rows,
            selected: None,
            widths,
        }
    }

    pub fn selected(mut self, idx: usize) -> Self {
        self.selected = Some(idx);
        self
    }

    pub fn widths(mut self, widths: Vec<Constraint>) -> Self {
        self.widths = widths;
        self
    }
}

impl Widget for &DataTable<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let header_cells: Vec<Cell> = self
            .headers
            .iter()
            .map(|h| {
                Cell::from(Span::styled(
                    h.to_string(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))
            })
            .collect();

        let header =
            Row::new(header_cells).style(Style::default().add_modifier(Modifier::UNDERLINED));

        let data_rows: Vec<Row> = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let style = if Some(i) == self.selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::REVERSED)
                } else if i % 2 == 0 {
                    Style::default()
                } else {
                    Style::default().fg(Color::White)
                };
                let cells: Vec<Cell> = row.iter().map(|c| Cell::from(c.clone())).collect();
                Row::new(cells).style(style)
            })
            .collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        super::render_widget(
            Table::new(data_rows, &self.widths)
                .header(header)
                .block(block),
            area,
            buf,
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 50. FileTree — collapsible file system tree
// ═══════════════════════════════════════════════════════════════════════

pub struct FileTreeEntry {
    pub name: String,
    pub is_dir: bool,
    pub depth: u16,
    pub expanded: bool,
    pub icon: String,
}

pub struct FileTree<'a> {
    pub entries: &'a [FileTreeEntry],
    pub selected: Option<usize>,
}

impl<'a> FileTree<'a> {
    pub fn new(entries: &'a [FileTreeEntry]) -> Self {
        Self {
            entries,
            selected: None,
        }
    }

    pub fn selected(mut self, idx: usize) -> Self {
        self.selected = Some(idx);
        self
    }
}

impl Widget for &FileTree<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let items: Vec<ListItem> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let indent = "  ".repeat(entry.depth as usize);
                let arrow = if entry.is_dir {
                    if entry.expanded { "▾ " } else { "▸ " }
                } else {
                    "  "
                };

                let style = if Some(i) == self.selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::REVERSED)
                } else if entry.is_dir {
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                ListItem::new(Line::from(vec![
                    Span::raw(indent),
                    Span::styled(arrow.to_string(), Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{} ", entry.icon), style),
                    Span::styled(entry.name.clone(), style),
                ]))
            })
            .collect();

        let block = Block::default().borders(Borders::ALL).title(" files ");

        super::render_widget(List::new(items).block(block), area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 51. VirtualList — virtualized list (renders only visible rows)
// ═══════════════════════════════════════════════════════════════════════

pub struct VirtualList<'a> {
    pub items: &'a [String],
    pub total_count: usize,
    pub scroll_offset: usize,
    pub selected: Option<usize>,
    pub block: Option<Block<'a>>,
}

impl<'a> VirtualList<'a> {
    pub fn new(items: &'a [String], total_count: usize, scroll_offset: usize) -> Self {
        Self {
            items,
            total_count,
            scroll_offset,
            selected: None,
            block: None,
        }
    }

    pub fn selected(mut self, idx: usize) -> Self {
        self.selected = Some(idx);
        self
    }

    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }
}

impl Widget for &VirtualList<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let list_items: Vec<ListItem> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let abs_idx = self.scroll_offset + i;
                let style = if Some(abs_idx) == self.selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                ListItem::new(Span::styled(item.clone(), style))
            })
            .collect();

        let mut widget = List::new(list_items);
        if let Some(ref block) = self.block {
            widget = widget.block(block.clone());
        }
        super::render_widget(widget, area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 52. KeyValueTable — two-column key-value display
// ═══════════════════════════════════════════════════════════════════════

pub struct KeyValueTable<'a> {
    pub pairs: &'a [(&'a str, String)],
    pub title: Option<&'a str>,
}

impl<'a> KeyValueTable<'a> {
    pub fn new(pairs: &'a [(&'a str, String)]) -> Self {
        Self { pairs, title: None }
    }

    pub fn title(mut self, title: &'a str) -> Self {
        self.title = Some(title);
        self
    }
}

impl Widget for &KeyValueTable<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let widths = [Constraint::Length(20), Constraint::Min(10)];

        let rows: Vec<Row> = self
            .pairs
            .iter()
            .map(|(key, value)| {
                Row::new(vec![
                    Cell::from(Span::styled(
                        key.to_string(),
                        Style::default().fg(Color::Cyan),
                    )),
                    Cell::from(Span::styled(
                        value.clone(),
                        Style::default().fg(Color::White),
                    )),
                ])
            })
            .collect();

        let mut block = Block::default().borders(Borders::ALL);
        if let Some(title) = self.title {
            block = block.title(format!(" {} ", title));
        }

        super::render_widget(Table::new(rows, widths).block(block), area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 54. SparklineView — inline sparkline for token history
// ═══════════════════════════════════════════════════════════════════════

pub struct SparklineView<'a> {
    pub data: &'a [u64],
    pub title: Option<&'a str>,
    pub color: Color,
}

impl<'a> SparklineView<'a> {
    pub fn new(data: &'a [u64]) -> Self {
        Self {
            data,
            title: None,
            color: Color::Cyan,
        }
    }

    pub fn title(mut self, title: &'a str) -> Self {
        self.title = Some(title);
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }
}

impl Widget for &SparklineView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut spark = Sparkline::default()
            .data(self.data)
            .style(Style::default().fg(self.color));

        if let Some(title) = self.title {
            spark = spark.block(Block::default().title(format!(" {} ", title)));
        }

        spark.render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 57. DepGraph — dependency graph (simplified node list)
// ═══════════════════════════════════════════════════════════════════════

pub struct DepGraph<'a> {
    pub nodes: &'a [(&'a str, Vec<usize>)], // (name, edges_to_indices)
}

impl<'a> DepGraph<'a> {
    pub fn new(nodes: &'a [(&'a str, Vec<usize>)]) -> Self {
        Self { nodes }
    }
}

impl Widget for &DepGraph<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut lines: Vec<Line> = Vec::new();
        for (i, (name, deps)) in self.nodes.iter().enumerate() {
            let dep_names: Vec<&str> = deps
                .iter()
                .filter_map(|&idx| self.nodes.get(idx).map(|(n, _)| *n))
                .collect();

            let mut spans = vec![
                Span::styled(format!("{:>3}. ", i), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    name.to_string(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ];

            if !dep_names.is_empty() {
                spans.push(Span::styled(
                    format!(" → {}", dep_names.join(", ")),
                    Style::default().fg(Color::DarkGray),
                ));
            }

            lines.push(Line::from(spans));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" dependencies ");

        Paragraph::new(lines).block(block).render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 60. TimelineView — event timeline (tool calls, agent turns)
// ═══════════════════════════════════════════════════════════════════════

pub struct TimelineEntry {
    pub icon: String,
    pub color: Color,
    pub label: String,
    pub timestamp: Option<String>,
}

pub struct TimelineView<'a> {
    pub entries: &'a [TimelineEntry],
    pub title: Option<&'a str>,
}

impl<'a> TimelineView<'a> {
    pub fn new(entries: &'a [TimelineEntry]) -> Self {
        Self {
            entries,
            title: None,
        }
    }

    pub fn title(mut self, title: &'a str) -> Self {
        self.title = Some(title);
        self
    }
}

impl Widget for &TimelineView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let items: Vec<ListItem> = self
            .entries
            .iter()
            .map(|entry| {
                let mut spans = vec![
                    Span::styled(
                        format!(" {} ", entry.icon),
                        Style::default().fg(entry.color),
                    ),
                    Span::styled(entry.label.clone(), Style::default().fg(Color::White)),
                ];
                if let Some(ref ts) = entry.timestamp {
                    spans.push(Span::styled(
                        format!("  {}", ts),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        let mut block = Block::default().borders(Borders::ALL);
        if let Some(title) = self.title {
            block = block.title(format!(" {} ", title));
        }

        super::render_widget(List::new(items).block(block), area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 61. MetricCard — summary metric: label + large number
// ═══════════════════════════════════════════════════════════════════════

pub struct MetricCard<'a> {
    pub label: &'a str,
    pub value: &'a str,
    pub color: Color,
    pub trend: Option<&'a str>, // "↑", "↓", "→"
}

impl<'a> MetricCard<'a> {
    pub fn new(label: &'a str, value: &'a str) -> Self {
        Self {
            label,
            value,
            color: Color::White,
            trend: None,
        }
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    pub fn trend(mut self, trend: &'a str) -> Self {
        self.trend = Some(trend);
        self
    }
}

impl Widget for &MetricCard<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut value_spans = vec![Span::styled(
            self.value.to_string(),
            Style::default().fg(self.color).add_modifier(Modifier::BOLD),
        )];
        if let Some(trend) = self.trend {
            let trend_color = match trend {
                "↑" => Color::Green,
                "↓" => Color::Red,
                _ => Color::DarkGray,
            };
            value_spans.push(Span::styled(
                format!(" {}", trend),
                Style::default().fg(trend_color),
            ));
        }

        let lines = vec![
            Line::from(Span::styled(
                self.label.to_string(),
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(value_spans),
        ];

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Center)
            .render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 62. Badge — colored tag/badge
// ═══════════════════════════════════════════════════════════════════════

pub struct Badge<'a> {
    pub text: &'a str,
    pub fg: Color,
    pub bg: Color,
}

impl<'a> Badge<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            fg: Color::White,
            bg: Color::DarkGray,
        }
    }

    pub fn color(mut self, fg: Color, bg: Color) -> Self {
        self.fg = fg;
        self.bg = bg;
        self
    }

    pub fn success(text: &'a str) -> Self {
        Self {
            text,
            fg: Color::Black,
            bg: Color::Green,
        }
    }

    pub fn warning(text: &'a str) -> Self {
        Self {
            text,
            fg: Color::Black,
            bg: Color::Yellow,
        }
    }

    pub fn error(text: &'a str) -> Self {
        Self {
            text,
            fg: Color::White,
            bg: Color::Red,
        }
    }

    pub fn info(text: &'a str) -> Self {
        Self {
            text,
            fg: Color::Black,
            bg: Color::Cyan,
        }
    }
}

impl Widget for &Badge<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(Span::styled(
            format!(" {} ", self.text),
            Style::default()
                .fg(self.fg)
                .bg(self.bg)
                .add_modifier(Modifier::BOLD),
        ))
        .render(area, buf);
    }
}
