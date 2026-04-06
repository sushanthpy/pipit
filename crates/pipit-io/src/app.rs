//! Full-screen ratatui TUI for pipit-cli.
//!
//! Fullscreen rendering mode — directly addresses visual lag and memory
//! bloat during long agentic sessions by:
//!
//!   1. Bounded ring buffers for content and activity (capped, auto-evict)
//!   2. Virtual viewport — only visible lines are parsed/rendered per frame
//!   3. Cached parsed lines — markdown re-parsing only when content changes
//!   4. Frame-budget rendering — skip frames if behind schedule
//!   5. Streaming text compaction — gc on turn boundaries
//!
//! Layout:
//!   ┌─ Status bar ──────────────────────────────────────┐
//!   │ pipit · repo · branch · model · mode · tokens     │
//!   ├─ Phase ───────────────────────────────────────────┤
//!   │ executing                          phase: 3/10    │
//!   ├─ Timeline ────────┬─ Response ───────────────────-┤
//!   │ ◆ diagnostic plan │ The codebase is a Rust CLI... │
//!   │ ○ Read src/main   │                               │
//!   │ ● Edit lib.rs     │ ## Architecture               │
//!   │ · turn 1 done     │ - pipit-core: agent loop      │
//!   ├─ Composer ─────────────────────────────────────────┤
//!   │ you› _                                             │
//!   │ Tab commands · @file · !shell · Ctrl-J multiline   │
//!   └───────────────────────────────────────────────────-┘

use crate::composer::{self, Composer};
use crate::tui::StatusBarState;
use crossterm::{
    event::{EnableBracketedPaste, DisableBracketedPaste, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
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
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

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
    /// Timeline entries (left pane): compact agent actions.
    pub activity_lines: Vec<ActivityLine>,
    /// Content lines (right pane): natural-language responses.
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
}

impl TuiState {
    pub fn new(status: StatusBarState, project_root: PathBuf) -> Self {
        Self {
            status,
            activity_lines: Vec::with_capacity(MAX_ACTIVITY_LINES),
            content_lines: Vec::with_capacity(256),
            composer: Composer::new(project_root),
            scroll_offset: 0,
            content_scroll_offset: 0,
            should_quit: false,
            streaming_text: String::with_capacity(4096),
            is_working: false,
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
        }
    }

    /// Start a working state (agent is processing).
    pub fn begin_working(&mut self, label: &str) {
        // Don't restart spinner from stale queued events after a run completes
        if self.run_finished {
            return;
        }
        self.is_working = true;
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
                .map(|l| l.to_string())
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
        let total = self.activity_lines.len() as u16;
        if total > 10 {
            self.scroll_offset = total.saturating_sub(10);
        }
    }

    pub fn auto_scroll_content(&mut self) {
        let total = self.content_lines.len() as u16;
        if total > 10 {
            self.content_scroll_offset = total.saturating_sub(10);
        }
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
            DisableBracketedPaste,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
        original_panic(info);
    }));

    enable_raw_mode()?;
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen, EnableBracketedPaste)?;
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
    execute!(terminal.backend_mut(), DisableBracketedPaste, LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    // Restore the default panic hook so post-TUI panics behave normally.
    let _ = std::panic::take_hook();
    Ok(())
}

/// Draw the full TUI frame.
pub fn draw(frame: &mut Frame, state: &TuiState) {
    let area = frame.area();

    let composer_h = composer::composer_height(&state.composer);

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),         // status bar
            Constraint::Length(1),         // task / phase strip
            Constraint::Min(5),            // main pane (timeline | content)
            Constraint::Length(composer_h), // dynamic composer height
        ])
        .split(area);

    draw_status_bar(frame, vertical[0], state);
    draw_task_phase_strip(frame, vertical[1], state);

    if state.has_received_input {
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

    // Draw the composer (replaces draw_input_bar)
    composer::draw_composer(frame, vertical[3], &state.composer, state.is_working);

    // Draw completion popup as overlay (must come LAST so it renders on top)
    composer::draw_completion_popup(frame, vertical[3], &state.composer);
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
    if state.task_label.is_empty() && state.phase_label.is_empty() && state.current_turn == 0 {
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray));
        frame.render_widget(block, area);
        return;
    }

    // Split area: left for task info, right for progress
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(30), Constraint::Length(20)])
        .split(area);

    // Left: prompt + phase labels
    let task_display = if state.task_label.len() > 50 {
        format!("{}…", &state.task_label.chars().take(48).collect::<String>())
    } else {
        state.task_label.clone()
    };

    let line = Line::from(vec![
        Span::styled(" ", Style::default().fg(Color::DarkGray)),
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
    frame.render_widget(paragraph, chunks[0]);

    // Right: turn progress gauge
    if state.current_turn > 0 && state.max_turns > 0 {
        let ratio = (state.current_turn as f64 / state.max_turns as f64).min(1.0);
        let pct = (ratio * 100.0) as u16;
        let gauge_label = format!("{}/{}", state.current_turn, state.max_turns);

        // Simple text-based progress bar (ratatui Gauge needs a different layout)
        let bar_width = chunks[1].width.saturating_sub(4) as usize;
        let filled = (ratio * bar_width as f64) as usize;
        let empty = bar_width.saturating_sub(filled);
        let bar = format!(" {}{}  {}",
            "█".repeat(filled),
            "░".repeat(empty),
            gauge_label,
        );

        let bar_color = if pct > 80 { Color::Yellow } else { Color::Cyan };
        let bar_line = Line::from(Span::styled(bar, Style::default().fg(bar_color)));

        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray));
        let p = Paragraph::new(bar_line).block(block);
        frame.render_widget(p, chunks[1]);
    } else {
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray));
        frame.render_widget(block, chunks[1]);
    }
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
        const SPINNER: &[&str] = &["\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}", "\u{2827}", "\u{2807}", "\u{280f}"];
        let frame = (state.spinner_frame / 4) as usize % SPINNER.len();
        let spinner_char = SPINNER[frame];

        let elapsed = state.working_since
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        let elapsed_str = if elapsed > 0 {
            format!(" {}s", elapsed)
        } else {
            String::new()
        };

        display.push(Line::from(vec![
            Span::styled(
                format!(" {} ", spinner_char),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(
                state.working_label.clone(),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(
                elapsed_str,
                Style::default().fg(Color::DarkGray),
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
    let pane_width = area.width.saturating_sub(2) as usize;

    // ── Virtual viewport: only process lines that will be visible ──
    // Instead of parsing ALL lines then slicing, we compute the visible
    // window first and only parse those lines. This is O(visible) instead
    // of O(total), critical for long sessions.

    // Collect all raw lines: committed + streaming
    let committed_count = state.content_lines.len();
    let streaming_lines: Vec<&str> = if !state.streaming_text.is_empty() {
        state.streaming_text.lines().collect()
    } else {
        Vec::new()
    };
    let total_raw = committed_count + streaming_lines.len();

    // Compute visible window (last inner_height lines, auto-scroll)
    let start = if total_raw > inner_height {
        total_raw - inner_height
    } else {
        0
    };
    let end = total_raw;

    // Build lines ONLY for the visible range
    let mut all_lines: Vec<Line> = Vec::with_capacity(inner_height + 2);
    let mut in_code_block = false;
    let mut code_lang = String::new();

    // We need to track code-block state from before the visible window.
    // Scan prior lines for fence states (cheaper than full parsing).
    for i in 0..start {
        let raw = if i < committed_count {
            state.content_lines[i].as_str()
        } else {
            streaming_lines[i - committed_count]
        };
        let trimmed = raw.trim();
        if trimmed.starts_with("```") {
            if in_code_block {
                in_code_block = false;
                code_lang.clear();
            } else {
                in_code_block = true;
                code_lang = trimmed.trim_start_matches('`').to_string();
            }
        }
    }

    // Now render only the visible lines
    for i in start..end {
        let raw = if i < committed_count {
            state.content_lines[i].as_str()
        } else {
            streaming_lines[i - committed_count]
        };
        let trimmed = raw.trim();

        // ── Code fence toggle ──
        if trimmed.starts_with("```") {
            if in_code_block {
                in_code_block = false;
                all_lines.push(Line::from(Span::styled(
                    format!(" {}", "─".repeat(pane_width.saturating_sub(2).min(40))),
                    Style::default().fg(Color::DarkGray),
                )));
                code_lang.clear();
                continue;
            } else {
                in_code_block = true;
                code_lang = trimmed.trim_start_matches('`').to_string();
                let label = if code_lang.is_empty() {
                    " code ".to_string()
                } else {
                    format!(" {} ", code_lang)
                };
                all_lines.push(Line::from(vec![
                    Span::styled(
                        format!(" ┌{}", "─".repeat(label.len())),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(label, Style::default().fg(Color::Cyan)),
                    Span::styled(
                        "─".repeat(pane_width.saturating_sub(4 + code_lang.len()).min(30)),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                continue;
            }
        }

        // ── Inside code block ──
        if in_code_block {
            let mut spans = vec![Span::styled(" │ ", Style::default().fg(Color::DarkGray))];
            let highlighted = highlight_code_line(raw, &code_lang);
            if highlighted.is_empty() {
                spans.push(Span::styled(raw.to_string(), Style::default().fg(Color::Green)));
            } else {
                spans.extend(highlighted);
            }
            all_lines.push(Line::from(spans));
            continue;
        }

        // ── Turn separator ──
        if trimmed.starts_with("───") || trimmed.starts_with("═══") {
            all_lines.push(Line::from(Span::styled(
                format!(" {}", trimmed),
                Style::default().fg(Color::DarkGray),
            )));
            continue;
        }

        // ── Markdown headers ──
        if trimmed.starts_with("### ") {
            let heading = trimmed.trim_start_matches("### ");
            all_lines.push(Line::from(vec![
                Span::styled(" ", Style::default()),
                Span::styled(
                    format!("  {}", heading),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
            ]));
            continue;
        }
        if trimmed.starts_with("## ") {
            let heading = trimmed.trim_start_matches("## ");
            all_lines.push(Line::from(Span::styled(
                format!(" ◆ {}", heading),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        if trimmed.starts_with("# ") {
            let heading = trimmed.trim_start_matches("# ");
            all_lines.push(Line::from(Span::styled(
                format!(" ━ {} ━", heading),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )));
            continue;
        }

        // ── Bullet points ──
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            let text = &trimmed[2..];
            all_lines.push(Line::from(vec![
                Span::styled("  • ", Style::default().fg(Color::Cyan)),
                Span::raw(style_inline_markdown(text)),
            ]));
            continue;
        }

        // ── Numbered lists ──
        if trimmed.len() > 2 && trimmed.as_bytes()[0].is_ascii_digit() {
            if let Some(rest) = trimmed.strip_prefix(|c: char| c.is_ascii_digit())
                .and_then(|s| s.strip_prefix(". "))
            {
                let num_str: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
                all_lines.push(Line::from(vec![
                    Span::styled(format!("  {}. ", num_str), Style::default().fg(Color::Cyan)),
                    Span::raw(style_inline_markdown(rest)),
                ]));
                continue;
            }
        }

        // ── Blockquotes ──
        if trimmed.starts_with("> ") {
            let text = &trimmed[2..];
            all_lines.push(Line::from(vec![
                Span::styled(" ▎ ", Style::default().fg(Color::Blue)),
                Span::styled(
                    text.to_string(),
                    Style::default().fg(Color::White).add_modifier(Modifier::ITALIC),
                ),
            ]));
            continue;
        }

        // ── Empty line ──
        if trimmed.is_empty() {
            all_lines.push(Line::from(""));
            continue;
        }

        // ── Default: inline markdown (bold, code spans, etc.) ──
        all_lines.push(style_paragraph_line(raw));
    }

    // Show progress indicator when agent is working or thinking
    if state.is_thinking || (state.is_working && state.streaming_text.is_empty()) {
        const SPINNER: &[&str] = &["\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}", "\u{2827}", "\u{2807}", "\u{280f}"];
        let spin_frame = (state.spinner_frame / 4) as usize % SPINNER.len();
        let elapsed = state.working_since
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        let elapsed_str = if elapsed > 0 {
            format!(" {}s", elapsed)
        } else {
            String::new()
        };
        let label = if state.is_thinking {
            "reasoning".to_string()
        } else if state.working_label.is_empty() {
            "thinking".to_string()
        } else {
            state.working_label.clone()
        };
        let spinner_color = if state.is_thinking { Color::Magenta } else { Color::Cyan };

        // Animated dots for thinking: ·· ··· ···· cycling
        let dots = if state.is_thinking {
            let n = ((state.spinner_frame / 8) % 4) as usize + 1;
            format!(" {}", "·".repeat(n))
        } else {
            String::new()
        };

        all_lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", SPINNER[spin_frame]),
                Style::default().fg(spinner_color),
            ),
            Span::styled(
                label,
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                dots,
                Style::default().fg(spinner_color),
            ),
            Span::styled(
                elapsed_str,
                Style::default().fg(Color::DarkGray),
            ),
        ]));
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

/// Style a paragraph line with inline markdown: `code`, **bold**, *italic*.
fn style_paragraph_line(raw: &str) -> Line<'static> {
    let spans = parse_inline_spans(raw);
    if spans.len() == 1 {
        // Fast path — no inline formatting
        Line::from(Span::styled(format!(" {}", raw), Style::default().fg(Color::White)))
    } else {
        let mut result = vec![Span::raw(" ")];
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
        KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
            state.content_scroll_offset = state.content_scroll_offset.saturating_sub(1);
            return true;
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
            let max = state.content_lines.len() as u16;
            state.content_scroll_offset = (state.content_scroll_offset + 1).min(max);
            return true;
        }
        KeyCode::PageUp if key.modifiers.contains(KeyModifiers::ALT) => {
            state.content_scroll_offset = state.content_scroll_offset.saturating_sub(10);
            return true;
        }
        KeyCode::PageDown if key.modifiers.contains(KeyModifiers::ALT) => {
            let max = state.content_lines.len() as u16;
            state.content_scroll_offset = (state.content_scroll_offset + 10).min(max);
            return true;
        }
        _ => {}
    }

    // Delegate to the composer
    state.composer.handle_key(key)
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
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
