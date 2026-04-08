//! Agent-specific components (63–74).
//!
//! Composed widgets for agent tool output, approval cards, session
//! summaries, and other AI-agent-specific UI elements.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, List, ListItem, Paragraph, Row, Table, Wrap};

use super::text::DiffView;

// ═══════════════════════════════════════════════════════════════════════
// 63. ToolCallDisplay — tool name, args, result with highlighting
// ═══════════════════════════════════════════════════════════════════════

pub struct ToolCallDisplay<'a> {
    pub tool_name: &'a str,
    pub args: &'a str,
    pub result: Option<&'a str>,
    pub success: bool,
    pub elapsed_ms: Option<u64>,
}

impl<'a> ToolCallDisplay<'a> {
    pub fn new(tool_name: &'a str, args: &'a str) -> Self {
        Self {
            tool_name,
            args,
            result: None,
            success: true,
            elapsed_ms: None,
        }
    }

    pub fn result(mut self, result: &'a str, success: bool) -> Self {
        self.result = Some(result);
        self.success = success;
        self
    }

    pub fn elapsed(mut self, ms: u64) -> Self {
        self.elapsed_ms = Some(ms);
        self
    }
}

impl Widget for &ToolCallDisplay<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let icon = if self.result.is_some() {
            if self.success { "✓" } else { "✗" }
        } else {
            "⋯"
        };
        let icon_color = if self.success {
            Color::Green
        } else {
            Color::Red
        };

        let mut lines = vec![Line::from(vec![
            Span::styled(format!("{} ", icon), Style::default().fg(icon_color)),
            Span::styled(
                self.tool_name.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            if let Some(ms) = self.elapsed_ms {
                Span::styled(format!("  {}ms", ms), Style::default().fg(Color::DarkGray))
            } else {
                Span::raw("")
            },
        ])];

        // Args (truncated)
        let args_display: String = self.args.chars().take(120).collect();
        lines.push(Line::from(Span::styled(
            format!("  {}", args_display),
            Style::default().fg(Color::DarkGray),
        )));

        // Result preview
        if let Some(result) = self.result {
            let preview: String = result.lines().take(3).collect::<Vec<_>>().join("\n  ");
            if !preview.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("  → {}", preview),
                    Style::default().fg(if self.success {
                        Color::Green
                    } else {
                        Color::Red
                    }),
                )));
            }
        }

        let border_color = if self.success {
            Color::DarkGray
        } else {
            Color::Red
        };
        let block = Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(border_color));

        Paragraph::new(lines).block(block).render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 64. AgentOutput — streaming LLM response with incremental rendering
// ═══════════════════════════════════════════════════════════════════════

pub struct AgentOutput<'a> {
    pub committed_lines: &'a [String],
    pub streaming_text: &'a str,
    pub is_thinking: bool,
    pub frame: u64,
}

impl<'a> AgentOutput<'a> {
    pub fn new(committed: &'a [String], streaming: &'a str) -> Self {
        Self {
            committed_lines: committed,
            streaming_text: streaming,
            is_thinking: false,
            frame: 0,
        }
    }

    pub fn thinking(mut self, thinking: bool, frame: u64) -> Self {
        self.is_thinking = thinking;
        self.frame = frame;
        self
    }
}

impl Widget for &AgentOutput<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut lines: Vec<Line> = Vec::new();

        // Committed lines
        for line in self.committed_lines {
            lines.push(Line::from(line.clone()));
        }

        // Streaming text
        if !self.streaming_text.is_empty() {
            for line in self.streaming_text.lines() {
                lines.push(Line::from(line.to_string()));
            }
        }

        // Thinking indicator
        if self.is_thinking {
            let dots_n = ((self.frame / 8) % 4) as usize + 1;
            let spinner_frames = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let idx = (self.frame / 4) as usize % spinner_frames.len();
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", spinner_frames[idx]),
                    Style::default().fg(Color::Magenta),
                ),
                Span::styled(
                    "reasoning",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                ),
                Span::styled(
                    format!(" {}", "·".repeat(dots_n)),
                    Style::default().fg(Color::Magenta),
                ),
            ]));
        }

        // Auto-scroll to bottom
        let visible_h = area.height as usize;
        let start = if lines.len() > visible_h {
            lines.len() - visible_h
        } else {
            0
        };
        let visible: Vec<Line> = lines[start..].to_vec();

        Paragraph::new(visible)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 65. ToolApprovalCard — tool approval with risk assessment
// ═══════════════════════════════════════════════════════════════════════

pub struct ToolApprovalCard<'a> {
    pub tool_name: &'a str,
    pub args_summary: &'a str,
    pub risk: &'a str,
    pub reason: &'a str,
    pub selected_allow: bool,
}

impl<'a> ToolApprovalCard<'a> {
    pub fn new(tool: &'a str, args: &'a str, risk: &'a str, reason: &'a str) -> Self {
        Self {
            tool_name: tool,
            args_summary: args,
            risk,
            reason,
            selected_allow: false,
        }
    }
}

impl Widget for &ToolApprovalCard<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let risk_color = match self.risk {
            "critical" | "high" => Color::Red,
            "medium" => Color::Yellow,
            _ => Color::Green,
        };

        let lines = vec![
            Line::from(vec![
                Span::styled("⚠ ", Style::default().fg(Color::Yellow)),
                Span::styled(
                    format!("Approve {}?", self.tool_name),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  [{}]", self.risk),
                    Style::default().fg(risk_color).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                self.args_summary.to_string(),
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                format!("reason: {}", self.reason),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )),
            Line::from(""),
            Line::from(vec![
                if self.selected_allow {
                    Span::styled(
                        " Allow (y) ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::styled(" Allow (y) ", Style::default().fg(Color::DarkGray))
                },
                Span::raw("  "),
                if !self.selected_allow {
                    Span::styled(
                        " Deny (n) ",
                        Style::default()
                            .fg(Color::White)
                            .bg(Color::Red)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::styled(" Deny (n) ", Style::default().fg(Color::DarkGray))
                },
            ]),
        ];

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(risk_color))
            .title(Span::styled(" approval ", Style::default().fg(risk_color)));

        Paragraph::new(lines).block(block).render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 66. FileEditPreview — before/after preview of file edits
// ═══════════════════════════════════════════════════════════════════════

pub struct FileEditPreview<'a> {
    pub file_path: &'a str,
    pub old_content: &'a str,
    pub new_content: &'a str,
}

impl<'a> FileEditPreview<'a> {
    pub fn new(file_path: &'a str, old: &'a str, new: &'a str) -> Self {
        Self {
            file_path: file_path,
            old_content: old,
            new_content: new,
        }
    }
}

impl Widget for &FileEditPreview<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let diff = DiffView::new(self.old_content, self.new_content).file_path(self.file_path);
        (&diff).render(area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 67. TaskListView — background tasks with status icons
// ═══════════════════════════════════════════════════════════════════════

pub struct TaskEntry {
    pub name: String,
    pub status: TaskStatus,
}

#[derive(Clone, Copy)]
pub enum TaskStatus {
    Pending,
    Running,
    Success,
    Failed,
}

pub struct TaskListView<'a> {
    pub tasks: &'a [TaskEntry],
}

impl<'a> TaskListView<'a> {
    pub fn new(tasks: &'a [TaskEntry]) -> Self {
        Self { tasks }
    }
}

impl Widget for &TaskListView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let items: Vec<ListItem> = self
            .tasks
            .iter()
            .map(|task| {
                let (icon, color) = match task.status {
                    TaskStatus::Pending => ("○", Color::DarkGray),
                    TaskStatus::Running => ("◌", Color::Yellow),
                    TaskStatus::Success => ("●", Color::Green),
                    TaskStatus::Failed => ("✗", Color::Red),
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!(" {} ", icon), Style::default().fg(color)),
                    Span::styled(task.name.clone(), Style::default().fg(Color::White)),
                ]))
            })
            .collect();

        let block = Block::default().borders(Borders::ALL).title(" tasks ");

        super::render_widget(List::new(items).block(block), area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 68. TodoListView — session todo with checkboxes
// ═══════════════════════════════════════════════════════════════════════

pub struct TodoItem {
    pub text: String,
    pub status: TodoStatus,
}

#[derive(Clone, Copy)]
pub enum TodoStatus {
    NotStarted,
    InProgress,
    Done,
}

pub struct TodoListView<'a> {
    pub items: &'a [TodoItem],
}

impl<'a> TodoListView<'a> {
    pub fn new(items: &'a [TodoItem]) -> Self {
        Self { items }
    }
}

impl Widget for &TodoListView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let list_items: Vec<ListItem> = self
            .items
            .iter()
            .map(|item| {
                let (check, color) = match item.status {
                    TodoStatus::NotStarted => ("[ ]", Color::DarkGray),
                    TodoStatus::InProgress => ("[~]", Color::Yellow),
                    TodoStatus::Done => ("[x]", Color::Green),
                };
                let text_style = match item.status {
                    TodoStatus::Done => Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::CROSSED_OUT),
                    _ => Style::default().fg(Color::White),
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{} ", check), Style::default().fg(color)),
                    Span::styled(item.text.clone(), text_style),
                ]))
            })
            .collect();

        super::render_widget(List::new(list_items), area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 70. MemoryView — memory categories as list
// ═══════════════════════════════════════════════════════════════════════

pub struct MemorySection {
    pub label: String,
    pub entry_count: usize,
    pub source: String, // "project", "global", "team"
}

pub struct MemoryView<'a> {
    pub sections: &'a [MemorySection],
}

impl<'a> MemoryView<'a> {
    pub fn new(sections: &'a [MemorySection]) -> Self {
        Self { sections }
    }
}

impl Widget for &MemoryView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let items: Vec<ListItem> = self
            .sections
            .iter()
            .map(|sec| {
                let source_color = match sec.source.as_str() {
                    "project" => Color::Cyan,
                    "global" => Color::Blue,
                    "team" => Color::Magenta,
                    _ => Color::DarkGray,
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {} ", sec.source),
                        Style::default().fg(source_color),
                    ),
                    Span::styled(sec.label.clone(), Style::default().fg(Color::White)),
                    Span::styled(
                        format!("  ({} entries)", sec.entry_count),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            })
            .collect();

        let block = Block::default().borders(Borders::ALL).title(" memory ");

        super::render_widget(List::new(items).block(block), area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 71. SessionSummary — turns, tokens, cost, files modified
// ═══════════════════════════════════════════════════════════════════════

pub struct SessionSummary<'a> {
    pub turns: u32,
    pub tokens_used: u64,
    pub cost: f64,
    pub files_modified: u32,
    pub tools_called: u32,
    pub model: &'a str,
}

impl<'a> SessionSummary<'a> {
    pub fn new(model: &'a str) -> Self {
        Self {
            turns: 0,
            tokens_used: 0,
            cost: 0.0,
            files_modified: 0,
            tools_called: 0,
            model,
        }
    }
}

impl Widget for &SessionSummary<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let widths = [Constraint::Length(18), Constraint::Min(10)];

        let rows = vec![
            Row::new(vec![
                Cell::from(Span::styled("Model", Style::default().fg(Color::DarkGray))),
                Cell::from(Span::styled(
                    self.model.to_string(),
                    Style::default().fg(Color::Cyan),
                )),
            ]),
            Row::new(vec![
                Cell::from(Span::styled("Turns", Style::default().fg(Color::DarkGray))),
                Cell::from(Span::styled(
                    self.turns.to_string(),
                    Style::default().fg(Color::White),
                )),
            ]),
            Row::new(vec![
                Cell::from(Span::styled("Tokens", Style::default().fg(Color::DarkGray))),
                Cell::from(Span::styled(
                    format!("{}", self.tokens_used),
                    Style::default().fg(Color::White),
                )),
            ]),
            Row::new(vec![
                Cell::from(Span::styled("Cost", Style::default().fg(Color::DarkGray))),
                Cell::from(Span::styled(
                    format!("${:.4}", self.cost),
                    Style::default().fg(Color::Yellow),
                )),
            ]),
            Row::new(vec![
                Cell::from(Span::styled(
                    "Files modified",
                    Style::default().fg(Color::DarkGray),
                )),
                Cell::from(Span::styled(
                    self.files_modified.to_string(),
                    Style::default().fg(Color::Green),
                )),
            ]),
            Row::new(vec![
                Cell::from(Span::styled(
                    "Tool calls",
                    Style::default().fg(Color::DarkGray),
                )),
                Cell::from(Span::styled(
                    self.tools_called.to_string(),
                    Style::default().fg(Color::White),
                )),
            ]),
        ];

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" session summary ");

        super::render_widget(Table::new(rows, widths).block(block), area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 72. AgentTree — coordinator → sub-agent hierarchy
// ═══════════════════════════════════════════════════════════════════════

pub struct AgentNode {
    pub name: String,
    pub status: String,
    pub depth: u16,
}

pub struct AgentTree<'a> {
    pub nodes: &'a [AgentNode],
}

impl<'a> AgentTree<'a> {
    pub fn new(nodes: &'a [AgentNode]) -> Self {
        Self { nodes }
    }
}

impl Widget for &AgentTree<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let items: Vec<ListItem> = self
            .nodes
            .iter()
            .map(|node| {
                let indent = "  ".repeat(node.depth as usize);
                let connector = if node.depth > 0 { "├─ " } else { "" };

                let status_color = match node.status.as_str() {
                    "running" => Color::Yellow,
                    "done" => Color::Green,
                    "error" => Color::Red,
                    _ => Color::DarkGray,
                };

                ListItem::new(Line::from(vec![
                    Span::raw(indent),
                    Span::styled(connector.to_string(), Style::default().fg(Color::DarkGray)),
                    Span::styled(node.name.clone(), Style::default().fg(Color::Cyan)),
                    Span::styled(
                        format!(" [{}]", node.status),
                        Style::default().fg(status_color),
                    ),
                ]))
            })
            .collect();

        let block = Block::default().borders(Borders::ALL).title(" agents ");

        super::render_widget(List::new(items).block(block), area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 73. SkillBrowser — browse/search/activate skills
// ═══════════════════════════════════════════════════════════════════════

pub struct SkillEntry {
    pub name: String,
    pub description: String,
    pub active: bool,
}

pub struct SkillBrowser<'a> {
    pub skills: &'a [SkillEntry],
    pub selected: Option<usize>,
}

impl<'a> SkillBrowser<'a> {
    pub fn new(skills: &'a [SkillEntry]) -> Self {
        Self {
            skills,
            selected: None,
        }
    }

    pub fn selected(mut self, idx: usize) -> Self {
        self.selected = Some(idx);
        self
    }
}

impl Widget for &SkillBrowser<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let items: Vec<ListItem> = self
            .skills
            .iter()
            .enumerate()
            .map(|(i, skill)| {
                let active_icon = if skill.active { "●" } else { "○" };
                let style = if Some(i) == self.selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };

                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {} ", active_icon),
                        Style::default().fg(if skill.active {
                            Color::Green
                        } else {
                            Color::DarkGray
                        }),
                    ),
                    Span::styled(skill.name.clone(), style),
                    Span::styled(
                        format!("  {}", skill.description),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            })
            .collect();

        let block = Block::default().borders(Borders::ALL).title(" skills ");

        super::render_widget(List::new(items).block(block), area, buf);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 74. McpServerStatus — MCP server connections with health
// ═══════════════════════════════════════════════════════════════════════

pub struct McpServer {
    pub name: String,
    pub status: McpStatus,
    pub tool_count: usize,
}

#[derive(Clone, Copy)]
pub enum McpStatus {
    Connected,
    Disconnected,
    Error,
    Starting,
}

pub struct McpServerStatus<'a> {
    pub servers: &'a [McpServer],
}

impl<'a> McpServerStatus<'a> {
    pub fn new(servers: &'a [McpServer]) -> Self {
        Self { servers }
    }
}

impl Widget for &McpServerStatus<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let widths = [
            Constraint::Length(3),
            Constraint::Min(15),
            Constraint::Length(8),
        ];

        let rows: Vec<Row> = self
            .servers
            .iter()
            .map(|server| {
                let (icon, color) = match server.status {
                    McpStatus::Connected => ("●", Color::Green),
                    McpStatus::Disconnected => ("○", Color::DarkGray),
                    McpStatus::Error => ("✗", Color::Red),
                    McpStatus::Starting => ("◌", Color::Yellow),
                };
                Row::new(vec![
                    Cell::from(Span::styled(icon.to_string(), Style::default().fg(color))),
                    Cell::from(Span::styled(
                        server.name.clone(),
                        Style::default().fg(Color::White),
                    )),
                    Cell::from(Span::styled(
                        format!("{} tools", server.tool_count),
                        Style::default().fg(Color::DarkGray),
                    )),
                ])
            })
            .collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" MCP servers ");

        super::render_widget(Table::new(rows, widths).block(block), area, buf);
    }
}
