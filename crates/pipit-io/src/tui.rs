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
#[allow(dead_code)]
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
    /// Extra fields for /config display
    pub provider_kind: String,
    pub base_url: String,
    pub agent_mode: String,
    pub max_turns: u32,
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
            provider_kind: String::new(),
            base_url: String::new(),
            agent_mode: String::new(),
            max_turns: 25,
        }
    }

    pub fn token_pct(&self) -> u64 {
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

/// The TUI — agent interface.
///
/// Layout:
/// 1. Persistent status bar (top)
/// 2. Plan display
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
        eprintln!(
            "{BG_DARK}{FG_WHITE}{}{RESET}",
            pad_to(&line1, term_width.min(120))
        );
        eprintln!(
            "{BG_DARK}{FG_GRAY}{}{RESET}",
            pad_to(&line2, term_width.min(120))
        );
        eprintln!("{DIM}└{rule}┘{RESET}");
    }

    /// Render the plan block (if set).
    pub fn render_task_block(&self) {
        if let Some(task) = &self.current_task {
            eprintln!("{BOLD}Objective{RESET}");
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
        eprint!("{BOLD_GREEN}you›{RESET} ",);
        let _ = io::stderr().flush();
    }

    /// Print a help banner with the interaction grammar.
    pub fn print_help() {
        eprintln!(
            r#"
{BOLD}Commands{RESET}
  {BOLD}Navigation{RESET}
  /help, /h            Show this help
  /status              Show session state and workflow assets
  /cost                Show token usage and cost
  /clear, /c           Clear conversation context
  /quit, /q            Exit pipit

  {BOLD}Configuration{RESET}
  /config              Show current configuration
  /setup               Run setup wizard
  /model <name>        Switch model at runtime
  /provider [name]     Switch provider/model profile (or list all)
  /permissions <mode>  Switch approval mode (suggest/auto_edit/full_auto)

  {BOLD}Context{RESET}
  /context, /ctx       View current working set
  /tokens, /tok        Show context pressure and token usage
  /compact             Compress context history
  /add <file>          Add file to working set
  /drop <file>         Remove file from working set

  {BOLD}Git & Version Control{RESET}
  /diff                Show uncommitted changes
  /commit [msg]        Commit with AI-generated message
  /undo                Undo last agent edits
  /branch [name]       Create branch or show current
  /branches            List all branches
  /switch <branch>     Switch branch

  {BOLD}Workflows{RESET}
  /plan [topic]        Discuss before editing (plan-first mode)
  /verify [scope]      Run verification (quick, full, pre-commit)
  /aside <question>    Quick question without losing task context
  /spec [file]         Spec-driven development
  /tdd [topic]         Test-driven development workflow
  /code-review         Review uncommitted changes
  /build-fix           Fix build errors incrementally
  /search <query>      Search codebase
  /loop [N] <prompt>   Re-run prompt every N seconds
  /bg <prompt>         Background task via daemon

  {BOLD}Session & Memory{RESET}
  /save [name]         Save session for later resumption
  /resume [name]       Resume or list saved sessions
  /memory [add|list]   Persistent cross-session knowledge
  /checkpoint [action] Create/restore/list git checkpoints

  {BOLD}System{RESET}
  /doctor              System health check
  /skills              List available skills
  /hooks               List active hooks
  /mcp                 MCP server status
  /deps                Dependency health scan

  {BOLD}Advanced{RESET}
  /bench [run|list]    Benchmark runner
  /browse <url>        Headless browser testing
  /mesh [status|join]  Distributed mesh management
  /watch [start|stop]  Ambient file watcher

{BOLD}Grammar{RESET}
  /command           Control the agent
  @file              Include file or folder in context
  !command           Run shell command directly (no AI)
  Tab                Autocomplete
  Ctrl-J             Insert newline (multiline input)
  ↑ ↓                History recall
"#
        );
    }

    /// Print the permissions panel.
    pub fn print_permissions(&self) {
        eprintln!("{BOLD}Approval Modes{RESET}");
        let modes = [
            (
                ApprovalMode::Suggest,
                "Read-only; all writes and commands need approval",
            ),
            (
                ApprovalMode::AutoEdit,
                "File edits need approval; reads are free",
            ),
            (
                ApprovalMode::CommandReview,
                "Shell commands need approval; edits are free",
            ),
            (
                ApprovalMode::FullAuto,
                "No routine prompts in trusted folders",
            ),
        ];
        for (mode, desc) in &modes {
            let marker = if *mode == self.status.approval_mode {
                format!("{GREEN}▸{RESET}")
            } else {
                format!(" ")
            };
            eprintln!(
                "  {} {BOLD}{:<14}{RESET} {DIM}{}{RESET}",
                marker,
                mode.label(),
                desc
            );
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
                // Subtle separator between turns (skip turn 0)
                if *turn_number > 0 {
                    eprintln!();
                }
            }
            AgentEvent::ContentDelta { text } => {
                if self.in_thinking {
                    self.in_thinking = false;
                    eprintln!("{RESET}");
                }
                // Strip thinking tags that leak from some providers
                let cleaned = text.replace("</think>", "").replace("<think>", "");
                if cleaned.trim().is_empty() && text.contains("think>") {
                    return;
                }
                if !self.in_content {
                    self.in_content = true;
                    eprint!("{BOLD_CYAN}●{RESET} ");
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
                        eprint!("{DIM}");
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

                // Show tool invocation with a bold action label like Codex
                let label = tool_action_label(name, args);
                eprintln!("{CYAN}●{RESET} {BOLD}{}{RESET}", label);
            }
            AgentEvent::ToolCallEnd {
                call_id: _,
                name,
                result,
                duration_ms,
            } => {
                self.finish_inline_sections();
                self.in_tool = false;

                let timing = format_duration(*duration_ms);

                match result {
                    pipit_core::ToolCallOutcome::Success {
                        content, mutated, ..
                    } => {
                        if *mutated {
                            let (icon, color) = if content.starts_with("Created") {
                                ("+", GREEN)
                            } else if content.starts_with("Updated") {
                                ("~", YELLOW)
                            } else {
                                ("●", GREEN)
                            };
                            let first_line = content.lines().next().unwrap_or(content);
                            self.push_activity(ActivityKind::Edit, content.clone());
                            eprintln!(
                                "  {color}{icon} {}{RESET} {DIM}{timing}{RESET}",
                                truncate(first_line, 80)
                            );
                        } else {
                            let line_count = content.lines().count();
                            match name.as_str() {
                                "bash" => {
                                    // Inline output with tree connector
                                    // Skip boilerplate "no output" messages
                                    let is_noise = content.trim().is_empty()
                                        || content.contains("Command completed successfully")
                                        || content.contains("(no output)");
                                    if !is_noise {
                                        let lines: Vec<&str> = content.lines().collect();
                                        let show = lines.len().min(5);
                                        if show > 0 {
                                            for (i, line) in lines[..show].iter().enumerate() {
                                                let connector = if i == show - 1 && lines.len() <= show {
                                                    "└"
                                                } else {
                                                    "├"
                                                };
                                                eprintln!(
                                                    "{DIM}  {connector} {}{RESET}",
                                                    truncate(line, 90)
                                                );
                                            }
                                            if lines.len() > show {
                                                eprintln!(
                                                    "{DIM}  └ … {} more lines{RESET}",
                                                    lines.len() - show
                                                );
                                            }
                                        }
                                    }
                                    self.push_activity(
                                        ActivityKind::Command,
                                        format!("bash → {} lines", line_count),
                                    );
                                }
                                _ => {
                                    // Read/grep/glob/list — the ToolCallStart label
                                    // already shows what happened; no extra output needed.
                                    self.push_activity(
                                        tool_activity_kind(name),
                                        format!("{} → done", name),
                                    );
                                }
                            }
                        }
                    }
                    pipit_core::ToolCallOutcome::PolicyBlocked { message, .. } => {
                        self.push_activity(
                            ActivityKind::Warning,
                            format!("{} blocked: {}", name, truncate(message, 60)),
                        );
                        eprintln!(
                            "{YELLOW}  └ blocked: {}{RESET}",
                            truncate(message, 100)
                        );
                    }
                    pipit_core::ToolCallOutcome::Error { message } => {
                        self.push_activity(
                            ActivityKind::Error,
                            format!("{} failed: {}", name, truncate(message, 60)),
                        );
                        eprintln!(
                            "{RED}  └ error: {}{RESET}",
                            truncate(message, 100)
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
                    format!(
                        "Compressed: {} msgs, ~{} tokens freed",
                        messages_removed, tokens_freed
                    ),
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
                    eprintln!("{DIM}usage› tokens {used}/{limit} | ${cost:.4}{RESET}",);
                }
            }
            AgentEvent::PlanSelected {
                strategy,
                rationale,
                pivoted,
                candidate_plans: _,
            } => {
                let prefix = if *pivoted { "Pivoting" } else { "Plan" };
                self.push_activity(
                    ActivityKind::Plan,
                    format!("{}: {} — {}", prefix, strategy, rationale),
                );
                eprintln!(
                    "{BLUE}●{RESET} {BOLD}{prefix}: {strategy}{RESET} {DIM}{rationale}{RESET}"
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
            AgentEvent::PhaseTransition { from: _, to, mode } => {
                self.finish_inline_sections();
                eprintln!("{DIM}● {mode} · {to}{RESET}");
            }
            AgentEvent::VerifierVerdict {
                verdict,
                confidence,
                findings_summary,
            } => {
                self.finish_inline_sections();
                let color = match verdict.as_str() {
                    "PASS" => GREEN,
                    "REPAIRABLE" => YELLOW,
                    _ => RED,
                };
                eprintln!(
                    "{color}verify› {verdict}{RESET} {DIM}(confidence: {confidence:.0}%){RESET}"
                );
                if !findings_summary.is_empty() {
                    for line in findings_summary.lines() {
                        eprintln!("  {DIM}{line}{RESET}");
                    }
                }
            }
            AgentEvent::RepairStarted { attempt, reason } => {
                self.finish_inline_sections();
                eprintln!("{YELLOW}repair› attempt {attempt}: {reason}{RESET}");
            }
            AgentEvent::Waiting { label } => {
                eprintln!("{DIM}{label}{RESET}");
            }
            AgentEvent::TurnEnd {
                turn_number: _,
                reason,
            } => {
                self.finish_inline_sections();
                // Only show end-of-turn for noteworthy reasons
                match reason {
                    pipit_core::TurnEndReason::Complete => {
                        // Silent — content already visible
                    }
                    pipit_core::TurnEndReason::ToolsExecuted => {
                        // Silent — tool results already shown inline
                    }
                    pipit_core::TurnEndReason::MaxTurns => {
                        eprintln!("{YELLOW}● Reached maximum turns{RESET}");
                    }
                    pipit_core::TurnEndReason::Error => {
                        eprintln!("{RED}● Turn ended with error{RESET}");
                    }
                    pipit_core::TurnEndReason::Cancelled => {
                        eprintln!("{DIM}● Cancelled{RESET}");
                    }
                }
            }
            AgentEvent::TurnPhaseEntered {
                turn: _,
                phase,
                detail,
                ..
            } => {
                // Live turn trace: show canonical phase transitions in activity feed
                let detail_str = detail.as_deref().unwrap_or("");
                let label = if detail_str.is_empty() {
                    format!("phase› {phase}")
                } else {
                    format!("phase› {phase} ({detail_str})")
                };
                self.push_activity(ActivityKind::Info, label);
            }
            AgentEvent::BudgetExtended { new_approved } => {
                self.push_activity(
                    ActivityKind::Info,
                    format!("budget extended to {} turns", new_approved),
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
    let flat: String = text
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    if flat.chars().count() <= max_chars {
        flat
    } else {
        format!("{}…", flat.chars().take(max_chars).collect::<String>())
    }
}

/// Format a duration in milliseconds for compact display.
fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

fn concise_provider_error(error: &str, will_retry: bool, show_trace: bool) -> String {
    if show_trace {
        return error.to_string();
    }

    // Extract a friendly message from common provider error patterns
    let friendly = parse_friendly_error(error);

    if will_retry {
        format!("{}; retrying", friendly)
    } else {
        friendly
    }
}

/// Parse JSON error bodies and HTTP error strings into user-friendly messages.
fn parse_friendly_error(error: &str) -> String {
    let lower = error.to_ascii_lowercase();

    // Try to extract JSON error message: {"error":{"message":"..."}}
    if let Some(msg_start) = error.find("\"message\"") {
        let after = &error[msg_start + 9..];
        // Find the value after "message":"
        if let Some(colon) = after.find(':') {
            let value_part = after[colon + 1..].trim().trim_start_matches('"');
            if let Some(end_quote) = value_part.find('"') {
                let extracted = &value_part[..end_quote];
                if !extracted.is_empty() {
                    return format_error_category(extracted);
                }
            }
        }
    }

    // Pattern-based friendly messages
    if lower.contains("can only get item") || lower.contains("tool_call") {
        return "Model returned malformed tool calls. This model may have limited tool-use support.".to_string();
    }
    if lower.contains("context length") || lower.contains("too many tokens") || lower.contains("too long") {
        return "Request too large for model context window. Try a shorter prompt or smaller project.".to_string();
    }
    if lower.contains("rate limit") || lower.contains("429") {
        return "Rate limited by provider. Will retry after cooldown.".to_string();
    }
    if lower.contains("authentication") || lower.contains("401") || lower.contains("api key") {
        return "Authentication failed. Check your API key or credentials.".to_string();
    }
    if lower.contains("model not found") || lower.contains("404") {
        return "Model not found. Check the model name and provider endpoint.".to_string();
    }
    if lower.contains("500") || lower.contains("internal server") {
        return "Provider internal error (500). Will retry.".to_string();
    }
    if lower.contains("502") || lower.contains("bad gateway") {
        return "Provider gateway error (502). Server may be restarting.".to_string();
    }
    if lower.contains("503") || lower.contains("service unavailable") {
        return "Provider temporarily unavailable (503). Will retry.".to_string();
    }

    // Fallback: first line, capped at 120 chars
    let first_line = error.lines().next().unwrap_or(error).trim();
    if first_line.len() > 120 {
        format!("{}…", &first_line[..120])
    } else {
        first_line.to_string()
    }
}

fn format_error_category(msg: &str) -> String {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("can only get item") {
        "Model returned malformed tool calls. This model may have limited tool-use support.".to_string()
    } else if lower.contains("context") || lower.contains("token") {
        format!("Context limit: {}", msg)
    } else {
        // Return extracted message as-is (already more helpful than raw JSON)
        msg.to_string()
    }
}

/// Summarize tool args for the activity stream.
fn tool_summary(name: &str, args: &serde_json::Value) -> String {
    match name {
        "read_file" => {
            let path = args["path"].as_str().unwrap_or("?");
            let start = args["start_line"].as_u64();
            let end = args["end_line"].as_u64();
            match (start, end) {
                (Some(s), Some(e)) => format!("Read {} (lines {}-{})", path, s, e),
                _ => format!("Read {}", path),
            }
        }
        "write_file" => {
            let path = args["path"].as_str().unwrap_or("?");
            let content = args["content"].as_str().unwrap_or("");
            let lines = content.lines().count();
            format!("Write {} ({} lines)", path, lines)
        }
        "edit_file" => {
            let path = args["path"].as_str().unwrap_or("?");
            format!("Edit {}", path)
        }
        "multi_edit" => {
            let path = args["path"].as_str().unwrap_or("?");
            format!("MultiEdit {}", path)
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
            format!("ls {}", path)
        }
        "scaffold_project" => {
            let root = args["project_root"].as_str().unwrap_or("?");
            let file_count = args["files"].as_array().map(|a| a.len()).unwrap_or(0);
            format!("Scaffold {} ({} files)", root, file_count)
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

/// Generate a Codex-style bold action label for tool invocations.
/// e.g. "Ran pwd", "Read src/main.rs", "Searched 'pattern'"
fn tool_action_label(name: &str, args: &serde_json::Value) -> String {
    match name {
        "bash" => {
            let cmd = args["command"].as_str().unwrap_or("");
            // Show just the command name/first word bold, rest normal
            format!("Ran {}", truncate(cmd, 80))
        }
        "read_file" => {
            let path = args["path"].as_str().unwrap_or("?");
            let short = std::path::Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(path);
            let start = args["start_line"].as_u64();
            let end = args["end_line"].as_u64();
            match (start, end) {
                (Some(s), Some(e)) => format!("Read {} (lines {}-{})", short, s, e),
                _ => format!("Read {}", short),
            }
        }
        "write_file" => {
            let path = args["path"].as_str().unwrap_or("?");
            let short = std::path::Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(path);
            format!("Wrote {}", short)
        }
        "edit_file" | "multi_edit" => {
            let path = args["path"].as_str().unwrap_or("?");
            let short = std::path::Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(path);
            format!("Edited {}", short)
        }
        "grep" => {
            let pattern = args["pattern"].as_str().unwrap_or("?");
            format!("Searched '{}'", truncate(pattern, 40))
        }
        "glob" => {
            let pattern = args["pattern"].as_str().unwrap_or("?");
            format!("Glob '{}'", truncate(pattern, 40))
        }
        "list_directory" => {
            let path = args["path"].as_str().unwrap_or(".");
            format!("Listed {}", path)
        }
        "scaffold_project" => {
            let root = args["project_root"].as_str().unwrap_or("?");
            let file_count = args["files"].as_array().map(|a| a.len()).unwrap_or(0);
            format!("Scaffolded {} ({} files)", root, file_count)
        }
        _ => {
            format!("{}", name)
        }
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
    crossterm::terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(100)
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
            ApprovalDecision::ScopedGrant(grant) => {
                eprintln!(
                    "{GREEN}  ✓ approved (scoped grant, {} constraints){RESET}",
                    grant.constraints.len()
                );
            }
        }

        decision
    }
}

#[cfg(test)]
mod tests {
    use super::{concise_provider_error, pad_to, render_bar, truncate, visible_char_count};

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
            "Something went wrong\nPlease check this guide\nhttps://example.com",
            true,
            false,
        );
        assert_eq!(rendered, "Something went wrong; retrying");
    }

    #[test]
    fn concise_provider_error_parses_tool_format_error() {
        let rendered = concise_provider_error(
            r#"HTTP 400: {"error":{"message":"Can only get item property"}}"#,
            false,
            false,
        );
        assert!(rendered.contains("malformed tool calls"));
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
