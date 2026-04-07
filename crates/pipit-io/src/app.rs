//! Full-screen ratatui TUI for pipit-cli.
//!
//! Two-mode design: Shell mode (default, terminal-first) and Task mode
//! (focused single-column view when agent is working).
//!
//! Layout (Shell mode):
//!   ┌─ top bar ─────────────────────────────────────────┐
//!   │ repo · branch · model · mode                      │
//!   │                                                   │
//!   │ > _                                               │
//!   │                                                   │
//!   │ Recent task: …                                    │
//!   │ Hints: /help  /review  /tasks                     │
//!   │                                                   │
//!   │ footer shortcuts                                  │
//!   └───────────────────────────────────────────────────┘
//!
//! Layout (Task mode):
//!   ┌─ top bar ─────────────────────────────────────────┐
//!   │ Task: … · status                                  │
//!   │                                                   │
//!   │ Activity                                          │
//!   │  • opened file.rs                                 │
//!   │  • ran tests                                      │
//!   │                                                   │
//!   │ Response stream                                   │
//!   │                                                   │
//!   │ footer shortcuts                                  │
//!   └───────────────────────────────────────────────────┘

use crate::composer::{self, Composer};
use crate::tui::StatusBarState;
use crossterm::{
    event::{EnableBracketedPaste, DisableBracketedPaste, EnableMouseCapture, DisableMouseCapture, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame, Terminal,
};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

/// Check if colors should be suppressed (NO_COLOR standard, TERM=dumb).
/// See https://no-color.org/
pub fn no_color() -> bool {
    static NO_COLOR: OnceLock<bool> = OnceLock::new();
    *NO_COLOR.get_or_init(|| {
        std::env::var("NO_COLOR").is_ok()
            || std::env::var("TERM").as_deref() == Ok("dumb")
    })
}

/// Cached syntect highlighting resources.
fn syntax_set() -> &'static syntect::parsing::SyntaxSet {
    static SS: OnceLock<syntect::parsing::SyntaxSet> = OnceLock::new();
    SS.get_or_init(syntect::parsing::SyntaxSet::load_defaults_newlines)
}

fn highlight_theme() -> &'static syntect::highlighting::Theme {
    static TH: OnceLock<syntect::highlighting::Theme> = OnceLock::new();
    TH.get_or_init(|| {
        let ts = syntect::highlighting::ThemeSet::load_defaults();
        ts.themes.get("base16-ocean.dark").cloned()
            .unwrap_or_else(|| ts.themes.into_values().next().unwrap())
    })
}

/// Active tool execution info for the stable tool output region.
#[derive(Debug, Clone)]
pub struct ActiveToolInfo {
    pub tool_name: String,
    pub args_summary: String,
    pub started_at: std::time::Instant,
}

/// UI mode — determines which screen is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiMode {
    /// Default. Clean terminal-first prompt.
    Shell,
    /// Focused single-column task view while the agent works.
    Task,
}

/// Overlay — temporary modal/drawer on top of the current mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
}

/// Terminal width classification for responsive layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidthClass {
    /// >= 140 cols
    Wide,
    /// 100–139 cols
    Standard,
    /// 80–99 cols
    Compact,
    /// < 80 cols
    TooSmall,
}

impl WidthClass {
    pub fn from_width(w: u16) -> Self {
        match w {
            0..=79 => Self::TooSmall,
            80..=99 => Self::Compact,
            100..=139 => Self::Standard,
            _ => Self::Wide,
        }
    }
}

/// Lines displayed in the timeline (left pane: tool actions, plans, turns).
#[derive(Debug, Clone)]
pub struct ActivityLine {
    pub icon: String,
    pub color: Color,
    pub text: String,
}

/// Maximum content lines retained. Beyond this, oldest lines are evicted.
/// Prevents unbounded memory growth during long sessions.
const MAX_CONTENT_LINES: usize = 2000;

/// Maximum activity entries retained.
const MAX_ACTIVITY_LINES: usize = 300;

/// How many lines to evict when the cap is hit (batch eviction for efficiency).
const CONTENT_EVICT_BATCH: usize = 500;
const ACTIVITY_EVICT_BATCH: usize = 100;

/// Maximum streaming text bytes before compaction.
const MAX_STREAMING_BYTES: usize = 256_000;

/// Shared TUI state that the event handler and main loop coordinate through.
#[derive(Debug)]
pub struct TuiState {
    pub status: StatusBarState,
    /// Current UI mode.
    pub ui_mode: UiMode,
    /// Current overlay (temporary modal/drawer).
    pub overlay: Overlay,
    /// Timeline entries: compact agent actions.
    pub activity_lines: Vec<ActivityLine>,
    /// Content lines: natural-language responses.
    /// Bounded to MAX_CONTENT_LINES — oldest are evicted.
    pub content_lines: Vec<String>,
    /// The rich input composer (replaces bare input_buffer).
    pub composer: Composer,
    pub scroll_offset: u16,
    pub content_scroll_offset: u16,
    pub should_quit: bool,
    /// Current streaming response text (in progress).
    pub streaming_text: String,
    /// Whether the agent is currently working.
    pub is_working: bool,
    /// Active tool execution status (tool name + summary). When set,
    /// a fixed-height region is rendered below streaming text to prevent
    /// layout flicker during tool execution.
    pub active_tool: Option<ActiveToolInfo>,
    /// Current working status label.
    pub working_label: String,
    /// Current description (from user prompt).
    pub task_label: String,
    /// Current phase (planning, executing, verifying, etc.).
    pub phase_label: String,
    /// Agent mode (fast, balanced, guarded, custom).
    pub agent_mode: String,
    /// Whether the user has submitted at least one input.
    pub has_received_input: bool,
    /// Frame counter for spinner animation (incremented every draw cycle).
    pub spinner_frame: u64,
    /// When the current working state began.
    pub working_since: Option<std::time::Instant>,
    /// Whether the current streaming text is thinking/reasoning (not response).
    pub is_thinking: bool,
    /// Buffer for partial `<think>` / `</think>` tags split across ContentDelta boundaries.
    pub tag_buffer: String,
    /// Set to true when an agent run completes — prevents stale queued events
    /// (e.g. a lingering "Preparing next turn…" Waiting event) from restarting
    /// the working spinner after the outcome has been processed.
    pub run_finished: bool,
    /// Whether we're inside a code block for rendering purposes.
    in_code_block: bool,
    /// Pre-parsed content lines for O(1) draw cost. Rebuilt only when
    /// content_lines changes (tracked via `cached_lines_count`).
    cached_parsed_lines: Vec<Line<'static>>,
    /// Length of content_lines when cache was last built.
    cached_lines_count: usize,
    /// Current turn number for progress indicator.
    pub current_turn: u32,
    /// Max turns configured for the session.
    pub max_turns: u32,
    /// Total content lines ever produced (monotonic counter for tracking evictions).
    total_content_produced: u64,
    /// Last frame timestamp for frame-budget rendering.
    last_frame_time: Option<std::time::Instant>,
    /// Completion status — set when the agent finishes a task.
    /// Rendered as a prominent banner in the phase strip.
    pub completion_status: Option<CompletionBanner>,
}

/// Prominent completion indicator shown after the agent finishes.
#[derive(Debug, Clone)]
pub struct CompletionBanner {
    pub icon: String,
    pub message: String,
    pub color: Color,
}

impl TuiState {
    pub fn new(status: StatusBarState, project_root: PathBuf) -> Self {
        Self {
            status,
            ui_mode: UiMode::Shell,
            overlay: Overlay::None,
            activity_lines: Vec::with_capacity(MAX_ACTIVITY_LINES),
            content_lines: Vec::with_capacity(256),
            composer: Composer::new(project_root),
            scroll_offset: 0,
            content_scroll_offset: 0,
            should_quit: false,
            streaming_text: String::with_capacity(4096),
            is_working: false,
            active_tool: None,
            working_label: String::new(),
            task_label: String::new(),
            phase_label: String::new(),
            agent_mode: "fast".to_string(),
            has_received_input: false,
            spinner_frame: 0,
            working_since: None,
            is_thinking: false,
            tag_buffer: String::new(),
            run_finished: false,
            in_code_block: false,
            cached_parsed_lines: Vec::new(),
            cached_lines_count: 0,
            current_turn: 0,
            max_turns: 10,
            total_content_produced: 0,
            last_frame_time: None,
            completion_status: None,
        }
    }

    /// Start a working state (agent is processing).
    pub fn begin_working(&mut self, label: &str) {
        // Don't restart spinner from stale queued events after a run completes
        if self.run_finished {
            return;
        }
        self.is_working = true;
        self.completion_status = None;  // Clear previous completion banner
        self.working_label = label.to_string();
        self.phase_label = label.trim_end_matches('…').to_string();
        if self.working_since.is_none() {
            self.working_since = Some(std::time::Instant::now());
        }
    }

    /// Finish working — commit the streaming text to the content pane.
    /// Applies bounded eviction to prevent memory bloat.
    pub fn finish_working(&mut self) {
        if !self.streaming_text.is_empty() {
            let new_lines: Vec<String> = self.streaming_text.lines()
                .map(|line| line.to_string())
                .collect();
            self.total_content_produced += new_lines.len() as u64;
            self.content_lines.extend(new_lines);
            self.streaming_text.clear();
            // Reclaim streaming buffer if it grew large
            if self.streaming_text.capacity() > MAX_STREAMING_BYTES {
                self.streaming_text = String::with_capacity(4096);
            }
        }
        self.is_working = false;
        self.is_thinking = false;
        self.working_label.clear();
        self.working_since = None;
        self.evict_if_needed();
        self.auto_scroll_content();
    }

    /// Evict oldest content/activity lines when buffers exceed caps.
    /// Batch eviction amortizes the cost over many pushes.
    fn evict_if_needed(&mut self) {
        if self.content_lines.len() > MAX_CONTENT_LINES {
            let drain_count = CONTENT_EVICT_BATCH.min(self.content_lines.len() / 2);
            self.content_lines.drain(..drain_count);
            // Invalidate parse cache since indices shifted
            self.cached_lines_count = 0;
            self.cached_parsed_lines.clear();
        }
        if self.activity_lines.len() > MAX_ACTIVITY_LINES {
            self.activity_lines.drain(..ACTIVITY_EVICT_BATCH);
        }
    }

    pub fn push_activity(&mut self, icon: &str, color: Color, text: String) {
        self.activity_lines.push(ActivityLine {
            icon: icon.to_string(),
            color,
            text,
        });
        if self.activity_lines.len() > MAX_ACTIVITY_LINES {
            self.activity_lines.drain(..ACTIVITY_EVICT_BATCH);
        }
        self.auto_scroll_timeline();
    }

    pub fn push_content(&mut self, text: &str) {
        self.streaming_text.push_str(text);
        // Compact streaming text if it grows too large (shouldn't happen
        // in normal operation, but prevents OOM from malicious/runaway output)
        if self.streaming_text.len() > MAX_STREAMING_BYTES {
            let keep_bytes = MAX_STREAMING_BYTES / 2;
            let drain_to = self.streaming_text.len() - keep_bytes;
            // Find a safe UTF-8 boundary
            let safe_drain = self.streaming_text.ceil_char_boundary(drain_to);
            self.streaming_text.drain(..safe_drain);
        }
    }

    /// Check if the frame budget allows a redraw (target ~30fps = 33ms).
    /// Returns true if enough time has passed since the last frame.
    pub fn should_redraw(&mut self) -> bool {
        let now = std::time::Instant::now();
        match self.last_frame_time {
            Some(last) => {
                if now.duration_since(last).as_millis() >= 33 {
                    self.last_frame_time = Some(now);
                    true
                } else {
                    false
                }
            }
            None => {
                self.last_frame_time = Some(now);
                true
            }
        }
    }

    /// Clear all content and activity — used by /clear command.
    pub fn clear_all(&mut self) {
        self.content_lines.clear();
        self.activity_lines.clear();
        self.streaming_text.clear();
        self.cached_parsed_lines.clear();
        self.cached_lines_count = 0;
        self.content_scroll_offset = 0;
        self.scroll_offset = 0;
    }

    fn auto_scroll_timeline(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn auto_scroll_content(&mut self) {
        self.content_scroll_offset = 0;
    }
}

pub type SharedTuiState = Arc<Mutex<TuiState>>;

pub fn init_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stderr>>> {
    // Install panic hook BEFORE entering alternate screen so any panic
    // during draw/event-handling restores the terminal instead of leaving
    // it in a corrupted state (blank alt-screen, raw mode, hidden cursor).
    let original_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        crate::set_tui_active(false);
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stderr(),
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
        original_panic(info);
    }));

    enable_raw_mode()?;
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen, EnableBracketedPaste, EnableMouseCapture)?;
    // Gate tracing output: any tracing events after this point are discarded
    // so they don't corrupt the ratatui framebuffer.
    crate::set_tui_active(true);
    let backend = CrosstermBackend::new(stderr);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

pub fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stderr>>) -> io::Result<()> {
    // Re-enable tracing output before leaving alternate screen.
    crate::set_tui_active(false);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), DisableMouseCapture, DisableBracketedPaste, LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    // Restore the default panic hook so post-TUI panics behave normally.
    let _ = std::panic::take_hook();
    Ok(())
}

/// Draw the full TUI frame — mode-based dispatch.
pub fn draw(frame: &mut Frame, state: &TuiState) {
    let area = frame.area();
    let wc = WidthClass::from_width(area.width);

    // Hard minimum: 80 cols
    if wc == WidthClass::TooSmall {
        let msg = Paragraph::new(Line::from(vec![
            Span::styled("  Resize to at least 80 columns", Style::default().fg(Color::Yellow)),
        ]));
        frame.render_widget(msg, area);
        return;
    }

    // Root layout: top bar · body · footer
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),  // top bar
            Constraint::Min(6),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    draw_top_bar(frame, root[0], state);

    match state.ui_mode {
        UiMode::Shell => draw_shell_mode(frame, root[1], state),
        UiMode::Task => draw_task_mode(frame, root[1], state, wc),
    }

    draw_footer(frame, root[2], state);

    // Draw completion popup as overlay (must come LAST so it renders on top)
    // Find the composer area for popup positioning — it's the bottom of the body
    let body = root[1];
    let composer_h = composer::composer_height(&state.composer);
    if body.height > composer_h {
        let composer_area = Rect::new(body.x, body.y + body.height - composer_h, body.width, composer_h);
        composer::draw_completion_popup(frame, composer_area, &state.composer);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Top bar — 1 row, always visible
// ═══════════════════════════════════════════════════════════════════════════

fn draw_top_bar(frame: &mut Frame, area: Rect, state: &TuiState) {
    let s = &state.status;
    let no_c = no_color();

    let mode_label = match state.ui_mode {
        UiMode::Shell => "SHELL",
        UiMode::Task => "TASK",
    };

    let mut left = vec![
        Span::styled(
            format!(" pipit v{} ", env!("CARGO_PKG_VERSION")),
            if no_c { Style::default().add_modifier(Modifier::BOLD) }
            else { Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD) },
        ),
        Span::styled(" ", Style::default()),
        Span::styled(
            format!("{}", s.repo_name),
            if no_c { Style::default().add_modifier(Modifier::BOLD) }
            else { Style::default().fg(Color::White).add_modifier(Modifier::BOLD) },
        ),
        Span::styled(" ", Style::default().fg(Color::DarkGray)),
        Span::styled(&s.branch, if no_c { Style::default() } else { Style::default().fg(Color::Magenta) }),
        Span::styled("  ", Style::default()),
        Span::styled(
            mode_label,
            if no_c { Style::default().add_modifier(Modifier::BOLD) }
            else { Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD) },
        ),
    ];

    // Right side: model · approvals · status
    let right_text = format!(
        "model:{} {}{}",
        s.model,
        s.approval_mode.label(),
        if state.is_working { "  running" } else { "" },
    );
    let left_width: usize = left.iter().map(|sp| sp.content.len()).sum();
    let pad = (area.width as usize).saturating_sub(left_width).saturating_sub(right_text.len() + 1);
    if pad > 0 {
        left.push(Span::raw(" ".repeat(pad)));
    }
    left.push(Span::styled(
        format!("{} ", right_text),
        Style::default().fg(Color::DarkGray),
    ));

    let paragraph = Paragraph::new(Line::from(left))
        .style(if no_c { Style::default() } else { Style::default().bg(Color::Rgb(30, 30, 40)) });
    frame.render_widget(paragraph, area);
}

// ═══════════════════════════════════════════════════════════════════════════
//  Footer — 1 row, always visible, context-sensitive shortcuts
// ═══════════════════════════════════════════════════════════════════════════

fn draw_footer(frame: &mut Frame, area: Rect, state: &TuiState) {
    let hints = match state.ui_mode {
        UiMode::Shell => {
            if state.is_working {
                " esc stop · /help · ctrl+c quit"
            } else {
                " /help · @file · !shell · enter send · esc cancel · ctrl+c quit"
            }
        }
        UiMode::Task => {
            " g shell · alt+↑↓ scroll · /help · esc stop · ctrl+c quit"
        }
    };

    // Completion banner on the right if present.
    if let Some(_banner) = &state.completion_status {
        // Banner is shown above the composer in Task mode, not in footer
    }
    frame.render_widget(
        Paragraph::new(Span::styled(hints, Style::default().fg(Color::DarkGray)))
            .style(Style::default().bg(Color::Rgb(30, 30, 40))),
        area,
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  Shell mode — clean terminal-first prompt
// ═══════════════════════════════════════════════════════════════════════════

fn draw_shell_mode(frame: &mut Frame, area: Rect, state: &TuiState) {
    let composer_h = composer::composer_height(&state.composer);

    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),          // gap
            Constraint::Length(composer_h), // composer / input
            Constraint::Length(1),          // gap
            Constraint::Min(3),            // recent task + hints
        ])
        .split(area);

    // Composer
    composer::draw_composer(frame, body[1], &state.composer, state.is_working);

    // Recent task card + hints
    draw_shell_hints(frame, body[3], state);
}

fn draw_shell_hints(frame: &mut Frame, area: Rect, state: &TuiState) {
    let mut lines: Vec<Line> = Vec::new();

    // Recent task card — show the last completed task if any
    if let Some(banner) = &state.completion_status {
        lines.push(Line::from(vec![
            Span::styled(" Recent task", Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("   ", Style::default()),
            Span::styled(&banner.icon, Style::default().fg(banner.color)),
            Span::styled(format!("  {}", banner.message), Style::default().fg(Color::White)),
        ]));
        if !state.task_label.is_empty() {
            let label = truncate_str(&state.task_label, (area.width as usize).saturating_sub(6));
            lines.push(Line::from(vec![
                Span::styled("   ", Style::default()),
                Span::styled(label, Style::default().fg(Color::DarkGray)),
            ]));
        }
        lines.push(Line::from(""));
    } else if state.has_received_input && !state.task_label.is_empty() {
        // Show task in progress
        lines.push(Line::from(vec![
            Span::styled(" Active task", Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD)),
        ]));
        let label = truncate_str(&state.task_label, (area.width as usize).saturating_sub(6));
        lines.push(Line::from(vec![
            Span::styled("   ", Style::default()),
            Span::styled(label, Style::default().fg(Color::White)),
        ]));
        if !state.phase_label.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("   status: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&state.phase_label, Style::default().fg(Color::Cyan)),
            ]));
        }
        lines.push(Line::from(""));
    }

    // Hint lines
    lines.push(Line::from(vec![
        Span::styled(" Hints", Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("   /help       ", Style::default().fg(Color::Cyan)),
        Span::styled("commands and shortcuts", Style::default().fg(Color::DarkGray)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("   @file       ", Style::default().fg(Color::Green)),
        Span::styled("attach file as context", Style::default().fg(Color::DarkGray)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("   !cmd        ", Style::default().fg(Color::Magenta)),
        Span::styled("run shell command", Style::default().fg(Color::DarkGray)),
    ]));

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}

// ═══════════════════════════════════════════════════════════════════════════
//  Task mode — single-column focused task view
// ═══════════════════════════════════════════════════════════════════════════

fn draw_task_mode(frame: &mut Frame, area: Rect, state: &TuiState, wc: WidthClass) {
    let composer_h = composer::composer_height(&state.composer);
    let activity_h = if wc == WidthClass::Compact { 5 } else { 7 };
    let banner_h: u16 = if state.completion_status.is_some() { 1 } else { 0 };
    let status_h: u16 = 2; // dedicated status box above composer (border + content)

    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),            // task title (single row)
            Constraint::Length(activity_h),   // activity feed
            Constraint::Min(4),              // response stream
            Constraint::Length(banner_h),    // completion banner
            Constraint::Length(status_h),    // status bar (above input)
            Constraint::Length(composer_h),   // composer
        ])
        .split(area);

    draw_task_header(frame, body[0], state);
    draw_task_activity(frame, body[1], state);
    draw_content_pane(frame, body[2], state);

    // Completion banner
    if let Some(banner) = &state.completion_status {
        let line = Line::from(vec![
            Span::styled(
                format!(" {} ", banner.icon),
                Style::default().fg(Color::Black).bg(banner.color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {}", banner.message),
                Style::default().fg(banner.color).add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), body[3]);
    }

    // Status bar — dedicated row above the composer
    draw_status_bar(frame, body[4], state);

    composer::draw_composer(frame, body[5], &state.composer, state.is_working);
}

fn draw_task_header(frame: &mut Frame, area: Rect, state: &TuiState) {
    // Single-row header: just the task label
    let task_display = if state.task_label.is_empty() {
        "Working…".to_string()
    } else {
        truncate_str(&state.task_label, (area.width as usize).saturating_sub(20))
    };

    let line = Line::from(vec![
        Span::styled(" Task: ", Style::default().fg(Color::DarkGray)),
        Span::styled(task_display, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// Dedicated status bar rendered directly above the composer input.
fn draw_status_bar(frame: &mut Frame, area: Rect, state: &TuiState) {
    let status_text = if state.is_working {
        if state.is_thinking {
            "reasoning"
        } else if !state.phase_label.is_empty() {
            &state.phase_label
        } else {
            "working"
        }
    } else {
        "idle"
    };

    let mut spans = Vec::new();

    // Spinner
    if state.is_working {
        const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let idx = (state.spinner_frame / 4) as usize % SPINNER.len();
        spans.push(Span::styled(
            format!(" {} ", SPINNER[idx]),
            Style::default().fg(Color::Cyan),
        ));
    }

    spans.push(Span::styled(" Status: ", Style::default().fg(Color::DarkGray)));
    spans.push(Span::styled(status_text, Style::default().fg(Color::Cyan)));

    // Show active tool name if executing
    if let Some(ref tool_info) = state.active_tool {
        spans.push(Span::styled(
            format!("  ▸ {}", tool_info.tool_name),
            Style::default().fg(Color::Yellow),
        ));
    }

    if state.current_turn > 0 {
        spans.push(Span::styled(
            format!("  turn {}/{}", state.current_turn, state.max_turns),
            Style::default().fg(Color::DarkGray),
        ));
    }

    if let Some(since) = state.working_since {
        let elapsed = since.elapsed().as_secs();
        if elapsed > 0 {
            spans.push(Span::styled(
                format!("  {}s", elapsed),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    let line = Line::from(spans);
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));
    let paragraph = Paragraph::new(line).block(block);
    frame.render_widget(paragraph, area);
}

fn draw_task_activity(frame: &mut Frame, area: Rect, state: &TuiState) {
    let inner_height = area.height.saturating_sub(1) as usize;
    let total = state.activity_lines.len();
    let start = total.saturating_sub(inner_height);

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            " Activity",
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
        )),
    ];

    for entry in state.activity_lines[start..].iter() {
        let max_text = (area.width as usize).saturating_sub(6);
        if entry.icon.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("   {}", truncate_str(&entry.text, max_text)),
                Style::default().fg(entry.color),
            )));
        } else {
            lines.push(Line::from(vec![
                Span::styled(format!("   {} ", entry.icon), Style::default().fg(entry.color)),
                Span::raw(truncate_str(&entry.text, max_text)),
            ]));
        }
    }

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(Color::Rgb(40, 40, 50)));
    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn draw_content_pane(frame: &mut Frame, area: Rect, state: &TuiState) {
    let inner_height = area.height.saturating_sub(2) as usize;
    let pane_width = area.width.saturating_sub(2) as usize;

    // Collect all raw lines: committed + streaming
    let streaming_lines: Vec<&str> = if !state.streaming_text.is_empty() {
        state.streaming_text.lines().collect()
    } else {
        Vec::new()
    };

    // Live streaming indicator: blinking cursor at end of current output
    let is_streaming = !state.streaming_text.is_empty() && state.is_working;
    let cursor_char = if is_streaming {
        // Blink every ~500ms (spinner_frame increments at draw rate)
        if (state.spinner_frame / 8) % 2 == 0 { "▌" } else { " " }
    } else {
        ""
    };

    let mut all_lines: Vec<Line> = Vec::with_capacity(inner_height + 2);
    let mut in_code_block = false;
    let mut code_lang = String::new();
    let mut in_turn_cell = false;
    let mut turn_has_body = false;
    let mut turn_start_index: Option<usize> = None;
    let mut prev_was_empty = false;

    // Helper: push with optional turn gutter
    macro_rules! emit {
        ($line:expr) => {{
            let line: Line<'static> = $line;
            if in_turn_cell && !in_code_block {
                let mut bordered = vec![Span::styled(" │ ", Style::default().fg(Color::DarkGray))];
                bordered.extend(line.spans);
                all_lines.push(Line::from(bordered));
            } else {
                all_lines.push(line);
            }
        }};
    }

    for raw in state.content_lines.iter().map(String::as_str).chain(streaming_lines.iter().copied()) {
        let trimmed = raw.trim();

        // ── Code fence ──
        if trimmed.starts_with("```") {
            if in_code_block {
                in_code_block = false;
                turn_has_body |= in_turn_cell;
                emit!(Line::from(Span::styled(
                    format!("  └{}", "─".repeat(pane_width.saturating_sub(5).min(30))),
                    Style::default().fg(Color::DarkGray),
                )));
                code_lang.clear();
                prev_was_empty = false;
            } else {
                in_code_block = true;
                turn_has_body |= in_turn_cell;
                code_lang = trimmed.trim_start_matches('`').to_string();
                if !prev_was_empty {
                    emit!(Line::from(""));
                }
                let label = if code_lang.is_empty() { " code ".to_string() } else { format!(" {} ", code_lang) };
                emit!(Line::from(vec![
                    Span::styled("  ┌", Style::default().fg(Color::DarkGray)),
                    Span::styled(label, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::styled("─".repeat(pane_width.saturating_sub(6 + code_lang.len()).min(16)), Style::default().fg(Color::DarkGray)),
                ]));
                prev_was_empty = false;
            }
            continue;
        }

        // ── Inside code block ──
        if in_code_block {
            turn_has_body |= in_turn_cell;
            let mut spans = vec![Span::styled("  │ ", Style::default().fg(Color::DarkGray))];
            let highlighted = highlight_code_line(raw, &code_lang);
            if highlighted.is_empty() {
                spans.push(Span::styled(raw.to_string(), Style::default().fg(Color::Green)));
            } else {
                spans.extend(highlighted);
            }
            if in_turn_cell {
                let mut bordered = vec![Span::styled(" │ ", Style::default().fg(Color::DarkGray))];
                bordered.extend(spans);
                all_lines.push(Line::from(bordered));
            } else {
                all_lines.push(Line::from(spans));
            }
            prev_was_empty = false;
            continue;
        }

        // ── Turn separator ──
        if trimmed.starts_with("══ Turn ") && trimmed.ends_with(" ══") {
            if in_turn_cell {
                if turn_has_body {
                    all_lines.push(Line::from(""));
                } else if let Some(start_idx) = turn_start_index.take() {
                    all_lines.truncate(start_idx);
                }
            }
            let turn_label = trimmed.trim_start_matches("══ ").trim_end_matches(" ══");
            turn_start_index = Some(all_lines.len());
            all_lines.push(Line::from(""));
            all_lines.push(Line::from(vec![
                Span::styled(" ╭─ ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    turn_label.to_string(),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {}", "─".repeat(pane_width.saturating_sub(turn_label.len() + 6).min(40))),
                    Style::default().fg(Color::Rgb(50, 50, 60)),
                ),
            ]));
            in_turn_cell = true;
            turn_has_body = false;
            prev_was_empty = false;
            continue;
        }

        // ── Legacy separator ──
        if trimmed.starts_with("───") || trimmed.starts_with("═══") {
            emit!(Line::from(Span::styled(format!(" {}", trimmed), Style::default().fg(Color::DarkGray))));
            prev_was_empty = false;
            continue;
        }

        if in_turn_cell && !trimmed.is_empty() {
            turn_has_body = true;
        }

        // ── Markdown headers ──
        if trimmed.starts_with("### ") {
            let heading = trimmed.trim_start_matches("### ");
            if !prev_was_empty { emit!(Line::from("")); }
            emit!(Line::from(vec![
                Span::styled("   ", Style::default()),
                Span::styled(heading.to_string(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            ]));
            emit!(Line::from(""));
            prev_was_empty = true;
            continue;
        }
        if trimmed.starts_with("## ") {
            let heading = trimmed.trim_start_matches("## ");
            if !prev_was_empty { emit!(Line::from("")); }
            emit!(Line::from(vec![
                Span::styled(" ◆ ", Style::default().fg(Color::Yellow)),
                Span::styled(heading.to_string(), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            ]));
            emit!(Line::from(""));
            prev_was_empty = true;
            continue;
        }
        if trimmed.starts_with("# ") {
            let heading = trimmed.trim_start_matches("# ");
            if !prev_was_empty { emit!(Line::from("")); }
            emit!(Line::from(vec![
                Span::styled(format!(" ━━ {} ", heading), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled("━".repeat(pane_width.saturating_sub(heading.len() + 6).min(20)), Style::default().fg(Color::DarkGray)),
            ]));
            emit!(Line::from(""));
            prev_was_empty = true;
            continue;
        }

        // ── Bullet points ──
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            let text = &trimmed[2..];
            let mut spans = vec![Span::styled("   • ", Style::default().fg(Color::Cyan))];
            spans.extend(parse_inline_spans(text));
            emit!(Line::from(spans));
            prev_was_empty = false;
            continue;
        }

        // ── Numbered lists ──
        if trimmed.len() > 2 && trimmed.as_bytes()[0].is_ascii_digit() {
            if let Some(rest) = trimmed.strip_prefix(|c: char| c.is_ascii_digit())
                .and_then(|s| s.strip_prefix(". "))
            {
                let num_str: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
                let mut spans = vec![Span::styled(format!("   {}. ", num_str), Style::default().fg(Color::Cyan))];
                spans.extend(parse_inline_spans(rest));
                emit!(Line::from(spans));
                prev_was_empty = false;
                continue;
            }
        }

        // ── Blockquotes ──
        if trimmed.starts_with("> ") {
            let text = &trimmed[2..];
            let mut spans = vec![Span::styled("  ▎ ", Style::default().fg(Color::Blue))];
            for s in parse_inline_spans(text) {
                spans.push(Span::styled(s.content.to_string(), s.style.add_modifier(Modifier::ITALIC)));
            }
            emit!(Line::from(spans));
            prev_was_empty = false;
            continue;
        }

        // ── Horizontal rule ──
        if (trimmed == "---" || trimmed == "***" || trimmed == "___")
            || (trimmed.len() >= 3 && trimmed.chars().all(|c| c == '-' || c == ' '))
        {
            emit!(Line::from(Span::styled(
                format!("  {}", "─".repeat(pane_width.saturating_sub(4).min(50))),
                Style::default().fg(Color::Rgb(50, 50, 60)),
            )));
            prev_was_empty = false;
            continue;
        }

        // ── Table rows ──
        if trimmed.starts_with('|') && trimmed.ends_with('|') {
            if trimmed.chars().all(|c| c == '|' || c == '-' || c == ':' || c == ' ') {
                emit!(Line::from(Span::styled(
                    format!("   {}", "─".repeat(pane_width.saturating_sub(6).min(50))),
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                let cells: Vec<&str> = trimmed.split('|').filter(|s| !s.is_empty()).map(|s| s.trim()).collect();
                let mut spans = vec![Span::styled("   ", Style::default())];
                for (i, cell) in cells.iter().enumerate() {
                    if i > 0 { spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray))); }
                    spans.extend(parse_inline_spans(cell));
                }
                emit!(Line::from(spans));
            }
            prev_was_empty = false;
            continue;
        }

        // ── Empty line ──
        if trimmed.is_empty() {
            if in_turn_cell && !turn_has_body { continue; }
            if !prev_was_empty {
                emit!(Line::from(""));
                prev_was_empty = true;
            }
            continue;
        }

        // ── Default paragraph text ──
        turn_has_body |= in_turn_cell;
        prev_was_empty = false;
        emit!(style_paragraph_line(raw));
    }

    // Close last turn section
    if in_turn_cell && turn_has_body {
        all_lines.push(Line::from(""));
    } else if in_turn_cell {
        if let Some(start_idx) = turn_start_index {
            all_lines.truncate(start_idx);
        }
    }

    // Thinking indicator
    if all_lines.is_empty() && (state.is_thinking || (state.is_working && state.streaming_text.is_empty())) {
        const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let spin_frame = (state.spinner_frame / 4) as usize % SPINNER.len();
        let label = if state.is_thinking { "reasoning" } else { "thinking" };
        let spinner_color = if state.is_thinking { Color::Magenta } else { Color::Cyan };
        all_lines.push(Line::from(vec![
            Span::styled(format!(" {} ", SPINNER[spin_frame]), Style::default().fg(spinner_color)),
            Span::styled(label, Style::default().fg(Color::DarkGray)),
        ]));
    }

    let total = all_lines.len();
    let max_scroll = total.saturating_sub(inner_height);
    let scroll_from_bottom = usize::from(state.content_scroll_offset).min(max_scroll);
    let top_scroll = max_scroll.saturating_sub(scroll_from_bottom);
    let end = total.saturating_sub(scroll_from_bottom);
    let scroll_info = if total > inner_height {
        format!(" {}/{} ", end, total)
    } else {
        String::new()
    };

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::Rgb(50, 50, 60)))
        .title(Span::styled(
            " response ",
            Style::default().fg(Color::DarkGray),
        ))
        .title_bottom(Span::styled(
            scroll_info,
            Style::default().fg(Color::DarkGray),
        ));

    // Append live streaming cursor at the end of content
    if !cursor_char.is_empty() {
        if let Some(last_line) = all_lines.last_mut() {
            last_line.spans.push(Span::styled(
                cursor_char.to_string(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ));
        } else {
            all_lines.push(Line::from(Span::styled(
                format!("   {}", cursor_char),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )));
        }
    }

    let paragraph = Paragraph::new(all_lines)
        .wrap(ratatui::widgets::Wrap { trim: false })
        .scroll((top_scroll.min(u16::MAX as usize) as u16, 0))
        .block(block);
    frame.render_widget(paragraph, area);
}

/// Style a paragraph line with inline markdown: `code`, **bold**, *italic*.
fn style_paragraph_line(raw: &str) -> Line<'static> {
    let spans = parse_inline_spans(raw);
    if spans.len() == 1 {
        Line::from(Span::styled(format!("   {}", raw), Style::default().fg(Color::White)))
    } else {
        let mut result = vec![Span::raw("   ")];
        result.extend(spans);
        Line::from(result)
    }
}

/// Parse inline markdown into styled spans: `code`, **bold**, *italic*.
fn parse_inline_spans(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut chars = text.char_indices().peekable();
    let mut buf = String::new();

    while let Some(&(_i, ch)) = chars.peek() {
        match ch {
            '`' => {
                // Flush buffer
                if !buf.is_empty() {
                    spans.push(Span::styled(buf.clone(), Style::default().fg(Color::White)));
                    buf.clear();
                }
                chars.next(); // consume `
                let mut code = String::new();
                while let Some(&(_, c)) = chars.peek() {
                    if c == '`' { chars.next(); break; }
                    code.push(c);
                    chars.next();
                }
                spans.push(Span::styled(
                    code,
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ));
            }
            '*' => {
                // Check for ** (bold) vs * (italic)
                chars.next();
                if chars.peek().map(|&(_, c)| c) == Some('*') {
                    // **bold**
                    chars.next();
                    if !buf.is_empty() {
                        spans.push(Span::styled(buf.clone(), Style::default().fg(Color::White)));
                        buf.clear();
                    }
                    let mut bold = String::new();
                    while let Some(&(_, c)) = chars.peek() {
                        if c == '*' {
                            chars.next();
                            if chars.peek().map(|&(_, c)| c) == Some('*') {
                                chars.next();
                                break;
                            }
                            bold.push('*');
                            continue;
                        }
                        bold.push(c);
                        chars.next();
                    }
                    spans.push(Span::styled(
                        bold,
                        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                    ));
                } else {
                    // *italic*
                    if !buf.is_empty() {
                        spans.push(Span::styled(buf.clone(), Style::default().fg(Color::White)));
                        buf.clear();
                    }
                    let mut italic = String::new();
                    while let Some(&(_, c)) = chars.peek() {
                        if c == '*' { chars.next(); break; }
                        italic.push(c);
                        chars.next();
                    }
                    spans.push(Span::styled(
                        italic,
                        Style::default().fg(Color::White).add_modifier(Modifier::ITALIC),
                    ));
                }
            }
            _ => {
                buf.push(ch);
                chars.next();
            }
        }
    }

    if !buf.is_empty() {
        spans.push(Span::styled(buf, Style::default().fg(Color::White)));
    }

    spans
}

/// Render inline markdown (simple version for list items).
fn style_inline_markdown(text: &str) -> String {
    // For list items, just return the text — the full span parsing
    // is done at the Line level in style_paragraph_line.
    text.to_string()
}

/// Handle a key event, updating state. Returns true if the event was consumed.
pub fn handle_key(state: &mut TuiState, key: KeyEvent) -> bool {
    if key.kind == KeyEventKind::Release {
        return false;
    }

    // Ctrl-C / Ctrl-D: quit (always handled at top level)
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.should_quit = true;
            return true;
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.should_quit = true;
            return true;
        }
        _ => {}
    }

    // Content pane scrolling: Alt-Up/Down
    match key.code {
        // Mode switching: 'g' goes to shell (only when composer is empty)
        KeyCode::Char('g') if state.ui_mode == UiMode::Task && state.composer.is_empty() && !key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.ui_mode = UiMode::Shell;
            return true;
        }
        KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
            let max = (state.content_lines.len() + state.streaming_text.lines().count()) as u16;
            state.content_scroll_offset = (state.content_scroll_offset + 1).min(max);
            return true;
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
            state.content_scroll_offset = state.content_scroll_offset.saturating_sub(1);
            return true;
        }
        KeyCode::PageUp if key.modifiers.contains(KeyModifiers::ALT) => {
            let max = (state.content_lines.len() + state.streaming_text.lines().count()) as u16;
            state.content_scroll_offset = (state.content_scroll_offset + 10).min(max);
            return true;
        }
        KeyCode::PageDown if key.modifiers.contains(KeyModifiers::ALT) => {
            state.content_scroll_offset = state.content_scroll_offset.saturating_sub(10);
            return true;
        }
        _ => {}
    }

    // Delegate to the composer
    state.composer.handle_key(key)
}

/// Handle a mouse event, updating state. Returns true if the event was consumed.
/// Region-aware: scrolling in the timeline pane scrolls timeline,
/// scrolling in the content pane scrolls content.
pub fn handle_mouse(state: &mut TuiState, mouse: MouseEvent, _terminal_width: u16) -> bool {
    match mouse.kind {
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
            // Scroll the content pane
            if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                let max = (state.content_lines.len() + state.streaming_text.lines().count()) as u16;
                state.content_scroll_offset = (state.content_scroll_offset + 3).min(max);
            } else {
                state.content_scroll_offset = state.content_scroll_offset.saturating_sub(3);
            }
            true
        }
        _ => false,
    }
}

/// Handle a terminal resize event.
/// Invalidates cached content, clamps scroll offsets, and forces a full redraw.
pub fn handle_resize(state: &mut TuiState, _cols: u16, _rows: u16) {
    // Invalidate parsed line cache — wrapping depends on width
    state.cached_parsed_lines.clear();
    state.cached_lines_count = 0;

    // Clamp content scroll offset to new content bounds
    let max = state.content_lines.len() as u16;
    state.content_scroll_offset = state.content_scroll_offset.min(max);

    // Clamp activity scroll offset
    let activity_max = state.activity_lines.len() as u16;
    state.scroll_offset = state.scroll_offset.min(activity_max);

    // Force immediate redraw on next frame
    state.last_frame_time = None;
}

fn truncate_str(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let width = UnicodeWidthStr::width(s);
    if width <= max {
        return s.to_string();
    }
    // For path-like strings, use middle-elision to preserve the filename
    if s.contains('/') {
        return middle_elide_path(s, max);
    }
    let mut current_width = 0;
    let truncated: String = s.chars().take_while(|c| {
        current_width += unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
        current_width <= max.saturating_sub(1)
    }).collect();
    format!("{}…", truncated)
}

/// Middle-elide a file path: keep the first component + … + last component(s).
/// Example: `client/src/pages/admin/AdminPromotions.jsx` → `client/…/AdminPromotions.jsx`
fn middle_elide_path(path: &str, max: usize) -> String {
    if max <= 3 {
        return "…".to_string();
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 2 {
        // Too few segments to middle-elide — just end-truncate
        let mut current_width = 0;
        let truncated: String = path.chars().take_while(|c| {
            current_width += unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
            current_width <= max.saturating_sub(1)
        }).collect();
        return format!("{}…", truncated);
    }

    let last = parts[parts.len() - 1];
    // Try to fit: first/…/last
    for i in 0..parts.len() - 1 {
        let prefix: String = parts[..=i].join("/");
        let candidate = format!("{}/…/{}", prefix, last);
        if candidate.len() <= max {
            return candidate;
        }
    }
    // Can't even fit first/…/last — just show …/last
    let candidate = format!("…/{}", last);
    if candidate.len() <= max {
        return candidate;
    }
    // Last resort: truncate the filename itself
    let mut current_width = 0;
    let truncated: String = last.chars().take_while(|c| {
        current_width += unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
        current_width <= max.saturating_sub(1)
    }).collect();
    format!("{}…", truncated)
}

/// Highlight a single line of code using syntect.
/// Returns an empty vec if the language is unknown.
fn highlight_code_line<'a>(line: &str, lang: &str) -> Vec<Span<'a>> {
    use syntect::easy::HighlightLines;
    use syntect::highlighting::FontStyle;

    let ss = syntax_set();
    let syntax = ss.find_syntax_by_token(lang)
        .or_else(|| ss.find_syntax_by_extension(lang))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let theme = highlight_theme();

    let mut h = HighlightLines::new(syntax, theme);
    let regions = match h.highlight_line(line, ss) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    regions.into_iter().map(|(style, text)| {
        let fg = Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b);
        let mut ratatui_style = Style::default().fg(fg);
        if style.font_style.contains(FontStyle::BOLD) {
            ratatui_style = ratatui_style.add_modifier(Modifier::BOLD);
        }
        if style.font_style.contains(FontStyle::ITALIC) {
            ratatui_style = ratatui_style.add_modifier(Modifier::ITALIC);
        }
        Span::styled(text.to_string(), ratatui_style)
    }).collect()
}
