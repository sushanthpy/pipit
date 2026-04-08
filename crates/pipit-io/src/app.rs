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
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Tabs,
    },
};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

/// Check if colors should be suppressed (NO_COLOR standard, TERM=dumb).
/// See https://no-color.org/
pub fn no_color() -> bool {
    static NO_COLOR: OnceLock<bool> = OnceLock::new();
    *NO_COLOR.get_or_init(|| {
        std::env::var("NO_COLOR").is_ok() || std::env::var("TERM").as_deref() == Ok("dumb")
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
        ts.themes
            .get("base16-ocean.dark")
            .cloned()
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

/// Top-level tab — selects the main content view.
/// Ctrl+1/2/3/4 or F2/F3/F4/F5 switches tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabView {
    /// Default coding view — shell + task modes.
    Coding,
    /// Agents & subagents — shows registered agents, running subagents, delegation status.
    Agents,
    /// Context & memory — file context, token budget, memory entries, knowledge.
    Context,
    /// Help & docs — keyboard shortcuts, slash commands, pipit usage guide.
    Help,
}

impl TabView {
    pub fn index(self) -> usize {
        match self {
            TabView::Coding => 0,
            TabView::Agents => 1,
            TabView::Context => 2,
            TabView::Help => 3,
        }
    }

    pub fn from_index(i: usize) -> Self {
        match i {
            0 => TabView::Coding,
            1 => TabView::Agents,
            2 => TabView::Context,
            3 => TabView::Help,
            _ => TabView::Coding,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            TabView::Coding => "Coding",
            TabView::Agents => "Agents",
            TabView::Context => "Context",
            TabView::Help => "Help",
        }
    }

    pub const ALL: [TabView; 4] = [
        TabView::Coding,
        TabView::Agents,
        TabView::Context,
        TabView::Help,
    ];
}

/// Overlay — temporary modal/drawer on top of the current mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Help,
    Search,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneFocus {
    Input,
    Activity,
    Response,
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
    /// Active top-level tab.
    pub active_tab: TabView,
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
    /// Which scrollable pane is focused for keyboard navigation in task mode.
    pub focused_pane: PaneFocus,
    /// Active pane-local search query.
    pub search_query: String,
    /// Which pane the current search applies to.
    pub search_target: PaneFocus,
    /// Selected match index within the current search results.
    pub search_match_index: usize,
    /// Content length at the start of the current turn.
    pub current_turn_content_start: usize,
    /// Whether the current turn has executed at least one tool call.
    pub current_turn_had_tool_calls: bool,
    /// Insert the markdown separator lazily so tool-only turns do not leave empty rules behind.
    pending_turn_separator: bool,
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
            active_tab: TabView::Coding,
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
            focused_pane: PaneFocus::Input,
            search_query: String::new(),
            search_target: PaneFocus::Response,
            search_match_index: 0,
            current_turn_content_start: 0,
            current_turn_had_tool_calls: false,
            pending_turn_separator: false,
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
        self.completion_status = None; // Clear previous completion banner
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
            let new_lines: Vec<String> = self
                .streaming_text
                .lines()
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
        if !text.is_empty() {
            self.ensure_turn_separator();
        }
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
        self.focused_pane = PaneFocus::Input;
        self.search_query.clear();
        self.search_match_index = 0;
        self.current_turn_content_start = 0;
        self.current_turn_had_tool_calls = false;
        self.pending_turn_separator = false;
    }

    fn auto_scroll_timeline(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn auto_scroll_content(&mut self) {
        self.content_scroll_offset = 0;
    }

    pub fn cycle_focus(&mut self, forward: bool) {
        self.focused_pane = match (self.focused_pane, forward) {
            (PaneFocus::Input, true) => PaneFocus::Response,
            (PaneFocus::Response, true) => PaneFocus::Activity,
            (PaneFocus::Activity, true) => PaneFocus::Input,
            (PaneFocus::Input, false) => PaneFocus::Activity,
            (PaneFocus::Activity, false) => PaneFocus::Response,
            (PaneFocus::Response, false) => PaneFocus::Input,
        };
    }

    pub fn cycle_tab(&mut self, forward: bool) {
        let len = TabView::ALL.len();
        let current = self.active_tab.index();
        let next = if forward {
            (current + 1) % len
        } else if current == 0 {
            len - 1
        } else {
            current - 1
        };
        self.active_tab = TabView::from_index(next);
    }

    pub fn scroll_activity_by(&mut self, delta: i16) {
        if delta >= 0 {
            self.scroll_offset =
                (self.scroll_offset + delta as u16).min(self.activity_lines.len() as u16);
        } else {
            self.scroll_offset = self.scroll_offset.saturating_sub(delta.unsigned_abs());
        }
    }

    pub fn scroll_content_by(&mut self, delta: i16) {
        let max = (self.content_lines.len() + self.streaming_text.lines().count()) as u16;
        if delta >= 0 {
            self.content_scroll_offset = (self.content_scroll_offset + delta as u16).min(max);
        } else {
            self.content_scroll_offset = self
                .content_scroll_offset
                .saturating_sub(delta.unsigned_abs());
        }
    }

    pub fn jump_activity_to_oldest(&mut self) {
        self.scroll_offset = self.activity_lines.len() as u16;
    }

    pub fn jump_content_to_oldest(&mut self) {
        self.content_scroll_offset =
            (self.content_lines.len() + self.streaming_text.lines().count()) as u16;
    }

    pub fn begin_search(&mut self, target: PaneFocus) {
        self.search_target = target;
        self.search_query.clear();
        self.search_match_index = 0;
        self.overlay = Overlay::Search;
    }

    pub fn clear_search(&mut self) {
        self.search_query.clear();
        self.search_match_index = 0;
    }

    pub fn search_matches(&self, target: PaneFocus) -> Vec<usize> {
        let query = self.search_query.trim();
        if query.is_empty() {
            return Vec::new();
        }
        let needle = query.to_lowercase();
        match target {
            PaneFocus::Activity => self
                .activity_lines
                .iter()
                .enumerate()
                .filter_map(|(idx, entry)| {
                    let haystack = format!("{} {}", entry.icon, entry.text).to_lowercase();
                    haystack.contains(&needle).then_some(idx)
                })
                .collect(),
            PaneFocus::Response => self
                .content_lines
                .iter()
                .map(String::as_str)
                .chain(self.streaming_text.lines())
                .enumerate()
                .filter_map(|(idx, line)| line.to_lowercase().contains(&needle).then_some(idx))
                .collect(),
            PaneFocus::Input => Vec::new(),
        }
    }

    pub fn active_search_match(&self, target: PaneFocus) -> Option<(usize, usize)> {
        let matches = self.search_matches(target);
        if matches.is_empty() {
            return None;
        }
        let current = self.search_match_index % matches.len();
        Some((matches[current], matches.len()))
    }

    pub fn sync_search_scroll(&mut self) {
        let Some((match_idx, _)) = self.active_search_match(self.search_target) else {
            return;
        };
        match self.search_target {
            PaneFocus::Activity => {
                self.scroll_offset = self
                    .activity_lines
                    .len()
                    .saturating_sub(match_idx + 1)
                    .min(u16::MAX as usize) as u16;
                self.focused_pane = PaneFocus::Activity;
            }
            PaneFocus::Response => {
                let total = self.content_lines.len() + self.streaming_text.lines().count();
                self.content_scroll_offset =
                    total.saturating_sub(match_idx + 1).min(u16::MAX as usize) as u16;
                self.focused_pane = PaneFocus::Response;
            }
            PaneFocus::Input => {}
        }
    }

    pub fn step_search_match(&mut self, forward: bool) -> bool {
        let matches = self.search_matches(self.search_target);
        if matches.is_empty() {
            return false;
        }
        if forward {
            self.search_match_index = (self.search_match_index + 1) % matches.len();
        } else if self.search_match_index == 0 {
            self.search_match_index = matches.len() - 1;
        } else {
            self.search_match_index -= 1;
        }
        self.sync_search_scroll();
        true
    }

    /// Start a new turn. Separators are inserted lazily when user-facing content appears.
    pub fn begin_turn(&mut self, turn_number: u32) {
        self.finish_working();
        self.current_turn = turn_number;
        self.current_turn_content_start = self.content_lines.len();
        self.current_turn_had_tool_calls = false;
        self.pending_turn_separator = !self.content_lines.is_empty();
        self.content_scroll_offset = 0;
        self.is_thinking = false;
        self.tag_buffer.clear();
    }

    /// Ensure the current turn starts with a markdown separator when needed.
    pub fn ensure_turn_separator(&mut self) {
        if !self.pending_turn_separator {
            return;
        }
        if self
            .content_lines
            .last()
            .is_some_and(|line| !line.is_empty())
        {
            self.content_lines.push(String::new());
        }
        self.content_lines.push("---".to_string());
        self.content_lines.push(String::new());
        self.pending_turn_separator = false;
    }

    /// Drop any user-facing content emitted earlier in this turn.
    pub fn discard_current_turn_content(&mut self) {
        self.streaming_text.clear();
        if self.content_lines.len() > self.current_turn_content_start {
            self.content_lines.truncate(self.current_turn_content_start);
            self.cached_lines_count = 0;
            self.cached_parsed_lines.clear();
        }
        self.pending_turn_separator = !self.content_lines.is_empty();
        self.content_scroll_offset = 0;
    }

    /// Replace any content emitted in the current turn with finalized text.
    pub fn replace_current_turn_content(&mut self, text: &str) {
        self.discard_current_turn_content();
        if !text.trim().is_empty() {
            self.ensure_turn_separator();
            for line in text.split('\n') {
                self.content_lines
                    .push(line.trim_end_matches('\r').to_string());
            }
        }
        self.cached_lines_count = 0;
        self.cached_parsed_lines.clear();
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
    execute!(
        stderr,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
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
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    )?;
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
        let msg = Paragraph::new(Line::from(vec![Span::styled(
            "  Resize to at least 80 columns",
            Style::default().fg(Color::Yellow),
        )]));
        frame.render_widget(msg, area);
        return;
    }

    // Root layout: top bar · tab bar · body · footer
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // top bar
            Constraint::Length(1), // tab bar
            Constraint::Min(6),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    draw_top_bar(frame, root[0], state);
    draw_tab_bar(frame, root[1], state);

    match state.active_tab {
        TabView::Coding => match state.ui_mode {
            UiMode::Shell => draw_shell_mode(frame, root[2], state),
            UiMode::Task => draw_task_mode(frame, root[2], state, wc),
        },
        TabView::Agents => draw_agents_tab(frame, root[2], state),
        TabView::Context => draw_context_tab(frame, root[2], state),
        TabView::Help => draw_help_tab(frame, root[2], state),
    }

    draw_footer(frame, root[3], state);

    // Draw completion popup as overlay (must come LAST so it renders on top)
    // Find the composer area for popup positioning — it's the bottom of the body
    let body = root[2];
    let composer_h = composer::composer_height(&state.composer);
    if body.height > composer_h {
        let composer_area = Rect::new(
            body.x,
            body.y + body.height - composer_h,
            body.width,
            composer_h,
        );
        composer::draw_completion_popup(frame, composer_area, &state.composer);
    }

    match state.overlay {
        Overlay::None => {}
        Overlay::Help => draw_help_overlay(frame, area, state),
        Overlay::Search => draw_search_overlay(frame, area, state),
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
            if no_c {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            },
        ),
        Span::styled(" ", Style::default()),
        Span::styled(
            format!("{}", s.repo_name),
            if no_c {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            },
        ),
        Span::styled(" ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            &s.branch,
            if no_c {
                Style::default()
            } else {
                Style::default().fg(Color::Magenta)
            },
        ),
        Span::styled("  ", Style::default()),
        Span::styled(
            mode_label,
            if no_c {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            },
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
    let pad = (area.width as usize)
        .saturating_sub(left_width)
        .saturating_sub(right_text.len() + 1);
    if pad > 0 {
        left.push(Span::raw(" ".repeat(pad)));
    }
    left.push(Span::styled(
        format!("{} ", right_text),
        Style::default().fg(Color::DarkGray),
    ));

    let paragraph = Paragraph::new(Line::from(left)).style(if no_c {
        Style::default()
    } else {
        Style::default().bg(Color::Rgb(30, 30, 40))
    });
    frame.render_widget(paragraph, area);
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tab bar — 1 row, shows 4 tabs with active highlight
// ═══════════════════════════════════════════════════════════════════════════

fn draw_tab_bar(frame: &mut Frame, area: Rect, state: &TuiState) {
    let titles: Vec<Line> = TabView::ALL
        .iter()
        .enumerate()
        .map(|(i, tab)| {
            let num = i + 1;
            Line::from(format!(" {} {} ", num, tab.title()))
        })
        .collect();

    let tabs = Tabs::new(titles)
        .select(state.active_tab.index())
        .style(
            Style::default()
                .fg(Color::DarkGray)
                .bg(Color::Rgb(20, 20, 30)),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .bg(Color::Rgb(30, 30, 50))
                .add_modifier(Modifier::BOLD),
        )
        .divider("│");

    frame.render_widget(tabs, area);
}

// ═══════════════════════════════════════════════════════════════════════════
//  Agents tab — registered agents, subagent status, delegation
// ═══════════════════════════════════════════════════════════════════════════

fn draw_agents_tab(frame: &mut Frame, area: Rect, state: &TuiState) {
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![Span::styled(
        "  Agents & Subagents",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));

    // Built-in agents
    lines.push(Line::from(vec![Span::styled(
        "  Built-in Agents",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));

    let builtin_agents = [
        ("explore", "Read-only codebase exploration", "🔍"),
        ("plan", "Structured planning before execution", "📋"),
        ("verify", "Adversarial verification of changes", "✓"),
        ("general", "Full agent with all tools", "⚡"),
        ("guide", "Documentation and explanation", "📖"),
    ];

    for (name, desc, icon) in &builtin_agents {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(format!("{} ", icon), Style::default()),
            Span::styled(
                format!("{:<12}", name),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {}", desc), Style::default().fg(Color::Gray)),
        ]));
    }

    lines.push(Line::from(""));

    // Active subagents (if any)
    lines.push(Line::from(vec![Span::styled(
        "  Subagent Status",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));

    if state.is_working
        && state
            .active_tool
            .as_ref()
            .map(|t| t.tool_name == "subagent")
            .unwrap_or(false)
    {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled("● ", Style::default().fg(Color::Green)),
            Span::styled("Active subagent running", Style::default().fg(Color::White)),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled("○ ", Style::default().fg(Color::DarkGray)),
            Span::styled("No active subagents", Style::default().fg(Color::DarkGray)),
        ]));
    }

    lines.push(Line::from(""));

    // Delegation info
    lines.push(Line::from(vec![Span::styled(
        "  Delegation",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(
            "The LLM can delegate subtasks using the ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled("subagent", Style::default().fg(Color::Cyan)),
        Span::styled(" tool.", Style::default().fg(Color::DarkGray)),
    ]));
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(
            "Subagents run as bounded child processes (max 15 turns).",
            Style::default().fg(Color::DarkGray),
        ),
    ]));

    let paragraph = Paragraph::new(lines).block(Block::default().borders(Borders::NONE));
    frame.render_widget(paragraph, area);
}

// ═══════════════════════════════════════════════════════════════════════════
//  Context tab — token budget, files in context, memory
// ═══════════════════════════════════════════════════════════════════════════

fn draw_context_tab(frame: &mut Frame, area: Rect, state: &TuiState) {
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![Span::styled(
        "  Context & Memory",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));

    // Token usage
    lines.push(Line::from(vec![Span::styled(
        "  Token Budget",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));

    let used = state.status.tokens_used;
    let limit = state.status.tokens_limit;
    let pct = if limit > 0 { used * 100 / limit } else { 0 };
    let bar_width = 30;
    let filled = (pct as usize * bar_width / 100).min(bar_width);
    let bar_color = match pct {
        0..=50 => Color::Green,
        51..=80 => Color::Yellow,
        _ => Color::Red,
    };

    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled("[", Style::default().fg(Color::DarkGray)),
        Span::styled("█".repeat(filled), Style::default().fg(bar_color)),
        Span::styled(
            "░".repeat(bar_width - filled),
            Style::default().fg(Color::Rgb(40, 40, 40)),
        ),
        Span::styled("]", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(" {}%", pct),
            Style::default().fg(bar_color).add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(
            format!("{} / {} tokens", used, limit),
            Style::default().fg(Color::Gray),
        ),
    ]));
    lines.push(Line::from(""));

    // Session info
    lines.push(Line::from(vec![Span::styled(
        "  Session",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));

    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled("Turn:    ", Style::default().fg(Color::Gray)),
        Span::styled(
            format!("{}", state.current_turn),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" / {}", state.max_turns),
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled("Mode:    ", Style::default().fg(Color::Gray)),
        Span::styled(&state.agent_mode, Style::default().fg(Color::Cyan)),
    ]));
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled("Model:   ", Style::default().fg(Color::Gray)),
        Span::styled(&state.status.model, Style::default().fg(Color::Green)),
    ]));
    if state.status.cost > 0.0 {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled("Cost:    ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("${:.4}", state.status.cost),
                Style::default().fg(Color::Yellow),
            ),
        ]));
    }

    lines.push(Line::from(""));

    // Memory status
    lines.push(Line::from(vec![Span::styled(
        "  Memory",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled("Use ", Style::default().fg(Color::DarkGray)),
        Span::styled("/memory", Style::default().fg(Color::Cyan)),
        Span::styled(
            " to view/add/clear persistent memory.",
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled("Stored in ", Style::default().fg(Color::DarkGray)),
        Span::styled(".pipit/MEMORY.md", Style::default().fg(Color::White)),
        Span::styled(" and ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "~/.config/pipit/MEMORY.md",
            Style::default().fg(Color::White),
        ),
    ]));

    let paragraph = Paragraph::new(lines).block(Block::default().borders(Borders::NONE));
    frame.render_widget(paragraph, area);
}

// ═══════════════════════════════════════════════════════════════════════════
//  Help tab — keyboard shortcuts, slash commands, usage guide
// ═══════════════════════════════════════════════════════════════════════════

fn draw_help_tab(frame: &mut Frame, area: Rect, state: &TuiState) {
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![Span::styled(
        "  Pipit Help & Documentation",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));

    // Keyboard shortcuts
    lines.push(Line::from(vec![Span::styled(
        "  Keyboard Shortcuts",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));

    let shortcuts = [
        ("Ctrl+1..4", "Switch tabs (Coding/Agents/Context/Help)"),
        ("Ctrl+←/→", "Cycle tabs left/right"),
        ("Mouse click", "Select a tab from the tab bar"),
        ("F1 / ?", "Toggle help overlay"),
        ("Tab", "Cycle pane focus (Input→Response→Activity)"),
        ("g", "Return to Shell mode from Task"),
        ("Esc", "Cancel current operation / close overlay"),
        ("Ctrl+F", "Search within current pane"),
        ("n / N", "Next / previous search match"),
        ("j/k ↑/↓", "Scroll within focused pane"),
        ("PgUp/PgDn", "Fast scroll"),
        ("Ctrl+C", "Quit pipit"),
    ];

    for (key, desc) in &shortcuts {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(
                format!("{:<16}", key),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(*desc, Style::default().fg(Color::Gray)),
        ]));
    }

    lines.push(Line::from(""));

    // Core slash commands
    lines.push(Line::from(vec![Span::styled(
        "  Core Commands",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));

    let commands = [
        ("/help", "Show available commands"),
        ("/compact", "Compress context to free tokens"),
        ("/clear", "Clear context and start fresh"),
        ("/model <name>", "Switch LLM model"),
        ("/undo", "Undo last agent changes"),
        ("/diff", "Show uncommitted changes"),
        ("/commit [msg]", "AI-generated commit message"),
        ("/plan", "Enter plan-first mode"),
        ("/tdd", "Test-driven development workflow"),
        ("/code-review", "Review uncommitted changes"),
        ("/save-session", "Save session for later"),
        ("/resume-session", "Resume a saved session"),
    ];

    for (cmd, desc) in &commands {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(format!("{:<20}", cmd), Style::default().fg(Color::Cyan)),
            Span::styled(*desc, Style::default().fg(Color::Gray)),
        ]));
    }

    lines.push(Line::from(""));

    // Git commands
    lines.push(Line::from(vec![Span::styled(
        "  Git Commands",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));

    let git_commands = [
        ("/branch [name]", "Create or show branch"),
        ("/switch <branch>", "Switch branch (auto-stash)"),
        ("/branches", "List all branches"),
        ("/search <query>", "Search codebase"),
    ];

    for (cmd, desc) in &git_commands {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(format!("{:<20}", cmd), Style::default().fg(Color::Cyan)),
            Span::styled(*desc, Style::default().fg(Color::Gray)),
        ]));
    }

    lines.push(Line::from(""));

    // Advanced
    lines.push(Line::from(vec![Span::styled(
        "  Advanced",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));

    let advanced = [
        ("/skills", "List available skills"),
        ("/mcp", "MCP server status"),
        ("/hooks", "List active hooks"),
        ("/memory", "Persistent memory (add/list/clear)"),
        ("/deps", "Dependency audit"),
        ("/bench", "Run benchmarks"),
        ("/mesh", "Mesh/delegation management"),
        ("/bg <task>", "Submit background task"),
        ("/loop <sec> <prompt>", "Continuous polling"),
    ];

    for (cmd, desc) in &advanced {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(format!("{:<20}", cmd), Style::default().fg(Color::Cyan)),
            Span::styled(*desc, Style::default().fg(Color::Gray)),
        ]));
    }

    lines.push(Line::from(""));

    // CLI modes
    lines.push(Line::from(vec![Span::styled(
        "  CLI Modes (--mode)",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));

    let modes = [
        ("fast / research", "Direct execution, no verification"),
        ("balanced / dev", "Plans before acting, heuristic verify"),
        ("guarded / review", "Full plan/execute/verify with repair"),
        ("custom", "Guarded with custom role models"),
    ];

    for (mode, desc) in &modes {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(format!("{:<20}", mode), Style::default().fg(Color::Magenta)),
            Span::styled(*desc, Style::default().fg(Color::Gray)),
        ]));
    }

    // Make scrollable
    let scroll = if state.content_scroll_offset > 0 && state.active_tab == TabView::Help {
        state.content_scroll_offset
    } else {
        0
    };

    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::NONE))
        .scroll((scroll, 0));
    frame.render_widget(paragraph, area);
}

// ═══════════════════════════════════════════════════════════════════════════
//  Footer — 1 row, always visible, context-sensitive shortcuts
// ═══════════════════════════════════════════════════════════════════════════

fn draw_footer(frame: &mut Frame, area: Rect, state: &TuiState) {
    let tab_hint = " ctrl+1-4 / ctrl+←→ / click tabs ·";
    let mode_hint = match state.active_tab {
        TabView::Coding => match state.ui_mode {
            UiMode::Shell => {
                if state.is_working {
                    " esc stop · ? help · /help · ctrl+c quit"
                } else {
                    " ? help · @file · !shell · enter send · esc cancel · ctrl+c quit"
                }
            }
            UiMode::Task => " ? help · tab focus · j/k scroll · g shell · esc stop · ctrl+c quit",
        },
        TabView::Agents => " agents view — ctrl+1 to return to coding",
        TabView::Context => " context view — ctrl+1 to return to coding",
        TabView::Help => " j/k scroll — ctrl+1 to return to coding",
    };

    let hints = format!("{}{}", tab_hint, mode_hint);

    // Completion banner on the right if present.
    if let Some(_banner) = &state.completion_status {
        // Banner is shown above the composer in Task mode, not in footer
    }
    frame.render_widget(
        Paragraph::new(Span::styled(&hints, Style::default().fg(Color::DarkGray)))
            .style(Style::default().bg(Color::Rgb(30, 30, 40))),
        area,
    );
}

fn draw_help_overlay(frame: &mut Frame, area: Rect, state: &TuiState) {
    let popup = centered_rect(area, 72, 18);
    let title = match state.ui_mode {
        UiMode::Shell => " help ",
        UiMode::Task => " task shortcuts ",
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled("  ? / F1", Style::default().fg(Color::Cyan)),
            Span::styled("  toggle this popup", Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  Esc", Style::default().fg(Color::Cyan)),
            Span::styled(
                "     close popup (outside popup: stop current run)",
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("  /help", Style::default().fg(Color::Cyan)),
            Span::styled(
                "   show the full markdown help in the response pane",
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(""),
    ];

    match state.ui_mode {
        UiMode::Shell => {
            lines.extend([
                Line::from(vec![
                    Span::styled("  Enter", Style::default().fg(Color::Cyan)),
                    Span::styled("   send prompt", Style::default().fg(Color::White)),
                ]),
                Line::from(vec![
                    Span::styled("  @file", Style::default().fg(Color::Cyan)),
                    Span::styled("   attach file context", Style::default().fg(Color::White)),
                ]),
                Line::from(vec![
                    Span::styled("  !cmd", Style::default().fg(Color::Cyan)),
                    Span::styled("    run a shell command", Style::default().fg(Color::White)),
                ]),
            ]);
        }
        UiMode::Task => {
            lines.extend([
                Line::from(vec![
                    Span::styled("  Ctrl+F", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        "  search the focused activity/response pane",
                        Style::default().fg(Color::White),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("  /", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        "       start a slash command in the input",
                        Style::default().fg(Color::White),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("  n / N", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        "   jump to next / previous search match",
                        Style::default().fg(Color::White),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("  Tab / Shift+Tab", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        "  cycle input, activity, and response focus",
                        Style::default().fg(Color::White),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("  h/l or ←/→", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        "   move focus between activity and response",
                        Style::default().fg(Color::White),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("  j/k or ↓/↑", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        "   scroll when activity/response has focus",
                        Style::default().fg(Color::White),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("  PgUp / PgDn", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        "  jump-scroll the focused pane",
                        Style::default().fg(Color::White),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("  Home / End", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        "  jump to oldest / newest content",
                        Style::default().fg(Color::White),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("  mouse wheel", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        "  scroll hovered pane and focus it",
                        Style::default().fg(Color::White),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("  Alt+↑↓ / Ctrl+↑↓", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        "  legacy direct response/activity scrolling",
                        Style::default().fg(Color::White),
                    ),
                ]),
            ]);
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "  Inspired by lazygit/k9s/btop patterns: ? for help, pane focus, vim-style movement, visible scroll position.",
        Style::default().fg(Color::DarkGray),
    )]));

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(Span::styled(
                    title,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        popup,
    );
}

fn draw_search_overlay(frame: &mut Frame, area: Rect, state: &TuiState) {
    let popup = centered_rect(area, 64, 5);
    let target = match state.search_target {
        PaneFocus::Activity => "activity",
        PaneFocus::Response => "response",
        PaneFocus::Input => "input",
    };
    let match_summary = if let Some((idx, total)) = state.active_search_match(state.search_target) {
        format!(
            "  {}/{} match at line {}",
            (state.search_match_index % total) + 1,
            total,
            idx + 1
        )
    } else if state.search_query.is_empty() {
        "  type to search".to_string()
    } else {
        "  no matches".to_string()
    };
    let cursor = if (state.spinner_frame / 8) % 2 == 0 {
        "▌"
    } else {
        " "
    };
    let lines = vec![
        Line::from(vec![
            Span::styled(
                format!("  /{} ", target),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{}{}", state.search_query, cursor),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(Span::styled(
            match_summary,
            Style::default().fg(Color::DarkGray),
        )),
    ];

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(Span::styled(
                    " search ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        popup,
    );
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let popup_width = area.width.min(width).max(20);
    let popup_height = area.height.min(height).max(6);
    Rect::new(
        area.x + area.width.saturating_sub(popup_width) / 2,
        area.y + area.height.saturating_sub(popup_height) / 2,
        popup_width,
        popup_height,
    )
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
            Constraint::Min(3),             // recent task + hints
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
        lines.push(Line::from(vec![Span::styled(
            " Recent task",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )]));
        lines.push(Line::from(vec![
            Span::styled("   ", Style::default()),
            Span::styled(&banner.icon, Style::default().fg(banner.color)),
            Span::styled(
                format!("  {}", banner.message),
                Style::default().fg(Color::White),
            ),
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
        lines.push(Line::from(vec![Span::styled(
            " Active task",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )]));
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
    lines.push(Line::from(vec![Span::styled(
        " Hints",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(vec![
        Span::styled("   ? /help     ", Style::default().fg(Color::Cyan)),
        Span::styled(
            "commands and shortcuts",
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("   @file       ", Style::default().fg(Color::Green)),
        Span::styled(
            "attach file as context",
            Style::default().fg(Color::DarkGray),
        ),
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
    let banner_h: u16 = if state.completion_status.is_some() {
        1
    } else {
        0
    };
    let status_h: u16 = 2; // dedicated status box above composer (border + content)

    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),          // task title (single row)
            Constraint::Length(activity_h), // activity feed
            Constraint::Min(4),             // response stream
            Constraint::Length(banner_h),   // completion banner
            Constraint::Length(status_h),   // status bar (above input)
            Constraint::Length(composer_h), // composer
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
                Style::default()
                    .fg(Color::Black)
                    .bg(banner.color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {}", banner.message),
                Style::default()
                    .fg(banner.color)
                    .add_modifier(Modifier::BOLD),
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
        Span::styled(
            task_display,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
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

    spans.push(Span::styled(
        " Status: ",
        Style::default().fg(Color::DarkGray),
    ));
    spans.push(Span::styled(status_text, Style::default().fg(Color::Cyan)));

    // Show active tool name if executing
    if let Some(ref tool_info) = state.active_tool {
        spans.push(Span::styled(
            format!("  ▸ {}", tool_info.tool_name),
            Style::default().fg(Color::Yellow),
        ));
    }

    if state.ui_mode == UiMode::Task {
        let focus_label = match state.focused_pane {
            PaneFocus::Input => "input",
            PaneFocus::Activity => "activity",
            PaneFocus::Response => "response",
        };
        spans.push(Span::styled(
            format!("  focus: {}", focus_label),
            Style::default().fg(Color::DarkGray),
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
    let inner_height = area.height.saturating_sub(2) as usize;
    let total = state.activity_lines.len();
    let max_scroll = total.saturating_sub(inner_height);
    let scroll_from_bottom = usize::from(state.scroll_offset).min(max_scroll);
    let start = total.saturating_sub(inner_height + scroll_from_bottom);
    let end = total.saturating_sub(scroll_from_bottom);
    let top_scroll = max_scroll.saturating_sub(scroll_from_bottom);
    let scroll_info = if total > inner_height {
        format!(" {}/{} ", end, total)
    } else {
        String::new()
    };
    let is_focused = state.focused_pane == PaneFocus::Activity;
    let matches = state.search_matches(PaneFocus::Activity);
    let active_match = if state.search_target == PaneFocus::Activity {
        state
            .active_search_match(PaneFocus::Activity)
            .map(|(idx, _)| idx)
    } else {
        None
    };

    let mut lines: Vec<Line> = Vec::new();

    for (offset, entry) in state.activity_lines[start..end].iter().enumerate() {
        let line_idx = start + offset;
        let is_match = matches.contains(&line_idx);
        let is_current = active_match == Some(line_idx);
        let max_text = (area.width as usize).saturating_sub(6);
        let mut line = if entry.icon.is_empty() {
            Line::from(Span::styled(
                format!("   {}", truncate_str(&entry.text, max_text)),
                Style::default().fg(entry.color),
            ))
        } else {
            Line::from(vec![
                Span::styled(
                    format!("   {} ", entry.icon),
                    Style::default().fg(entry.color),
                ),
                Span::raw(truncate_str(&entry.text, max_text)),
            ])
        };
        if is_match {
            apply_search_highlight(&mut line, is_current);
        }
        lines.push(line);
    }

    let search_summary =
        if state.search_target == PaneFocus::Activity && !state.search_query.is_empty() {
            if let Some((_idx, total_matches)) = state.active_search_match(PaneFocus::Activity) {
                format!(
                    "  /{} ({}/{})",
                    state.search_query,
                    (state.search_match_index % total_matches) + 1,
                    total_matches
                )
            } else {
                format!("  /{} (0)", state.search_query)
            }
        } else {
            String::new()
        };

    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(if is_focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::Rgb(40, 40, 50))
        })
        .title(Span::styled(
            if is_focused {
                " activity [focus] "
            } else {
                " activity "
            },
            if is_focused {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ))
        .title_bottom(Span::styled(
            format!("{}{}", scroll_info, search_summary),
            Style::default().fg(Color::DarkGray),
        ));
    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);

    if total > inner_height && area.width > 3 {
        let mut scrollbar_state = ScrollbarState::new(total)
            .position(top_scroll)
            .viewport_content_length(inner_height);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_style(if is_focused {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            })
            .track_style(Style::default().fg(Color::Rgb(40, 40, 50)));
        frame.render_stateful_widget(
            scrollbar,
            area.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut scrollbar_state,
        );
    }
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
        if (state.spinner_frame / 8) % 2 == 0 {
            "▌"
        } else {
            " "
        }
    } else {
        ""
    };

    let mut all_lines: Vec<Line> = Vec::with_capacity(inner_height + 2);
    let mut in_code_block = false;
    let mut in_diff_block = false;
    let mut code_lang = String::new();
    let mut in_turn_cell = false;
    let mut turn_has_body = false;
    let mut turn_start_index: Option<usize> = None;
    let mut prev_was_empty = false;
    let content_matches = if state.search_target == PaneFocus::Response {
        state.search_matches(PaneFocus::Response)
    } else {
        Vec::new()
    };
    let content_active_match = if state.search_target == PaneFocus::Response {
        state
            .active_search_match(PaneFocus::Response)
            .map(|(idx, _)| idx)
    } else {
        None
    };

    // Helper: push with optional turn gutter
    macro_rules! emit {
        ($line:expr, $is_match:expr, $is_current:expr) => {{
            let mut line: Line<'static> = $line;
            if $is_match {
                apply_search_highlight(&mut line, $is_current);
            }
            if in_turn_cell && !in_code_block {
                let mut bordered = vec![Span::styled(" │ ", Style::default().fg(Color::DarkGray))];
                bordered.extend(line.spans);
                all_lines.push(Line::from(bordered));
            } else {
                all_lines.push(line);
            }
        }};
    }

    for (raw_line_index, raw) in state
        .content_lines
        .iter()
        .map(String::as_str)
        .chain(streaming_lines.iter().copied())
        .enumerate()
    {
        let trimmed = raw.trim();
        let raw_is_match = content_matches.contains(&raw_line_index);
        let raw_is_current = content_active_match == Some(raw_line_index);

        // ── Code fence ──
        if trimmed.starts_with("```") {
            if in_code_block {
                in_code_block = false;
                turn_has_body |= in_turn_cell;
                emit!(
                    Line::from(Span::styled(
                        format!("  └{}", "─".repeat(pane_width.saturating_sub(5).min(30))),
                        Style::default().fg(Color::DarkGray),
                    )),
                    raw_is_match,
                    raw_is_current
                );
                code_lang.clear();
                prev_was_empty = false;
            } else {
                in_code_block = true;
                turn_has_body |= in_turn_cell;
                code_lang = trimmed.trim_start_matches('`').to_string();
                if !prev_was_empty {
                    emit!(Line::from(""), raw_is_match, raw_is_current);
                }
                let label = if code_lang.is_empty() {
                    " code ".to_string()
                } else {
                    format!(" {} ", code_lang)
                };
                emit!(
                    Line::from(vec![
                        Span::styled("  ┌", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            label,
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD)
                        ),
                        Span::styled(
                            "─".repeat(pane_width.saturating_sub(6 + code_lang.len()).min(16)),
                            Style::default().fg(Color::DarkGray)
                        ),
                    ]),
                    raw_is_match,
                    raw_is_current
                );
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
                spans.push(Span::styled(
                    raw.to_string(),
                    Style::default().fg(Color::Green),
                ));
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

        // ── Unified diff blocks ──
        if !in_diff_block && looks_like_diff_start(raw) {
            in_diff_block = true;
        }
        if in_diff_block {
            if raw.is_empty() || looks_like_diff_line(raw) {
                turn_has_body |= in_turn_cell;
                emit!(render_diff_line(raw), raw_is_match, raw_is_current);
                prev_was_empty = raw.is_empty();
                continue;
            }
            in_diff_block = false;
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
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        " {}",
                        "─".repeat(pane_width.saturating_sub(turn_label.len() + 6).min(40))
                    ),
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
            emit!(
                Line::from(Span::styled(
                    format!(" {}", trimmed),
                    Style::default().fg(Color::DarkGray)
                )),
                raw_is_match,
                raw_is_current
            );
            prev_was_empty = false;
            continue;
        }

        if in_turn_cell && !trimmed.is_empty() {
            turn_has_body = true;
        }

        // ── Markdown headers ──
        if trimmed.starts_with("### ") {
            let heading = trimmed.trim_start_matches("### ");
            if !prev_was_empty {
                emit!(Line::from(""), raw_is_match, raw_is_current);
            }
            emit!(
                Line::from(vec![
                    Span::styled("   ", Style::default()),
                    Span::styled(
                        heading.to_string(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    ),
                ]),
                raw_is_match,
                raw_is_current
            );
            emit!(Line::from(""), raw_is_match, raw_is_current);
            prev_was_empty = true;
            continue;
        }
        if trimmed.starts_with("## ") {
            let heading = trimmed.trim_start_matches("## ");
            if !prev_was_empty {
                emit!(Line::from(""), raw_is_match, raw_is_current);
            }
            emit!(
                Line::from(vec![
                    Span::styled(" ◆ ", Style::default().fg(Color::Yellow)),
                    Span::styled(
                        heading.to_string(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    ),
                ]),
                raw_is_match,
                raw_is_current
            );
            emit!(Line::from(""), raw_is_match, raw_is_current);
            prev_was_empty = true;
            continue;
        }
        if trimmed.starts_with("# ") {
            let heading = trimmed.trim_start_matches("# ");
            if !prev_was_empty {
                emit!(Line::from(""), raw_is_match, raw_is_current);
            }
            emit!(
                Line::from(vec![
                    Span::styled(
                        format!(" ━━ {} ", heading),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    ),
                    Span::styled(
                        "━".repeat(pane_width.saturating_sub(heading.len() + 6).min(20)),
                        Style::default().fg(Color::DarkGray)
                    ),
                ]),
                raw_is_match,
                raw_is_current
            );
            emit!(Line::from(""), raw_is_match, raw_is_current);
            prev_was_empty = true;
            continue;
        }

        // ── Bullet points ──
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            let text = &trimmed[2..];
            let mut spans = vec![Span::styled("   • ", Style::default().fg(Color::Cyan))];
            spans.extend(parse_inline_spans(text));
            emit!(Line::from(spans), raw_is_match, raw_is_current);
            prev_was_empty = false;
            continue;
        }

        // ── Numbered lists ──
        if trimmed.len() > 2 && trimmed.as_bytes()[0].is_ascii_digit() {
            if let Some(rest) = trimmed
                .strip_prefix(|c: char| c.is_ascii_digit())
                .and_then(|s| s.strip_prefix(". "))
            {
                let num_str: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
                let mut spans = vec![Span::styled(
                    format!("   {}. ", num_str),
                    Style::default().fg(Color::Cyan),
                )];
                spans.extend(parse_inline_spans(rest));
                emit!(Line::from(spans), raw_is_match, raw_is_current);
                prev_was_empty = false;
                continue;
            }
        }

        // ── Blockquotes ──
        if trimmed.starts_with("> ") {
            let text = &trimmed[2..];
            let mut spans = vec![Span::styled("  ▎ ", Style::default().fg(Color::Blue))];
            for s in parse_inline_spans(text) {
                spans.push(Span::styled(
                    s.content.to_string(),
                    s.style.add_modifier(Modifier::ITALIC),
                ));
            }
            emit!(Line::from(spans), raw_is_match, raw_is_current);
            prev_was_empty = false;
            continue;
        }

        // ── Horizontal rule ──
        if (trimmed == "---" || trimmed == "***" || trimmed == "___")
            || (trimmed.len() >= 3 && trimmed.chars().all(|c| c == '-' || c == ' '))
        {
            emit!(
                Line::from(Span::styled(
                    format!("  {}", "─".repeat(pane_width.saturating_sub(4).min(50))),
                    Style::default().fg(Color::Rgb(50, 50, 60)),
                )),
                raw_is_match,
                raw_is_current
            );
            prev_was_empty = false;
            continue;
        }

        // ── Table rows ──
        if trimmed.starts_with('|') && trimmed.ends_with('|') {
            if trimmed
                .chars()
                .all(|c| c == '|' || c == '-' || c == ':' || c == ' ')
            {
                emit!(
                    Line::from(Span::styled(
                        format!("   {}", "─".repeat(pane_width.saturating_sub(6).min(50))),
                        Style::default().fg(Color::DarkGray),
                    )),
                    raw_is_match,
                    raw_is_current
                );
            } else {
                let cells: Vec<&str> = trimmed
                    .split('|')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.trim())
                    .collect();
                let mut spans = vec![Span::styled("   ", Style::default())];
                for (i, cell) in cells.iter().enumerate() {
                    if i > 0 {
                        spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
                    }
                    spans.extend(parse_inline_spans(cell));
                }
                emit!(Line::from(spans), raw_is_match, raw_is_current);
            }
            prev_was_empty = false;
            continue;
        }

        // ── Empty line ──
        if trimmed.is_empty() {
            if in_turn_cell && !turn_has_body {
                continue;
            }
            if !prev_was_empty {
                emit!(Line::from(""), raw_is_match, raw_is_current);
                prev_was_empty = true;
            }
            continue;
        }

        // ── Default paragraph text ──
        turn_has_body |= in_turn_cell;
        prev_was_empty = false;
        emit!(style_paragraph_line(raw), raw_is_match, raw_is_current);
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
    if all_lines.is_empty()
        && (state.is_thinking || (state.is_working && state.streaming_text.is_empty()))
    {
        const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let spin_frame = (state.spinner_frame / 4) as usize % SPINNER.len();
        let label = if state.is_thinking {
            "reasoning"
        } else {
            "thinking"
        };
        let spinner_color = if state.is_thinking {
            Color::Magenta
        } else {
            Color::Cyan
        };
        all_lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", SPINNER[spin_frame]),
                Style::default().fg(spinner_color),
            ),
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
    let search_summary =
        if state.search_target == PaneFocus::Response && !state.search_query.is_empty() {
            if let Some((_idx, total_matches)) = state.active_search_match(PaneFocus::Response) {
                format!(
                    "  /{} ({}/{})",
                    state.search_query,
                    (state.search_match_index % total_matches) + 1,
                    total_matches
                )
            } else {
                format!("  /{} (0)", state.search_query)
            }
        } else {
            String::new()
        };

    let is_focused = state.focused_pane == PaneFocus::Response;
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(if is_focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::Rgb(50, 50, 60))
        })
        .title(Span::styled(
            if is_focused {
                " response [focus] "
            } else {
                " response "
            },
            if is_focused {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ))
        .title_bottom(Span::styled(
            format!("{}{}", scroll_info, search_summary),
            Style::default().fg(Color::DarkGray),
        ));

    // Append live streaming cursor at the end of content
    if !cursor_char.is_empty() {
        if let Some(last_line) = all_lines.last_mut() {
            last_line.spans.push(Span::styled(
                cursor_char.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            all_lines.push(Line::from(Span::styled(
                format!("   {}", cursor_char),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
        }
    }

    let paragraph = Paragraph::new(all_lines)
        .wrap(ratatui::widgets::Wrap { trim: false })
        .scroll((top_scroll.min(u16::MAX as usize) as u16, 0))
        .block(block);
    frame.render_widget(paragraph, area);

    if total > inner_height && area.width > 3 {
        let mut scrollbar_state = ScrollbarState::new(total)
            .position(top_scroll)
            .viewport_content_length(inner_height);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_style(if is_focused {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            })
            .track_style(Style::default().fg(Color::Rgb(40, 40, 50)));
        frame.render_stateful_widget(
            scrollbar,
            area.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut scrollbar_state,
        );
    }
}

/// Style a paragraph line with inline markdown: `code`, **bold**, *italic*.
fn style_paragraph_line(raw: &str) -> Line<'static> {
    let spans = parse_inline_spans(raw);
    if spans.len() == 1 {
        Line::from(Span::styled(
            format!("   {}", raw),
            Style::default().fg(Color::White),
        ))
    } else {
        let mut result = vec![Span::raw("   ")];
        result.extend(spans);
        Line::from(result)
    }
}

fn looks_like_diff_start(raw: &str) -> bool {
    raw.starts_with("diff --git ")
        || raw.starts_with("index ")
        || raw.starts_with("--- ")
        || raw.starts_with("+++ ")
        || raw.starts_with("@@ ")
        || raw == "(new file)"
        || raw == "(deleted)"
}

fn looks_like_diff_line(raw: &str) -> bool {
    looks_like_diff_start(raw)
        || raw.starts_with('+')
        || raw.starts_with('-')
        || raw.starts_with(' ')
        || raw.starts_with("\\ No newline at end of file")
}

fn render_diff_line(raw: &str) -> Line<'static> {
    let style = if raw.starts_with("@@ ") {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else if raw.starts_with("+++ ") || raw.starts_with('+') {
        Style::default().fg(Color::Green)
    } else if raw.starts_with("--- ") || raw.starts_with('-') {
        Style::default().fg(Color::Red)
    } else if raw.starts_with("diff --git ")
        || raw.starts_with("index ")
        || raw.starts_with("\\ No newline at end of file")
        || raw == "(new file)"
        || raw == "(deleted)"
    {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };

    if raw.is_empty() {
        Line::from(Span::raw("   "))
    } else {
        Line::from(vec![
            Span::styled("   ", Style::default()),
            Span::styled(raw.to_string(), style),
        ])
    }
}

fn apply_search_highlight(line: &mut Line<'static>, is_current: bool) {
    let bg = if is_current {
        Color::Rgb(0, 70, 90)
    } else {
        Color::Rgb(45, 45, 20)
    };
    for span in &mut line.spans {
        span.style = span.style.bg(bg);
        if is_current && span.style.fg.is_none() {
            span.style = span.style.fg(Color::White);
        }
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
                    if c == '`' {
                        chars.next();
                        break;
                    }
                    code.push(c);
                    chars.next();
                }
                spans.push(Span::styled(
                    code,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
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
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ));
                } else {
                    // *italic*
                    if !buf.is_empty() {
                        spans.push(Span::styled(buf.clone(), Style::default().fg(Color::White)));
                        buf.clear();
                    }
                    let mut italic = String::new();
                    while let Some(&(_, c)) = chars.peek() {
                        if c == '*' {
                            chars.next();
                            break;
                        }
                        italic.push(c);
                        chars.next();
                    }
                    spans.push(Span::styled(
                        italic,
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::ITALIC),
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

#[cfg(test)]
mod tests {
    use super::{
        Overlay, PaneFocus, TabView, TuiState, handle_key, handle_mouse, looks_like_diff_line,
        looks_like_diff_start, render_diff_line, scroll_target_for_mouse,
    };
    use crate::tui::StatusBarState;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use pipit_config::ApprovalMode;
    use ratatui::style::Color;
    use std::path::PathBuf;

    #[test]
    fn detects_unified_diff_markers() {
        assert!(looks_like_diff_start("--- a/src/lib.rs"));
        assert!(looks_like_diff_line("+use anyhow::Result;"));
        assert!(looks_like_diff_line("@@ -1,3 +1,4 @@"));
        assert!(!looks_like_diff_start("### Edited `src/lib.rs`"));
    }

    #[test]
    fn renders_added_diff_lines_in_green() {
        let line = render_diff_line("+let updated = true;");
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[1].style.fg, Some(Color::Green));
    }

    #[test]
    fn mouse_scroll_targets_activity_region_in_task_mode() {
        let status = StatusBarState::new(
            "repo".to_string(),
            "model".to_string(),
            ApprovalMode::Suggest,
        );
        let mut state = TuiState::new(status, PathBuf::from("."));
        state.ui_mode = super::UiMode::Task;
        let target = scroll_target_for_mouse(&state, 3, 120, 40);
        assert_eq!(target, PaneFocus::Activity);
    }

    #[test]
    fn tab_cycles_task_focus() {
        let status = StatusBarState::new(
            "repo".to_string(),
            "model".to_string(),
            ApprovalMode::Suggest,
        );
        let mut state = TuiState::new(status, PathBuf::from("."));
        state.ui_mode = super::UiMode::Task;
        state.focused_pane = PaneFocus::Response;
        let consumed = handle_key(&mut state, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(consumed);
        assert_eq!(state.focused_pane, PaneFocus::Activity);
    }

    #[test]
    fn ctrl_right_cycles_tabs_forward() {
        let status = StatusBarState::new(
            "repo".to_string(),
            "model".to_string(),
            ApprovalMode::Suggest,
        );
        let mut state = TuiState::new(status, PathBuf::from("."));
        let consumed = handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL),
        );
        assert!(consumed);
        assert_eq!(state.active_tab, TabView::Agents);
    }

    #[test]
    fn mouse_click_on_tab_bar_selects_tab() {
        let status = StatusBarState::new(
            "repo".to_string(),
            "model".to_string(),
            ApprovalMode::Suggest,
        );
        let mut state = TuiState::new(status, PathBuf::from("."));
        let consumed = handle_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 15,
                row: 1,
                modifiers: KeyModifiers::NONE,
            },
            120,
            40,
        );
        assert!(consumed);
        assert_eq!(state.active_tab, TabView::Agents);
    }

    #[test]
    fn question_mark_opens_help_overlay_when_input_is_empty() {
        let status = StatusBarState::new(
            "repo".to_string(),
            "model".to_string(),
            ApprovalMode::Suggest,
        );
        let mut state = TuiState::new(status, PathBuf::from("."));
        let consumed = handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT),
        );
        assert!(consumed);
        assert_eq!(state.overlay, Overlay::Help);
    }

    #[test]
    fn typing_moves_focus_back_to_input() {
        let status = StatusBarState::new(
            "repo".to_string(),
            "model".to_string(),
            ApprovalMode::Suggest,
        );
        let mut state = TuiState::new(status, PathBuf::from("."));
        state.ui_mode = super::UiMode::Task;
        state.focused_pane = PaneFocus::Response;
        let consumed = handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        assert!(consumed);
        assert_eq!(state.focused_pane, PaneFocus::Input);
    }

    #[test]
    fn ctrl_f_starts_search_for_focused_response_pane() {
        let status = StatusBarState::new(
            "repo".to_string(),
            "model".to_string(),
            ApprovalMode::Suggest,
        );
        let mut state = TuiState::new(status, PathBuf::from("."));
        state.ui_mode = super::UiMode::Task;
        state.focused_pane = PaneFocus::Response;
        let consumed = handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
        );
        assert!(consumed);
        assert_eq!(state.overlay, Overlay::Search);
        assert_eq!(state.search_target, PaneFocus::Response);
    }

    #[test]
    fn slash_moves_focus_to_input_for_commands() {
        let status = StatusBarState::new(
            "repo".to_string(),
            "model".to_string(),
            ApprovalMode::Suggest,
        );
        let mut state = TuiState::new(status, PathBuf::from("."));
        state.ui_mode = super::UiMode::Task;
        state.focused_pane = PaneFocus::Response;
        let consumed = handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
        );
        assert!(consumed);
        assert_eq!(state.focused_pane, PaneFocus::Input);
        assert_eq!(state.overlay, Overlay::None);
        assert_eq!(state.composer.text(), "/");
    }

    #[test]
    fn search_navigation_moves_to_next_match() {
        let status = StatusBarState::new(
            "repo".to_string(),
            "model".to_string(),
            ApprovalMode::Suggest,
        );
        let mut state = TuiState::new(status, PathBuf::from("."));
        state.ui_mode = super::UiMode::Task;
        state.focused_pane = PaneFocus::Response;
        state.content_lines = vec![
            "alpha".to_string(),
            "beta".to_string(),
            "alpha beta".to_string(),
        ];
        state.search_target = PaneFocus::Response;
        state.search_query = "alpha".to_string();
        let consumed = handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        );
        assert!(consumed);
        assert_eq!(state.search_match_index, 1);
    }
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

    // Tab switching: Ctrl+1/2/3/4 or F2/F3/F4/F5
    match key.code {
        KeyCode::Char('1') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.active_tab = TabView::Coding;
            return true;
        }
        KeyCode::Char('2') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.active_tab = TabView::Agents;
            return true;
        }
        KeyCode::Char('3') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.active_tab = TabView::Context;
            return true;
        }
        KeyCode::Char('4') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.active_tab = TabView::Help;
            return true;
        }
        KeyCode::F(2) => {
            state.active_tab = TabView::Coding;
            return true;
        }
        KeyCode::F(3) => {
            state.active_tab = TabView::Agents;
            return true;
        }
        KeyCode::F(4) => {
            state.active_tab = TabView::Context;
            return true;
        }
        KeyCode::F(5) => {
            state.active_tab = TabView::Help;
            return true;
        }
        KeyCode::Left if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.cycle_tab(false);
            return true;
        }
        KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.cycle_tab(true);
            return true;
        }
        _ => {}
    }

    if matches!(key.code, KeyCode::F(1))
        || (matches!(key.code, KeyCode::Char('?')) && state.composer.is_empty())
    {
        state.overlay = match state.overlay {
            Overlay::None => Overlay::Help,
            Overlay::Help => Overlay::None,
            Overlay::Search => Overlay::Help,
        };
        return true;
    }

    if state.overlay == Overlay::Search {
        match key.code {
            KeyCode::Esc => {
                state.overlay = Overlay::None;
                if state.search_query.is_empty() {
                    state.clear_search();
                }
            }
            KeyCode::Enter => {
                state.overlay = Overlay::None;
                state.sync_search_scroll();
            }
            KeyCode::Backspace => {
                state.search_query.pop();
                state.search_match_index = 0;
                state.sync_search_scroll();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                state.clear_search();
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                state.search_query.push(ch);
                state.search_match_index = 0;
                state.sync_search_scroll();
            }
            _ => {}
        }
        return true;
    }

    if state.overlay == Overlay::Help {
        if matches!(key.code, KeyCode::Esc) {
            state.overlay = Overlay::None;
        }
        return true;
    }

    let task_mode = state.ui_mode == UiMode::Task && state.composer.is_empty();
    let pane_nav = task_mode && state.focused_pane != PaneFocus::Input;

    // Content pane scrolling: Alt-Up/Down
    match key.code {
        // Mode switching: 'g' goes to shell (only when composer is empty)
        KeyCode::Char('g')
            if state.ui_mode == UiMode::Task
                && state.composer.is_empty()
                && !key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            state.ui_mode = UiMode::Shell;
            return true;
        }
        KeyCode::Char('f')
            if task_mode
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(
                    state.focused_pane,
                    PaneFocus::Activity | PaneFocus::Response
                ) =>
        {
            state.begin_search(state.focused_pane);
            return true;
        }
        KeyCode::Char('n') if pane_nav && !state.search_query.is_empty() => {
            return state.step_search_match(true);
        }
        KeyCode::Char('N') if pane_nav && !state.search_query.is_empty() => {
            return state.step_search_match(false);
        }
        KeyCode::Tab if task_mode => {
            state.cycle_focus(true);
            return true;
        }
        KeyCode::BackTab if task_mode => {
            state.cycle_focus(false);
            return true;
        }
        KeyCode::Left if pane_nav => {
            state.cycle_focus(false);
            return true;
        }
        KeyCode::Right if pane_nav => {
            state.cycle_focus(true);
            return true;
        }
        KeyCode::Char('h') if pane_nav => {
            state.cycle_focus(false);
            return true;
        }
        KeyCode::Char('l') if pane_nav => {
            state.cycle_focus(true);
            return true;
        }
        KeyCode::Up | KeyCode::Char('k') if pane_nav => {
            match state.focused_pane {
                PaneFocus::Input => {}
                PaneFocus::Activity => state.scroll_activity_by(1),
                PaneFocus::Response => state.scroll_content_by(1),
            }
            return true;
        }
        KeyCode::Down | KeyCode::Char('j') if pane_nav => {
            match state.focused_pane {
                PaneFocus::Input => {}
                PaneFocus::Activity => state.scroll_activity_by(-1),
                PaneFocus::Response => state.scroll_content_by(-1),
            }
            return true;
        }
        KeyCode::PageUp if pane_nav => {
            match state.focused_pane {
                PaneFocus::Input => {}
                PaneFocus::Activity => state.scroll_activity_by(10),
                PaneFocus::Response => state.scroll_content_by(10),
            }
            return true;
        }
        KeyCode::PageDown if pane_nav => {
            match state.focused_pane {
                PaneFocus::Input => {}
                PaneFocus::Activity => state.scroll_activity_by(-10),
                PaneFocus::Response => state.scroll_content_by(-10),
            }
            return true;
        }
        KeyCode::Home if pane_nav => {
            match state.focused_pane {
                PaneFocus::Input => {}
                PaneFocus::Activity => state.jump_activity_to_oldest(),
                PaneFocus::Response => state.jump_content_to_oldest(),
            }
            return true;
        }
        KeyCode::End if pane_nav => {
            match state.focused_pane {
                PaneFocus::Input => {}
                PaneFocus::Activity => state.scroll_offset = 0,
                PaneFocus::Response => state.content_scroll_offset = 0,
            }
            return true;
        }
        KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.scroll_activity_by(1);
            return true;
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.scroll_activity_by(-1);
            return true;
        }
        KeyCode::PageUp if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.scroll_activity_by(10);
            return true;
        }
        KeyCode::PageDown if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.scroll_activity_by(-10);
            return true;
        }
        KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
            state.scroll_content_by(1);
            return true;
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
            state.scroll_content_by(-1);
            return true;
        }
        KeyCode::PageUp if key.modifiers.contains(KeyModifiers::ALT) => {
            state.scroll_content_by(10);
            return true;
        }
        KeyCode::PageDown if key.modifiers.contains(KeyModifiers::ALT) => {
            state.scroll_content_by(-10);
            return true;
        }
        _ => {}
    }

    if state.ui_mode == UiMode::Task
        && state.focused_pane != PaneFocus::Input
        && matches!(key.code, KeyCode::Char(_))
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
    {
        state.focused_pane = PaneFocus::Input;
    }

    // Delegate to the composer
    state.composer.handle_key(key)
}

/// Handle a mouse event, updating state. Returns true if the event was consumed.
/// Region-aware: scrolling in the activity pane scrolls activity,
/// scrolling in the content pane scrolls content.
pub fn handle_mouse(
    state: &mut TuiState,
    mouse: MouseEvent,
    terminal_width: u16,
    terminal_height: u16,
) -> bool {
    if state.overlay != Overlay::None {
        if matches!(mouse.kind, MouseEventKind::Down(_)) {
            state.overlay = Overlay::None;
            return true;
        }
    }

    match mouse.kind {
        MouseEventKind::Down(_) => {
            if mouse.row == 1 {
                if let Some(tab) = tab_for_mouse_column(mouse.column) {
                    state.active_tab = tab;
                    return true;
                }
            }
            if state.ui_mode == UiMode::Task {
                state.focused_pane =
                    scroll_target_for_mouse(state, mouse.row, terminal_width, terminal_height);
                return true;
            }
            false
        }
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
            let target = scroll_target_for_mouse(state, mouse.row, terminal_width, terminal_height);
            state.focused_pane = target;
            match target {
                PaneFocus::Input => return false,
                PaneFocus::Activity => {
                    if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                        state.scroll_activity_by(3);
                    } else {
                        state.scroll_activity_by(-3);
                    }
                }
                PaneFocus::Response => {
                    if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                        state.scroll_content_by(3);
                    } else {
                        state.scroll_content_by(-3);
                    }
                }
            }
            true
        }
        _ => false,
    }
}

fn tab_for_mouse_column(column: u16) -> Option<TabView> {
    let mut start: u16 = 0;
    for (index, tab) in TabView::ALL.iter().enumerate() {
        let label = format!(" {} {} ", index + 1, tab.title());
        let width = label.chars().count() as u16;
        let end = start.saturating_add(width);
        if column >= start && column < end {
            return Some(*tab);
        }
        start = end.saturating_add(1); // divider width
    }
    None
}

fn scroll_target_for_mouse(
    state: &TuiState,
    row: u16,
    terminal_width: u16,
    terminal_height: u16,
) -> PaneFocus {
    if state.ui_mode != UiMode::Task {
        return PaneFocus::Input;
    }

    let composer_h = composer::composer_height(&state.composer);
    let wc = WidthClass::from_width(terminal_width);
    let activity_h = if wc == WidthClass::Compact { 5 } else { 7 };
    let banner_h: u16 = if state.completion_status.is_some() {
        1
    } else {
        0
    };
    let status_h: u16 = 2;
    let total_height = terminal_height.max(2);
    let body_y: u16 = 1; // top bar row
    let body_end = total_height.saturating_sub(1); // footer row
    let activity_start = body_y + 1; // task header row
    let activity_end = activity_start + activity_h;
    let content_end = body_end.saturating_sub(banner_h + status_h + composer_h);

    if row >= activity_start && row < activity_end {
        PaneFocus::Activity
    } else if row >= activity_end && row < content_end {
        PaneFocus::Response
    } else {
        PaneFocus::Input
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
    let truncated: String = s
        .chars()
        .take_while(|c| {
            current_width += unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
            current_width <= max.saturating_sub(1)
        })
        .collect();
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
        let truncated: String = path
            .chars()
            .take_while(|c| {
                current_width += unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
                current_width <= max.saturating_sub(1)
            })
            .collect();
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
    let truncated: String = last
        .chars()
        .take_while(|c| {
            current_width += unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
            current_width <= max.saturating_sub(1)
        })
        .collect();
    format!("{}…", truncated)
}

/// Highlight a single line of code using syntect.
/// Returns an empty vec if the language is unknown.
fn highlight_code_line<'a>(line: &str, lang: &str) -> Vec<Span<'a>> {
    use syntect::easy::HighlightLines;
    use syntect::highlighting::FontStyle;

    let ss = syntax_set();
    let syntax = ss
        .find_syntax_by_token(lang)
        .or_else(|| ss.find_syntax_by_extension(lang))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let theme = highlight_theme();

    let mut h = HighlightLines::new(syntax, theme);
    let regions = match h.highlight_line(line, ss) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    regions
        .into_iter()
        .map(|(style, text)| {
            let fg = Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b);
            let mut ratatui_style = Style::default().fg(fg);
            if style.font_style.contains(FontStyle::BOLD) {
                ratatui_style = ratatui_style.add_modifier(Modifier::BOLD);
            }
            if style.font_style.contains(FontStyle::ITALIC) {
                ratatui_style = ratatui_style.add_modifier(Modifier::ITALIC);
            }
            Span::styled(text.to_string(), ratatui_style)
        })
        .collect()
}
