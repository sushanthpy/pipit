//! Full-screen ratatui TUI for pipit-cli.
//!
//! Layout:
//!   ┌─ Status bar ──────────────────────────────────────┐
//!   │ repo · branch · model · mode · tokens · cost      │
//!   ├─ Activity pane ──────────────────────────────────-─┤
//!   │ (scrolling log of agent actions + content)         │
//!   ├─ Input bar ───────────────────────────────────────-┤
//!   │ you› _                                             │
//!   └───────────────────────────────────────────────────-┘

use crate::tui::StatusBarState;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers, EnableBracketedPaste, DisableBracketedPaste},
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

/// Lines displayed in the activity pane (agent output, tool actions, etc.).
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
    pub activity_lines: Vec<ActivityLine>,
    pub input_buffer: String,
    pub cursor_pos: usize,
    pub scroll_offset: u16,
    pub should_quit: bool,
    /// When set, the input has been submitted and should be consumed.
    pub submitted_input: Option<String>,
    /// Current streaming response text (in progress).
    pub streaming_text: String,
    /// Whether the agent is currently working.
    pub is_working: bool,
    /// Current working status label.
    pub working_label: String,
}

impl TuiState {
    pub fn new(status: StatusBarState) -> Self {
        Self {
            status,
            activity_lines: Vec::new(),
            input_buffer: String::new(),
            cursor_pos: 0,
            scroll_offset: 0,
            should_quit: false,
            submitted_input: None,
            streaming_text: String::new(),
            is_working: false,
            working_label: String::new(),
        }
    }

    /// Start a working state (agent is processing).
    pub fn begin_working(&mut self, label: &str) {
        self.is_working = true;
        self.working_label = label.to_string();
        self.streaming_text.clear();
    }

    /// Finish working — commit the streaming text to the activity log.
    pub fn finish_working(&mut self) {
        if !self.streaming_text.is_empty() {
            // Split streaming text into lines and add to activity
            for line in self.streaming_text.lines() {
                if !line.trim().is_empty() {
                    self.activity_lines.push(ActivityLine {
                        icon: String::new(),
                        color: Color::White,
                        text: line.to_string(),
                    });
                }
            }
            self.streaming_text.clear();
        }
        self.is_working = false;
        self.working_label.clear();
        self.auto_scroll();
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
        self.auto_scroll();
    }

    pub fn push_content(&mut self, text: &str) {
        // Append to the streaming buffer — displayed live in the streaming section
        self.streaming_text.push_str(text);
    }

    fn auto_scroll(&mut self) {
        let total = self.activity_lines.len() as u16;
        if total > 10 {
            self.scroll_offset = total.saturating_sub(10);
        }
    }

    /// Handle a paste event — insert the entire pasted block as one input.
    pub fn handle_paste(&mut self, text: &str) {
        // Replace newlines with spaces so pasted error messages become one line
        let cleaned = text.replace('\n', " ").replace('\r', "");
        // Insert at cursor position (byte-safe)
        let byte_pos = self.cursor_byte_pos();
        self.input_buffer.insert_str(byte_pos, &cleaned);
        self.cursor_pos += cleaned.chars().count();
    }

    /// Convert char-based cursor_pos to byte offset in input_buffer.
    fn cursor_byte_pos(&self) -> usize {
        self.input_buffer
            .char_indices()
            .nth(self.cursor_pos)
            .map(|(i, _)| i)
            .unwrap_or(self.input_buffer.len())
    }

    /// Get the visible portion of the input buffer for rendering.
    /// Returns (display_string, cursor_x_offset_in_display).
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

/// The shared state handle.
pub type SharedTuiState = Arc<Mutex<TuiState>>;

/// Initialize the terminal for full-screen TUI mode.
pub fn init_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stderr>>> {
    enable_raw_mode()?;
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stderr);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Restore the terminal to normal mode.
pub fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stderr>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), DisableBracketedPaste, LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// Draw the full TUI frame.
pub fn draw(frame: &mut Frame, state: &TuiState) {
    let area = frame.area();

    // Layout: status bar (3 lines) | activity (flexible) | input (3 lines)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // status bar
            Constraint::Min(5),    // activity pane
            Constraint::Length(3), // input bar
        ])
        .split(area);

    draw_status_bar(frame, chunks[0], state);
    draw_activity_pane(frame, chunks[1], state);
    draw_input_bar(frame, chunks[2], state);
}

fn draw_status_bar(frame: &mut Frame, area: Rect, state: &TuiState) {
    let s = &state.status;
    let branch_marker = if s.dirty { "*" } else { "" };
    let token_pct = if s.tokens_limit > 0 {
        (s.tokens_used * 100) / s.tokens_limit
    } else {
        0
    };

    let line1 = Line::from(vec![
        Span::styled(" pipit", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled(" · ", Style::default().fg(Color::DarkGray)),
        Span::styled(&s.repo_name, Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled(format!("{}{}", s.branch, branch_marker), Style::default().fg(Color::Green)),
        Span::raw("  "),
        Span::styled(&s.model, Style::default().fg(Color::Yellow)),
        Span::raw("  "),
        Span::styled(s.approval_mode.label(), Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
    ]);

    let token_color = if token_pct > 85 {
        Color::Red
    } else if token_pct > 60 {
        Color::Yellow
    } else {
        Color::Green
    };

    let line2 = Line::from(vec![
        Span::raw(" tokens: "),
        Span::styled(format!("{}%", token_pct), Style::default().fg(token_color)),
        Span::raw(format!("  ${:.4}", s.cost)),
    ]);

    let text = ratatui::text::Text::from(vec![line1, line2]);
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));
    let paragraph = Paragraph::new(text).block(block);
    frame.render_widget(paragraph, area);
}

fn draw_activity_pane(frame: &mut Frame, area: Rect, state: &TuiState) {
    let inner_height = area.height.saturating_sub(2) as usize;

    // Build all display lines: activity log + streaming section
    let mut all_lines: Vec<Line> = Vec::new();

    // 1. Activity log (completed actions)
    for entry in &state.activity_lines {
        if entry.icon.is_empty() {
            all_lines.push(Line::from(Span::styled(
                entry.text.clone(),
                Style::default().fg(entry.color),
            )));
        } else {
            all_lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} ", entry.icon),
                    Style::default().fg(entry.color),
                ),
                Span::raw(entry.text.clone()),
            ]));
        }
    }

    // 2. Streaming section (if agent is working)
    if state.is_working {
        if !state.working_label.is_empty() {
            all_lines.push(Line::from(""));
            all_lines.push(Line::from(vec![
                Span::styled("  ⟳ ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    state.working_label.clone(),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
            ]));
        }
        // Show streaming text (last N lines that fit)
        if !state.streaming_text.is_empty() {
            all_lines.push(Line::from(Span::styled(
                "  ─────",
                Style::default().fg(Color::DarkGray),
            )));
            for line in state.streaming_text.lines() {
                all_lines.push(Line::from(Span::styled(
                    format!("  {}", line),
                    Style::default().fg(Color::White),
                )));
            }
        }
    }

    // Auto-scroll: show the last N lines that fit
    let total = all_lines.len();
    let start = if total > inner_height {
        total - inner_height
    } else {
        0
    };

    let visible_lines: Vec<Line> = all_lines[start..].to_vec();

    let scroll_indicator = if total > inner_height {
        format!(" [{}/{}] ", total, total)
    } else {
        String::new()
    };

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            scroll_indicator,
            Style::default().fg(Color::DarkGray),
        ));

    let text = ratatui::text::Text::from(visible_lines);
    let paragraph = Paragraph::new(text).block(block).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_input_bar(frame: &mut Frame, area: Rect, state: &TuiState) {
    let prompt = "you› ";
    let max_visible = (area.width as usize).saturating_sub(prompt.len() + 2);

    let (display_text, cursor_display_pos) = state.visible_input(max_visible);
    let char_count = state.input_buffer.chars().count();

    let line = Line::from(vec![
        Span::styled(prompt, Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        Span::raw(&display_text),
    ]);

    let char_count_hint = if char_count > max_visible {
        format!(" [{} chars]", char_count)
    } else {
        String::new()
    };

    let hint_line = Line::from(vec![
        Span::styled(
            format!(" /help  @file  !shell{}", char_count_hint),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    let text = ratatui::text::Text::from(vec![line, hint_line]);
    let block = Block::default();
    let paragraph = Paragraph::new(text).block(block);
    frame.render_widget(paragraph, area);

    // Position cursor within visible portion
    let cursor_x = area.x + prompt.len() as u16 + cursor_display_pos as u16;
    let cursor_y = area.y;
    frame.set_cursor_position((cursor_x, cursor_y));
}

/// Handle a key event, updating state. Returns true if the event was consumed.
pub fn handle_key(state: &mut TuiState, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter => {
            if !state.input_buffer.is_empty() {
                let input = state.input_buffer.clone();
                state.input_buffer.clear();
                state.cursor_pos = 0;
                // Show user input in activity (truncated for display)
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
                // Find the char at this position and remove it
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
            if state.cursor_pos > 0 {
                state.cursor_pos -= 1;
            }
            true
        }
        KeyCode::Right => {
            let char_count = state.input_buffer.chars().count();
            if state.cursor_pos < char_count {
                state.cursor_pos += 1;
            }
            true
        }
        KeyCode::Home => {
            state.cursor_pos = 0;
            true
        }
        KeyCode::End => {
            state.cursor_pos = state.input_buffer.chars().count();
            true
        }
        KeyCode::Up => {
            // Scroll up
            if state.scroll_offset > 0 {
                state.scroll_offset -= 1;
            }
            true
        }
        KeyCode::Down => {
            // Scroll down
            let max = state.activity_lines.len() as u16;
            if state.scroll_offset < max {
                state.scroll_offset += 1;
            }
            true
        }
        KeyCode::PageUp => {
            state.scroll_offset = state.scroll_offset.saturating_sub(10);
            true
        }
        KeyCode::PageDown => {
            let max = state.activity_lines.len() as u16;
            state.scroll_offset = (state.scroll_offset + 10).min(max);
            true
        }
        _ => false,
    }
}
