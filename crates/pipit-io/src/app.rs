//! Full-screen ratatui TUI for pipit-cli.
//!
//! Layout:
//!   ┌─ Status bar ──────────────────────────────────────┐
//!   │ pipit · repo · branch · model · mode · tokens     │
//!   ├─ Task / Phase ────────────────────────────────────┤
//!   │ task: explain codebase          phase: executing   │
//!   ├─ Timeline ────────┬─ Response ───────────────────-┤
//!   │ ◆ diagnostic plan │ The codebase is a Rust CLI... │
//!   │ ○ Read src/main   │                               │
//!   │ ● Edit lib.rs     │ ## Architecture               │
//!   │ · turn 1 done     │ - pipit-core: agent loop      │
//!   ├─ Composer ─────────────────────────────────────────┤
//!   │ you› _                                             │
//!   │ Tab commands · @file · !shell · Ctrl-J multiline   │
//!   └───────────────────────────────────────────────────-┘

use crate::tui::StatusBarState;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, EnableBracketedPaste, DisableBracketedPaste},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io;
use std::sync::{Arc, Mutex};

/// Lines displayed in the timeline (left pane: tool actions, plans, turns).
#[derive(Debug, Clone)]
pub struct ActivityLine {
    pub icon: String,
    pub color: Color,
    pub text: String,
}

/// Shared TUI state that the event handler and main loop coordinate through.
#[derive(Debug)]
pub struct TuiState {
    pub status: StatusBarState,
    /// Timeline entries (left pane): compact agent actions.
    pub activity_lines: Vec<ActivityLine>,
    /// Content lines (right pane): natural-language responses.
    pub content_lines: Vec<String>,
    pub input_buffer: String,
    pub cursor_pos: usize,
    pub scroll_offset: u16,
    pub content_scroll_offset: u16,
    pub should_quit: bool,
    /// When set, the input has been submitted and should be consumed.
    pub submitted_input: Option<String>,
    /// Current streaming response text (in progress).
    pub streaming_text: String,
    /// Whether the agent is currently working.
    pub is_working: bool,
    /// Current working status label.
    pub working_label: String,
    /// Current task description (from user prompt).
    pub task_label: String,
    /// Current phase (planning, executing, verifying, etc.).
    pub phase_label: String,
    /// Agent mode (fast, balanced, guarded, custom).
    pub agent_mode: String,
    /// Whether the user has submitted at least one input.
    pub has_received_input: bool,
}

impl TuiState {
    pub fn new(status: StatusBarState) -> Self {
        Self {
            status,
            activity_lines: Vec::new(),
            content_lines: Vec::new(),
            input_buffer: String::new(),
            cursor_pos: 0,
            scroll_offset: 0,
            content_scroll_offset: 0,
            should_quit: false,
            submitted_input: None,
            streaming_text: String::new(),
            is_working: false,
            working_label: String::new(),
            task_label: String::new(),
            phase_label: String::new(),
            agent_mode: "fast".to_string(),
            has_received_input: false,
        }
    }

    /// Start a working state (agent is processing).
    pub fn begin_working(&mut self, label: &str) {
        self.is_working = true;
        self.working_label = label.to_string();
        self.phase_label = label.trim_end_matches('…').to_string();
    }

    /// Finish working — commit the streaming text to the content pane.
    pub fn finish_working(&mut self) {
        if !self.streaming_text.is_empty() {
            for line in self.streaming_text.lines() {
                self.content_lines.push(line.to_string());
            }
            self.streaming_text.clear();
        }
        self.is_working = false;
        self.working_label.clear();
        self.auto_scroll_content();
    }

    pub fn push_activity(&mut self, icon: &str, color: Color, text: String) {
        self.activity_lines.push(ActivityLine {
            icon: icon.to_string(),
            color,
            text,
        });
        if self.activity_lines.len() > 500 {
            self.activity_lines.drain(..100);
        }
        self.auto_scroll_timeline();
    }

    pub fn push_content(&mut self, text: &str) {
        self.streaming_text.push_str(text);
    }

    fn auto_scroll_timeline(&mut self) {
        let total = self.activity_lines.len() as u16;
        if total > 10 {
            self.scroll_offset = total.saturating_sub(10);
        }
    }

    fn auto_scroll_content(&mut self) {
        let total = self.content_lines.len() as u16;
        if total > 10 {
            self.content_scroll_offset = total.saturating_sub(10);
        }
    }

    /// Handle a paste event.
    pub fn handle_paste(&mut self, text: &str) {
        let cleaned = text.replace('\n', " ").replace('\r', "");
        let byte_pos = self.cursor_byte_pos();
        self.input_buffer.insert_str(byte_pos, &cleaned);
        self.cursor_pos += cleaned.chars().count();
    }

    fn cursor_byte_pos(&self) -> usize {
        self.input_buffer
            .char_indices()
            .nth(self.cursor_pos)
            .map(|(i, _)| i)
            .unwrap_or(self.input_buffer.len())
    }

    pub fn visible_input(&self, max_chars: usize) -> (String, usize) {
        let chars: Vec<char> = self.input_buffer.chars().collect();
        let total = chars.len();
        if total <= max_chars {
            (self.input_buffer.clone(), self.cursor_pos)
        } else {
            let half = max_chars / 2;
            let start = self.cursor_pos.saturating_sub(half);
            let end = (start + max_chars).min(total);
            let start = end.saturating_sub(max_chars);
            let display: String = chars[start..end].iter().collect();
            (display, self.cursor_pos - start)
        }
    }
}

pub type SharedTuiState = Arc<Mutex<TuiState>>;

pub fn init_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stderr>>> {
    enable_raw_mode()?;
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stderr);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

pub fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stderr>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), DisableBracketedPaste, LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// Draw the full TUI frame.
pub fn draw(frame: &mut Frame, state: &TuiState) {
    let area = frame.area();

    // Layout: status(2) | task/phase(1) | main_pane(flex) | input(3)
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),  // status bar
            Constraint::Length(1),  // task / phase strip
            Constraint::Min(5),    // main pane (timeline | content)
            Constraint::Length(3), // input bar
        ])
        .split(area);

    draw_status_bar(frame, vertical[0], state);
    draw_task_phase_strip(frame, vertical[1], state);

    if state.has_received_input {
        // Split main pane: left timeline (30%) | right content (70%)
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(30),
                Constraint::Percentage(70),
            ])
            .split(vertical[2]);
        draw_timeline_pane(frame, cols[0], state);
        draw_content_pane(frame, cols[1], state);
    } else {
        draw_welcome_pane(frame, vertical[2], state);
    }

    draw_input_bar(frame, vertical[3], state);
}

fn draw_status_bar(frame: &mut Frame, area: Rect, state: &TuiState) {
    let s = &state.status;
    let branch_marker = if s.dirty { "*" } else { "" };
    let token_pct = s.token_pct();

    let token_color = if token_pct > 85 { Color::Red }
        else if token_pct > 60 { Color::Yellow }
        else { Color::Green };

    let verify_chip = match &s.verification {
        crate::tui::VerificationState::Passing => Span::styled(" ✓pass ", Style::default().fg(Color::Green)),
        crate::tui::VerificationState::Failing(_) => Span::styled(" ✗fail ", Style::default().fg(Color::Red)),
        crate::tui::VerificationState::Running => Span::styled(" ⟳verify ", Style::default().fg(Color::Cyan)),
        crate::tui::VerificationState::Unknown => Span::styled("", Style::default()),
    };

    let mode_chip = if state.agent_mode != "fast" {
        Span::styled(
            format!(" {} ", state.agent_mode),
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled("", Style::default())
    };

    let line = Line::from(vec![
        Span::styled(" pipit", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled(" · ", Style::default().fg(Color::DarkGray)),
        Span::styled(&s.repo_name, Style::default().fg(Color::Cyan)),
        Span::styled(format!(" {}{}", s.branch, branch_marker), Style::default().fg(Color::Green)),
        Span::styled(" · ", Style::default().fg(Color::DarkGray)),
        Span::styled(&s.model, Style::default().fg(Color::Yellow)),
        Span::raw(" "),
        Span::styled(s.approval_mode.label(), Style::default().fg(Color::Magenta)),
        mode_chip,
        verify_chip,
        Span::styled(" · ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{}%", token_pct), Style::default().fg(token_color)),
        Span::styled(format!(" ${:.4}", s.cost), Style::default().fg(Color::DarkGray)),
    ]);

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));
    let paragraph = Paragraph::new(line).block(block);
    frame.render_widget(paragraph, area);
}

fn draw_task_phase_strip(frame: &mut Frame, area: Rect, state: &TuiState) {
    if state.task_label.is_empty() && state.phase_label.is_empty() {
        // Empty state — just a thin border
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray));
        frame.render_widget(block, area);
        return;
    }

    let task_display = if state.task_label.len() > 60 {
        format!("{}…", &state.task_label.chars().take(58).collect::<String>())
    } else {
        state.task_label.clone()
    };

    let line = Line::from(vec![
        Span::styled(" task: ", Style::default().fg(Color::DarkGray)),
        Span::styled(&task_display, Style::default().fg(Color::White)),
        Span::raw("  "),
        Span::styled("phase: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            &state.phase_label,
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
    ]);

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));
    let paragraph = Paragraph::new(line).block(block);
    frame.render_widget(paragraph, area);
}

fn draw_timeline_pane(frame: &mut Frame, area: Rect, state: &TuiState) {
    let inner_height = area.height.saturating_sub(2) as usize;
    let total = state.activity_lines.len();

    let start = if total > inner_height {
        total - inner_height
    } else {
        0
    };

    let lines: Vec<Line> = state.activity_lines[start..]
        .iter()
        .map(|entry| {
            if entry.icon.is_empty() {
                Line::from(Span::styled(
                    truncate_str(&entry.text, (area.width as usize).saturating_sub(4)),
                    Style::default().fg(entry.color),
                ))
            } else {
                Line::from(vec![
                    Span::styled(
                        format!(" {} ", entry.icon),
                        Style::default().fg(entry.color),
                    ),
                    Span::raw(truncate_str(
                        &entry.text,
                        (area.width as usize).saturating_sub(6),
                    )),
                ])
            }
        })
        .collect();

    // Working indicator at bottom
    let mut display = lines;
    if state.is_working && !state.working_label.is_empty() {
        display.push(Line::from(vec![
            Span::styled(" ⟳ ", Style::default().fg(Color::Cyan)),
            Span::styled(
                &state.working_label,
                Style::default().fg(Color::Cyan),
            ),
        ]));
    }

    let scroll_info = if total > inner_height {
        format!(" {}/{} ", start + inner_height, total)
    } else {
        String::new()
    };

    let block = Block::default()
        .borders(Borders::RIGHT | Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            " timeline ",
            Style::default().fg(Color::DarkGray),
        ))
        .title_bottom(Span::styled(
            scroll_info,
            Style::default().fg(Color::DarkGray),
        ));

    let paragraph = Paragraph::new(display).block(block);
    frame.render_widget(paragraph, area);
}

fn draw_content_pane(frame: &mut Frame, area: Rect, state: &TuiState) {
    let inner_height = area.height.saturating_sub(2) as usize;

    // Build content: committed lines + streaming
    let mut all_lines: Vec<Line> = state
        .content_lines
        .iter()
        .map(|l| Line::from(Span::styled(format!(" {}", l), Style::default().fg(Color::White))))
        .collect();

    // Add streaming text
    if !state.streaming_text.is_empty() {
        for line in state.streaming_text.lines() {
            all_lines.push(Line::from(Span::styled(
                format!(" {}", line),
                Style::default().fg(Color::White),
            )));
        }
    }

    let total = all_lines.len();
    let start = if total > inner_height {
        total - inner_height
    } else {
        0
    };
    let visible: Vec<Line> = all_lines[start..].to_vec();

    let scroll_info = if total > inner_height {
        format!(" {}/{} ", start + inner_height, total)
    } else {
        String::new()
    };

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            " response ",
            Style::default().fg(Color::DarkGray),
        ))
        .title_bottom(Span::styled(
            scroll_info,
            Style::default().fg(Color::DarkGray),
        ));

    let paragraph = Paragraph::new(visible).block(block).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_welcome_pane(frame: &mut Frame, area: Rect, _state: &TuiState) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            r"       _._", Style::default().fg(Color::Yellow),
        )),
        Line::from(Span::styled(
            r"      (o >", Style::default().fg(Color::Yellow),
        )),
        Line::from(Span::styled(
            r"     / / \", Style::default().fg(Color::Yellow),
        )),
        Line::from(Span::styled(
            r"    (_|  /", Style::default().fg(Color::Yellow),
        )),
        Line::from(Span::styled(
            r#"      " ""#, Style::default().fg(Color::Yellow),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "      pipit",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "      AI coding agent",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("      /help", Style::default().fg(Color::Cyan)),
            Span::styled("  commands", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("      @file", Style::default().fg(Color::Green)),
            Span::styled("  attach context", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("      !cmd ", Style::default().fg(Color::Magenta)),
            Span::styled("  run shell", Style::default().fg(Color::DarkGray)),
        ]),
    ];

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));
    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn draw_input_bar(frame: &mut Frame, area: Rect, state: &TuiState) {
    let prompt = "you› ";
    let max_visible = (area.width as usize).saturating_sub(prompt.len() + 2);
    let (display_text, cursor_display_pos) = state.visible_input(max_visible);

    let line = Line::from(vec![
        Span::styled(prompt, Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        Span::raw(&display_text),
    ]);

    let hint_text = if state.is_working {
        " Esc stop · /help · Ctrl-C quit"
    } else {
        " /help · @file · !shell · Esc cancel · Ctrl-C quit"
    };

    let hint_line = Line::from(Span::styled(
        hint_text,
        Style::default().fg(Color::DarkGray),
    ));

    let text = ratatui::text::Text::from(vec![line, hint_line]);
    let paragraph = Paragraph::new(text).block(Block::default());
    frame.render_widget(paragraph, area);

    let cursor_x = area.x + prompt.len() as u16 + cursor_display_pos as u16;
    let cursor_y = area.y;
    frame.set_cursor_position((cursor_x, cursor_y));
}

/// Handle a key event, updating state. Returns true if the event was consumed.
pub fn handle_key(state: &mut TuiState, key: KeyEvent) -> bool {
    // Only handle Press and Repeat — ignore Release events (crossterm 0.26+ on macOS)
    if key.kind == KeyEventKind::Release {
        return false;
    }

    match key.code {
        KeyCode::Enter => {
            if !state.input_buffer.is_empty() {
                let input = state.input_buffer.clone();
                state.input_buffer.clear();
                state.cursor_pos = 0;

                // Set task label from first input
                if !state.has_received_input {
                    state.has_received_input = true;
                    state.task_label = if input.len() > 80 {
                        format!("{}…", &input.chars().take(78).collect::<String>())
                    } else {
                        input.clone()
                    };
                    // Clear welcome content
                    state.content_lines.clear();
                }

                let display = if input.len() > 120 {
                    format!("{}… [{} chars]", &input.chars().take(100).collect::<String>(), input.chars().count())
                } else {
                    input.clone()
                };
                state.push_activity("›", Color::Green, display);
                state.submitted_input = Some(input);
            }
            true
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.should_quit = true;
            true
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.should_quit = true;
            true
        }
        KeyCode::Char(c) => {
            let byte_pos = state.cursor_byte_pos();
            state.input_buffer.insert(byte_pos, c);
            state.cursor_pos += 1;
            true
        }
        KeyCode::Backspace => {
            if state.cursor_pos > 0 {
                state.cursor_pos -= 1;
                let byte_pos = state.cursor_byte_pos();
                if let Some((_, ch)) = state.input_buffer.char_indices().nth(state.cursor_pos) {
                    state.input_buffer.drain(byte_pos..byte_pos + ch.len_utf8());
                }
            }
            true
        }
        KeyCode::Delete => {
            let char_count = state.input_buffer.chars().count();
            if state.cursor_pos < char_count {
                let byte_pos = state.cursor_byte_pos();
                if let Some((_, ch)) = state.input_buffer.char_indices().nth(state.cursor_pos) {
                    state.input_buffer.drain(byte_pos..byte_pos + ch.len_utf8());
                }
            }
            true
        }
        KeyCode::Left => {
            if state.cursor_pos > 0 { state.cursor_pos -= 1; }
            true
        }
        KeyCode::Right => {
            let c = state.input_buffer.chars().count();
            if state.cursor_pos < c { state.cursor_pos += 1; }
            true
        }
        KeyCode::Home => { state.cursor_pos = 0; true }
        KeyCode::End => {
            state.cursor_pos = state.input_buffer.chars().count();
            true
        }
        KeyCode::Up | KeyCode::PageUp => {
            // Scroll timeline up
            if state.scroll_offset > 0 {
                state.scroll_offset = state.scroll_offset.saturating_sub(
                    if key.code == KeyCode::PageUp { 10 } else { 1 }
                );
            }
            true
        }
        KeyCode::Down | KeyCode::PageDown => {
            let max = state.activity_lines.len() as u16;
            let delta = if key.code == KeyCode::PageDown { 10 } else { 1 };
            state.scroll_offset = (state.scroll_offset + delta).min(max);
            true
        }
        _ => false,
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}
