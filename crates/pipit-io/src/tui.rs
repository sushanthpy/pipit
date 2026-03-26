use pipit_config::ApprovalMode;
use pipit_core::AgentEvent;
use std::io::{self, Write};

// ─── ANSI helpers ────────────────────────────────────────────────────────────

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BLUE: &str = "\x1b[34m";
const CYAN: &str = "\x1b[36m";
const BOLD_CYAN: &str = "\x1b[1;36m";
const BOLD_BLUE: &str = "\x1b[1;34m";
const BOLD_GREEN: &str = "\x1b[1;32m";
const BG_DARK: &str = "\x1b[48;5;236m";
const FG_WHITE: &str = "\x1b[97m";
const FG_GRAY: &str = "\x1b[38;5;250m";

// ─── Status bar state ────────────────────────────────────────────────────────

/// Persistent state for the top status bar.
#[derive(Debug, Clone)]
pub struct StatusBarState {
    pub repo_name: String,
    pub branch: String,
    pub dirty: bool,
    pub model: String,
    pub approval_mode: ApprovalMode,
    pub sandbox: bool,
    pub tokens_used: u64,
    pub tokens_limit: u64,
    pub cost: f64,
    pub verification: VerificationState,
}

#[derive(Debug, Clone, Default)]
pub enum VerificationState {
    #[default]
    Unknown,
    Passing,
    Failing(String),
    Running,
}

impl StatusBarState {
    pub fn new(repo_name: String, model: String, approval_mode: ApprovalMode) -> Self {
        Self {
            repo_name,
            branch: detect_git_branch().unwrap_or_else(|| "—".to_string()),
            dirty: detect_git_dirty(),
            model,
            approval_mode,
            sandbox: false,
            tokens_used: 0,
            tokens_limit: 200_000,
            cost: 0.0,
            verification: VerificationState::default(),
        }
    }

    fn token_pct(&self) -> u64 {
        if self.tokens_limit == 0 {
            0
        } else {
            (self.tokens_used * 100) / self.tokens_limit
        }
    }
}

// ─── Activity stream ─────────────────────────────────────────────────────────

/// A single entry in the activity stream.
#[derive(Debug, Clone)]
pub struct ActivityEntry {
    kind: ActivityKind,
    text: String,
}

#[derive(Debug, Clone)]
enum ActivityKind {
    Read,
    Edit,
    Command,
    Info,
    Warning,
    Error,
    Approval,
    Plan,
}

impl ActivityKind {
    fn icon(&self) -> &'static str {
        match self {
            Self::Read => "○",
            Self::Edit => "●",
            Self::Command => "▸",
            Self::Info => "·",
            Self::Warning => "⚠",
            Self::Error => "✗",
            Self::Approval => "?",
            Self::Plan => "◆",
        }
    }

    fn color(&self) -> &'static str {
        match self {
            Self::Read => DIM,
            Self::Edit => GREEN,
            Self::Command => CYAN,
            Self::Info => DIM,
            Self::Warning => YELLOW,
            Self::Error => RED,
            Self::Approval => YELLOW,
            Self::Plan => BLUE,
        }
    }
}

// ─── PipitUi ─────────────────────────────────────────────────────────────────

/// The TUI — task-first agent interface.
///
/// Layout:
/// 1. Persistent status bar (top)
/// 2. Task + plan display
/// 3. Activity stream (concise action cards)
/// 4. Content output (markdown-rendered agent responses)
/// 5. Composer prompt (bottom)
pub struct PipitUi {
    show_thinking: bool,
    show_token_usage: bool,
    show_trace: bool,

    // Inline state
    in_thinking: bool,
    in_tool: bool,
    in_content: bool,
    active_turn: Option<u32>,
    tool_calls_this_turn: usize,

    // Persistent state
    status: StatusBarState,
    activity: Vec<ActivityEntry>,
    current_task: Option<String>,
    current_plan: Vec<String>,
}

impl PipitUi {
    pub fn new(
        show_thinking: bool,
        show_token_usage: bool,
        show_trace: bool,
        status: StatusBarState,
    ) -> Self {
        Self {
            show_thinking,
            show_token_usage,
            show_trace,
            in_thinking: false,
            in_tool: false,
            in_content: false,
            active_turn: None,
            tool_calls_this_turn: 0,
            status,
            activity: Vec::new(),
            current_task: None,
            current_plan: Vec::new(),
        }
    }

    /// Access mutable status for external updates.
    pub fn status_mut(&mut self) -> &mut StatusBarState {
        &mut self.status
    }

    /// Set the current task description.
    pub fn set_task(&mut self, task: String) {
        self.current_task = Some(task);
    }

    /// Set the current plan steps.
    pub fn set_plan(&mut self, steps: Vec<String>) {
        self.current_plan = steps;
    }

    // ── Rendering ────────────────────────────────────────────────────────

    /// Render the status bar to stderr.
    pub fn render_status_bar(&self) {
        let term_width = terminal_width();
        let branch_marker = if self.status.dirty { "*" } else { "" };
        let token_pct = self.status.token_pct();
        let verification_label = match &self.status.verification {
            VerificationState::Unknown => format!("{}—{}", DIM, RESET),
            VerificationState::Passing => format!("{}✓ pass{}", GREEN, RESET),
            VerificationState::Failing(msg) => format!("{}✗ {}{}", RED, truncate(msg, 12), RESET),
            VerificationState::Running => format!("{}… running{}", YELLOW, RESET),
        };

        // Line 1: repo, branch, model, mode
        let line1 = format!(
            " pipit · {repo}  {branch}{dirty}  {model}  {mode}",
            repo = self.status.repo_name,
            branch = self.status.branch,
            dirty = branch_marker,
            model = self.status.model,
            mode = self.status.approval_mode.label(),
        );

        // Line 2: sandbox, tokens, cost, verification
        let line2 = format!(
            " tokens: {pct}%  ${cost:.4}  {verify}",
            pct = token_pct,
            cost = self.status.cost,
            verify = verification_label,
        );

        // Draw horizontal rule + bar
        let rule = "─".repeat(term_width.min(120));
        eprintln!("{DIM}┌{rule}┐{RESET}");
        eprintln!("{BG_DARK}{FG_WHITE}{}{RESET}", pad_to(&line1, term_width.min(120)));
        eprintln!("{BG_DARK}{FG_GRAY}{}{RESET}", pad_to(&line2, term_width.min(120)));
        eprintln!("{DIM}└{rule}┘{RESET}");
    }

    /// Render the task + plan block (if set).
    pub fn render_task_block(&self) {
        if let Some(task) = &self.current_task {
            eprintln!("{BOLD}Task{RESET}");
            eprintln!("  {}", task);
            eprintln!();
        }
        if !self.current_plan.is_empty() {
            eprintln!("{BOLD}Plan{RESET}");
            for (i, step) in self.current_plan.iter().enumerate() {
                eprintln!("  {}. {}", i + 1, step);
            }
            eprintln!();
        }
    }

    /// Render the recent activity stream.
    pub fn render_activity(&self, last_n: usize) {
        if self.activity.is_empty() {
            return;
        }
        let start = self.activity.len().saturating_sub(last_n);
        for entry in &self.activity[start..] {
            eprintln!(
                "  {color}{icon}{RESET} {text}",
                color = entry.kind.color(),
                icon = entry.kind.icon(),
                text = entry.text,
            );
        }
    }

    /// Print the full header on session start.
    pub fn print_header(&self) {
        self.render_status_bar();
        eprintln!();
        eprintln!(
            "{BOLD_CYAN}pipit{RESET} {DIM}v0.1.0 | /help for commands | {mode} mode{RESET}",
            mode = self.status.approval_mode.label(),
        );
        if !self.show_trace {
            eprintln!("{DIM}compact mode on | pass --trace-ui for detailed traces{RESET}");
        }
        eprintln!();
    }

    /// Print the input prompt (composer).
    pub fn print_prompt(&self) {
        eprint!(
            "{BOLD_GREEN}you›{RESET} ",
        );
        let _ = io::stderr().flush();
    }

    /// Print a help banner with the interaction grammar.
    pub fn print_help() {
        eprintln!(
            r#"
{BOLD}Commands{RESET}
  /help, /h            Show this help
  /status              Show session state and workflow assets
  /plans               Show ranked plans and pivot history
  /context             View current working set and token usage
  /tokens              Show context pressure and cost
  /permissions         Show approval modes
  /permissions <mode>  Switch mode (plan, edit, cmd, full)
  /plan [topic]        Discuss before editing (no changes)
  /tdd [topic]         Test-driven development workflow
  /code-review         Review uncommitted changes (severity-categorized)
  /build-fix           Fix build errors incrementally
  /add <file>          Add file to working set
  /drop <file>         Remove file from working set
  /compact             Compress context history
  /verify [scope]      Run verification (quick, full, pre-commit)
  /aside <question>    Quick question without losing task context
  /checkpoint [action] Create/restore/list git checkpoints
  /save-session [name] Save session for later resumption
  /resume-session [n]  Resume or list saved sessions
  /clear, /c           Clear conversation context
  /cost                Show token usage and cost
  /quit, /q            Exit pipit

{BOLD}Grammar{RESET}
  /command           Control the agent
  @file              Include file or folder in context
  !command           Shell passthrough
  Tab                Autocomplete
  Esc Esc            Rewind / history
  ?                  Keyboard map
"#
        );
    }

    /// Print the permissions panel.
    pub fn print_permissions(&self) {
        eprintln!("{BOLD}Approval Modes{RESET}");
        let modes = [
            (ApprovalMode::Suggest, "Read-only; all writes and commands need approval"),
            (ApprovalMode::AutoEdit, "File edits need approval; reads are free"),
            (ApprovalMode::CommandReview, "Shell commands need approval; edits are free"),
            (ApprovalMode::FullAuto, "No routine prompts in trusted folders"),
        ];
        for (mode, desc) in &modes {
            let marker = if *mode == self.status.approval_mode {
                format!("{GREEN}▸{RESET}")
            } else {
                format!(" ")
            };
            eprintln!("  {} {BOLD}{:<14}{RESET} {DIM}{}{RESET}", marker, mode.label(), desc);
        }
        eprintln!();
        eprintln!("{DIM}Switch with: /permissions <mode>{RESET}");
    }

    /// Print a context/working-set summary.
    pub fn print_context_summary(&self, files: &[String], token_usage: u64, token_limit: u64) {
        eprintln!("{BOLD}Working Set{RESET}");
        if files.is_empty() {
            eprintln!("  {DIM}(no files in context){RESET}");
        } else {
            for f in files {
                eprintln!("  {DIM}○{RESET} {}", f);
            }
        }
        eprintln!();
        let pct = if token_limit > 0 {
            (token_usage * 100) / token_limit
        } else {
            0
        };
        let bar = render_bar(pct as usize, 30);
        eprintln!("  tokens: {} / {} ({}%)", token_usage, token_limit, pct);
        eprintln!("  {}", bar);
    }

    // ── Approval preview cards ───────────────────────────────────────────

    /// Render a diff approval card (for edit_file / write_file).
    pub fn print_diff_approval(name: &str, path: &str, diff: &str) {
        let rule = "─".repeat(60);
        eprintln!();
        eprintln!("{YELLOW}┌{rule}{RESET}");
        eprintln!("{YELLOW}│ ⚠  Approve {name}: {path}{RESET}");
        eprintln!("{YELLOW}├{rule}{RESET}");
        for line in diff.lines() {
            let colored = if line.starts_with('+') {
                format!("{GREEN}{}{RESET}", line)
            } else if line.starts_with('-') {
                format!("{RED}{}{RESET}", line)
            } else {
                format!("{DIM}{}{RESET}", line)
            };
            eprintln!("{YELLOW}│{RESET} {}", colored);
        }
        eprintln!("{YELLOW}└{rule}{RESET}");
        eprint!("{YELLOW}  [y]es / [n]o / [e]dit >{RESET} ");
        let _ = io::stderr().flush();
    }

    /// Render a command approval card (for bash).
    pub fn print_command_approval(command: &str) {
        let rule = "─".repeat(60);
        eprintln!();
        eprintln!("{YELLOW}┌{rule}{RESET}");
        eprintln!("{YELLOW}│ ⚠  Approve command{RESET}");
        eprintln!("{YELLOW}├{rule}{RESET}");
        eprintln!("{YELLOW}│{RESET} {BOLD}$ {}{RESET}", command);
        eprintln!("{YELLOW}└{rule}{RESET}");
        eprint!("{YELLOW}  [y]es / [n]o >{RESET} ");
        let _ = io::stderr().flush();
    }

    /// Render a generic tool approval card.
    pub fn print_tool_approval(name: &str, args: &serde_json::Value) {
        let rule = "─".repeat(60);
        eprintln!();
        eprintln!("{YELLOW}┌{rule}{RESET}");
        eprintln!("{YELLOW}│ ⚠  Approve {name}{RESET}");
        eprintln!("{YELLOW}├{rule}{RESET}");
        // Print args compactly
        if let Ok(pretty) = serde_json::to_string_pretty(args) {
            for line in pretty.lines().take(20) {
                eprintln!("{YELLOW}│{RESET} {DIM}{}{RESET}", line);
            }
        }
        eprintln!("{YELLOW}└{rule}{RESET}");
        eprint!("{YELLOW}  [y]es / [n]o >{RESET} ");
        let _ = io::stderr().flush();
    }

    // ── Event handling ───────────────────────────────────────────────────

    /// Process an agent event and render to terminal.
    pub fn handle_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::TurnStart { turn_number } => {
                self.finish_inline_sections();
                self.active_turn = Some(*turn_number);
                self.tool_calls_this_turn = 0;
                if *turn_number > 0 {
                    eprintln!();
                    eprintln!(
                        "{BOLD_BLUE}── Turn {turn_number}{RESET} {DIM}planning + execution{RESET}"
                    );
                }
            }
            AgentEvent::ContentDelta { text } => {
                if self.in_thinking {
                    self.in_thinking = false;
                    eprintln!("{RESET}");
                }
                // Strip thinking tags that leak from some providers
                let cleaned = text
                    .replace("</think>", "")
                    .replace("<think>", "");
                if cleaned.trim().is_empty() && text.contains("think>") {
                    // Pure thinking tag, skip entirely
                    return;
                }
                if !self.in_content {
                    self.in_content = true;
                    print!("{BOLD_CYAN}pipit›{RESET} ");
                }
                print!("{}", cleaned);
                let _ = io::stdout().flush();
            }
            AgentEvent::ThinkingDelta { text } => {
                if self.show_thinking {
                    if self.in_content {
                        self.in_content = false;
                        println!();
                    }
                    if !self.in_thinking {
                        self.in_thinking = true;
                        eprint!("{DIM}thinking› ");
                    }
                    eprint!("{}", text);
                    let _ = io::stderr().flush();
                }
            }
            AgentEvent::ContentComplete { .. } => {
                if self.in_content {
                    self.in_content = false;
                    println!();
                }
            }
            AgentEvent::ToolCallStart {
                call_id: _,
                name,
                args,
            } => {
                self.finish_inline_sections();
                self.in_tool = true;
                self.tool_calls_this_turn += 1;

                let summary = tool_summary(name, args);
                let kind = tool_activity_kind(name);
                self.push_activity(kind, summary.clone());

                if self.show_trace {
                    eprintln!("{DIM}tool› {name} {}{RESET}", truncate(&summary, 96));
                }
            }
            AgentEvent::ToolCallEnd {
                call_id: _,
                name,
                result,
            } => {
                self.finish_inline_sections();
                self.in_tool = false;

                match result {
                    pipit_core::ToolCallOutcome::Success { content, mutated } => {
                        if *mutated {
                            self.push_activity(
                                ActivityKind::Edit,
                                format!("{} ✓ modified", name),
                            );
                            eprintln!("{GREEN}  ● {name} updated files{RESET}");
                        } else if self.show_trace {
                            let preview = truncate(content, 120);
                            eprintln!("{DIM}  ○ {name} ok | {preview}{RESET}");
                        }
                    }
                    pipit_core::ToolCallOutcome::PolicyBlocked { message, .. } => {
                        self.push_activity(
                            ActivityKind::Warning,
                            format!("{} blocked: {}", name, truncate(message, 60)),
                        );
                        eprintln!(
                            "{YELLOW}  ⚠ {name} blocked | {}{RESET}",
                            truncate(message, 120)
                        );
                    }
                    pipit_core::ToolCallOutcome::Error { message } => {
                        self.push_activity(
                            ActivityKind::Error,
                            format!("{} failed: {}", name, truncate(message, 60)),
                        );
                        eprintln!(
                            "{RED}  ✗ {name} failed | {}{RESET}",
                            truncate(message, 120)
                        );
                    }
                }
            }
            AgentEvent::ToolApprovalNeeded {
                call_id: _,
                name,
                args: _,
            } => {
                self.push_activity(
                    ActivityKind::Approval,
                    format!("Waiting for approval: {}", name),
                );
                // The actual approval card is rendered by the ApprovalHandler,
                // not the event subscriber, to avoid duplicate rendering.
            }
            AgentEvent::CompressionStart => {
                self.finish_inline_sections();
                self.push_activity(ActivityKind::Info, "Compressing context…".to_string());
                if self.show_trace {
                    eprintln!("{DIM}context› compressing conversation history{RESET}");
                }
            }
            AgentEvent::CompressionEnd {
                messages_removed,
                tokens_freed,
            } => {
                self.push_activity(
                    ActivityKind::Info,
                    format!("Compressed: {} msgs, ~{} tokens freed", messages_removed, tokens_freed),
                );
                if self.show_trace {
                    eprintln!(
                        "{DIM}context› removed {} messages, freed ~{} tokens{RESET}",
                        messages_removed, tokens_freed
                    );
                }
            }
            AgentEvent::TokenUsageUpdate { used, limit, cost } => {
                self.status.tokens_used = *used;
                self.status.tokens_limit = *limit;
                self.status.cost = *cost;

                if self.show_token_usage && self.show_trace {
                    eprintln!(
                        "{DIM}usage› tokens {used}/{limit} | ${cost:.4}{RESET}",
                    );
                }
            }
            AgentEvent::PlanSelected {
                strategy,
                rationale,
                pivoted,
                candidate_plans: _,
            } => {
                let prefix = if *pivoted { "Plan pivot" } else { "Plan" };
                self.push_activity(
                    ActivityKind::Plan,
                    format!("{}: {} — {}", prefix, strategy, rationale),
                );
                eprintln!(
                    "{BOLD_BLUE}{} › {}{RESET} {DIM}{}{RESET}",
                    prefix.to_lowercase(),
                    strategy,
                    rationale
                );
            }
            AgentEvent::ProviderError { error, will_retry } => {
                self.finish_inline_sections();
                let rendered = concise_provider_error(error, *will_retry, self.show_trace);
                if *will_retry {
                    self.push_activity(
                        ActivityKind::Warning,
                        format!("provider: {}", truncate(&rendered, 60)),
                    );
                    eprintln!("{YELLOW}provider› {rendered}{RESET}");
                } else {
                    self.push_activity(
                        ActivityKind::Error,
                        format!("provider: {}", truncate(&rendered, 60)),
                    );
                    eprintln!("{RED}provider› {rendered}{RESET}");
                }
            }
            AgentEvent::ToolError { call_id: _, error } => {
                self.finish_inline_sections();
                self.push_activity(
                    ActivityKind::Error,
                    format!("tool error: {}", truncate(error, 60)),
                );
                eprintln!("{RED}tool› {error}{RESET}");
            }
            AgentEvent::LoopDetected { tool_name, count } => {
                self.finish_inline_sections();
                self.push_activity(
                    ActivityKind::Warning,
                    format!("{} repeated {} times", tool_name, count),
                );
                eprintln!("{YELLOW}loop› {tool_name} repeated {count} times{RESET}");
            }
            AgentEvent::SteeringMessageInjected { text } => {
                if self.show_trace {
                    eprintln!("{DIM}steering› {text}{RESET}");
                }
            }
            AgentEvent::TurnEnd {
                turn_number,
                reason,
            } => {
                self.finish_inline_sections();
                let reason_label = match reason {
                    pipit_core::TurnEndReason::Complete => "done",
                    pipit_core::TurnEndReason::ToolsExecuted => "tools executed",
                    pipit_core::TurnEndReason::MaxTurns => "max turns",
                    pipit_core::TurnEndReason::Error => "error",
                    pipit_core::TurnEndReason::Cancelled => "cancelled",
                };
                let tools = self.tool_calls_this_turn;
                eprintln!(
                    "{DIM}turn {turn_number} · {tools} tool(s) · {reason_label}{RESET}"
                );
            }
        }
    }

    // ── Private helpers ──────────────────────────────────────────────────

    fn push_activity(&mut self, kind: ActivityKind, text: String) {
        self.activity.push(ActivityEntry { kind, text });
        // Keep bounded
        if self.activity.len() > 200 {
            self.activity.drain(..50);
        }
    }

    fn finish_inline_sections(&mut self) {
        if self.in_thinking {
            self.in_thinking = false;
            eprintln!("{RESET}");
        }
        if self.in_content {
            self.in_content = false;
            println!();
        }
    }
}

// ─── Free helpers ────────────────────────────────────────────────────────────

fn truncate(text: &str, max_chars: usize) -> String {
    // Strip newlines for single-line display
    let flat: String = text.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if flat.chars().count() <= max_chars {
        flat
    } else {
        format!("{}…", flat.chars().take(max_chars).collect::<String>())
    }
}

fn concise_provider_error(error: &str, will_retry: bool, show_trace: bool) -> String {
    if show_trace {
        return error.to_string();
    }
    let first_line = error.lines().next().unwrap_or(error).trim();
    if will_retry {
        format!("{}; retrying", first_line)
    } else {
        first_line.to_string()
    }
}

/// Summarize tool args for the activity stream.
fn tool_summary(name: &str, args: &serde_json::Value) -> String {
    match name {
        "read_file" => {
            let path = args["path"].as_str().unwrap_or("?");
            format!("Read {}", path)
        }
        "write_file" => {
            let path = args["path"].as_str().unwrap_or("?");
            format!("Write {}", path)
        }
        "edit_file" => {
            let path = args["path"].as_str().unwrap_or("?");
            format!("Edit {}", path)
        }
        "bash" => {
            let cmd = args["command"].as_str().unwrap_or("");
            format!("$ {}", truncate(cmd, 80))
        }
        "grep" => {
            let pattern = args["pattern"].as_str().unwrap_or("?");
            format!("Grep '{}'", truncate(pattern, 40))
        }
        "glob" => {
            let pattern = args["pattern"].as_str().unwrap_or("?");
            format!("Glob '{}'", truncate(pattern, 40))
        }
        "list_directory" => {
            let path = args["path"].as_str().unwrap_or(".");
            format!("List {}", path)
        }
        _ => {
            format!("{} …", name)
        }
    }
}

/// Map tool name to activity kind for icon/color rendering.
fn tool_activity_kind(name: &str) -> ActivityKind {
    match name {
        "read_file" | "grep" | "glob" | "list_directory" => ActivityKind::Read,
        "edit_file" | "write_file" => ActivityKind::Edit,
        "bash" => ActivityKind::Command,
        _ => ActivityKind::Info,
    }
}

/// Generate a simple inline diff from search/replace strings.
fn format_inline_diff(search: &str, replace: &str) -> String {
    let mut diff = String::new();
    for line in search.lines() {
        diff.push('-');
        diff.push_str(line);
        diff.push('\n');
    }
    for line in replace.lines() {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    diff
}

/// Get terminal width, defaulting to 100.
fn terminal_width() -> usize {
    // Try the COLUMNS env var first, then crossterm
    if let Ok(cols) = std::env::var("COLUMNS") {
        if let Ok(n) = cols.parse::<usize>() {
            return n;
        }
    }
    crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(100)
}

/// Pad (or truncate) a string to exactly `width` visible characters.
fn pad_to(s: &str, width: usize) -> String {
    // Count visible chars (skip ANSI escape sequences)
    let visible_len = visible_char_count(s);
    if visible_len >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - visible_len))
    }
}

/// Count visible characters, skipping ANSI escape codes.
fn visible_char_count(s: &str) -> usize {
    let mut count = 0;
    let mut in_escape = false;
    for c in s.chars() {
        if in_escape {
            if c.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else if c == '\x1b' {
            in_escape = true;
        } else {
            count += 1;
        }
    }
    count
}

/// Render a simple text progress bar.
fn render_bar(pct: usize, width: usize) -> String {
    let filled = (pct * width) / 100;
    let empty = width.saturating_sub(filled);
    let color = if pct > 85 {
        RED
    } else if pct > 60 {
        YELLOW
    } else {
        GREEN
    };
    format!(
        "{color}[{}{}]{RESET} {}%",
        "█".repeat(filled),
        "░".repeat(empty),
        pct
    )
}

/// Detect current git branch.
fn detect_git_branch() -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Detect if the working directory is dirty.
fn detect_git_dirty() -> bool {
    std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

// ─── Interactive approval handler ────────────────────────────────────────────

use async_trait::async_trait;
use pipit_core::{ApprovalDecision, ApprovalHandler};

/// Interactive approval handler that renders approval cards to stderr
/// and reads the user's decision from stdin.
pub struct InteractiveApprovalHandler;

#[async_trait]
impl ApprovalHandler for InteractiveApprovalHandler {
    async fn request_approval(
        &self,
        _call_id: &str,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> ApprovalDecision {
        // Render the appropriate approval card
        match tool_name {
            "bash" => {
                let cmd = args["command"].as_str().unwrap_or("(unknown)");
                PipitUi::print_command_approval(cmd);
            }
            "edit_file" => {
                let path = args["path"].as_str().unwrap_or("?");
                let search = args["search"].as_str().unwrap_or("");
                let replace = args["replace"].as_str().unwrap_or("");
                let diff = format_inline_diff(search, replace);
                PipitUi::print_diff_approval(tool_name, path, &diff);
            }
            "write_file" => {
                let path = args["path"].as_str().unwrap_or("?");
                let content = args["content"].as_str().unwrap_or("");
                let preview = truncate(content, 500);
                let diff = format!("+{}", preview.replace('\n', "\n+"));
                PipitUi::print_diff_approval(tool_name, path, &diff);
            }
            _ => {
                PipitUi::print_tool_approval(tool_name, args);
            }
        }

        // Read decision from stdin (blocking via tokio::task::spawn_blocking)
        let decision = tokio::task::spawn_blocking(|| {
            let mut input = String::new();
            match std::io::stdin().read_line(&mut input) {
                Ok(_) => {
                    let trimmed = input.trim().to_lowercase();
                    match trimmed.as_str() {
                        "y" | "yes" | "" => ApprovalDecision::Approve,
                        _ => ApprovalDecision::Deny,
                    }
                }
                Err(_) => ApprovalDecision::Deny,
            }
        })
        .await
        .unwrap_or(ApprovalDecision::Deny);

        // Echo the decision
        match &decision {
            ApprovalDecision::Approve => {
                eprintln!("{GREEN}  ✓ approved{RESET}");
            }
            ApprovalDecision::Deny => {
                eprintln!("{RED}  ✗ denied{RESET}");
            }
        }

        decision
    }
}

#[cfg(test)]
mod tests {
    use super::{concise_provider_error, truncate, visible_char_count, render_bar, pad_to};

    #[test]
    fn truncate_short_text_is_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_text_adds_ellipsis() {
        assert_eq!(truncate("abcdefgh", 5), "abcde…");
    }

    #[test]
    fn truncate_replaces_newlines() {
        assert_eq!(truncate("a\nb\nc", 10), "a b c");
    }

    #[test]
    fn concise_provider_error_hides_multiline_trace_in_compact_mode() {
        let rendered = concise_provider_error(
            "Request too large\nPlease check this guide\nhttps://example.com",
            true,
            false,
        );
        assert_eq!(rendered, "Request too large; retrying");
    }

    #[test]
    fn visible_char_count_strips_ansi() {
        assert_eq!(visible_char_count("\x1b[31mhello\x1b[0m"), 5);
    }

    #[test]
    fn pad_to_extends_short_string() {
        assert_eq!(pad_to("hi", 5), "hi   ");
    }

    #[test]
    fn render_bar_shows_percentage() {
        let bar = render_bar(50, 10);
        assert!(bar.contains("50%"));
    }
}
