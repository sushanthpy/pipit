//! Full-screen TUI mode — extracted from main.rs.
//!
//! Owns the ratatui terminal lifecycle, crossterm event dispatch,
//! agent event mapping, slash-command interpretation, and the
//! Composer-based input widget.

use anyhow::{Context, Result};
use pipit_core::{AgentLoop, AgentOutcome};
use pipit_io::StatusBarState;
use pipit_io::app::{self, TuiState};
use pipit_skills::SkillRegistry;
use ratatui::style::Color;
use regex::Regex;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio_util::sync::CancellationToken;

use crate::dbg_log;

use crate::workflow;

/// Convert a SlashCommand back to its string form for forwarding to the agent.
fn slash_command_to_str(cmd: &pipit_io::input::SlashCommand) -> String {
    use pipit_io::input::SlashCommand::*;
    match cmd {
        Help => "help".to_string(),
        Status => "status".to_string(),
        Plans => "plans".to_string(),
        Clear => "clear".to_string(),
        Model(s) => {
            if s.is_empty() {
                "model".to_string()
            } else {
                format!("model {}", s)
            }
        }
        Compact => "compact".to_string(),
        Undo => "undo".to_string(),
        Branch(Some(s)) => format!("branch {}", s),
        Branch(None) => "branch".to_string(),
        BranchList => "branches".to_string(),
        BranchSwitch(s) => format!("switch {}", s),
        Cost => "cost".to_string(),
        Quit => "quit".to_string(),
        Context => "context".to_string(),
        Tokens => "tokens".to_string(),
        Permissions(Some(s)) => format!("permissions {}", s),
        Permissions(None) => "permissions".to_string(),
        Plan(Some(s)) => format!("plan {}", s),
        Plan(None) => "plan".to_string(),
        Add(s) => format!("add {}", s),
        Drop(s) => format!("drop {}", s),
        Summarize => "summarize".to_string(),
        Rewind => "rewind".to_string(),
        Verify(Some(s)) => format!("verify {}", s),
        Verify(None) => "verify".to_string(),
        SaveSession(Some(s)) => format!("save {}", s),
        SaveSession(None) => "save".to_string(),
        ResumeSession(Some(s)) => format!("resume {}", s),
        ResumeSession(None) => "resume".to_string(),
        Aside(s) => {
            if s.is_empty() {
                "aside".to_string()
            } else {
                format!("aside {}", s)
            }
        }
        Checkpoint(Some(s)) => format!("checkpoint {}", s),
        Checkpoint(None) => "checkpoint".to_string(),
        Tdd(Some(s)) => format!("tdd {}", s),
        Tdd(None) => "tdd".to_string(),
        CodeReview => "code-review".to_string(),
        BuildFix => "build-fix".to_string(),
        Threat => "threat".to_string(),
        Evolve(Some(s)) => format!("evolve {}", s),
        Evolve(None) => "evolve".to_string(),
        Env(Some(s)) => format!("env {}", s),
        Env(None) => "env".to_string(),
        Spec(Some(s)) => format!("spec {}", s),
        Spec(None) => "spec".to_string(),
        Setup => "setup".to_string(),
        Config(Some(s)) => format!("config {}", s),
        Config(None) => "config".to_string(),
        Doctor => "doctor".to_string(),
        Skills => "skills".to_string(),
        Hooks => "hooks".to_string(),
        Mcp => "mcp".to_string(),
        Diff => "diff".to_string(),
        Commit(Some(s)) => format!("commit {}", s),
        Commit(None) => "commit".to_string(),
        Search(s) => {
            if s.is_empty() {
                "search".to_string()
            } else {
                format!("search {}", s)
            }
        }
        Loop(Some(s)) => format!("loop {}", s),
        Loop(None) => "loop".to_string(),
        Memory(Some(s)) => format!("memory {}", s),
        Memory(None) => "memory".to_string(),
        Background(Some(s)) => format!("bg {}", s),
        Background(None) => "bg".to_string(),
        Bench(Some(s)) => format!("bench {}", s),
        Bench(None) => "bench".to_string(),
        Browse(Some(s)) => format!("browse {}", s),
        Browse(None) => "browse".to_string(),
        Mesh(Some(s)) => format!("mesh {}", s),
        Mesh(None) => "mesh".to_string(),
        Watch(Some(s)) => format!("watch {}", s),
        Watch(None) => "watch".to_string(),
        Deps(Some(s)) => format!("deps {}", s),
        Deps(None) => "deps".to_string(),
        Unknown(s) => s.clone(),
    }
}

/// Run the full-screen TUI mode.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    mut agent: AgentLoop,
    event_rx: &mut tokio::sync::broadcast::Receiver<pipit_core::AgentEvent>,
    project_root: &PathBuf,
    _skills: &mut SkillRegistry,
    _workflow_assets: &workflow::WorkflowAssets,
    extensions: &Arc<dyn pipit_extensions::ExtensionRunner>,
    status: StatusBarState,
    _trace_ui: bool,
    agent_mode: pipit_core::AgentMode,
) -> Result<()> {
    use crossterm::event::{self as crossterm_event, Event};

    dbg_log("[tui] entering tui::run()");
    let _ = extensions.on_session_start().await;
    dbg_log("[tui] on_session_start done");

    let tui_state = Arc::new(std::sync::Mutex::new(TuiState::new(
        status,
        project_root.clone(),
    )));
    dbg_log("[tui] TuiState created, calling init_terminal…");
    let mut terminal = app::init_terminal().context("Failed to init TUI")?;
    dbg_log("[tui] init_terminal OK (alternate screen active)");

    // Set agent mode
    {
        let mut state = tui_state.lock().unwrap();
        state.agent_mode = agent_mode.to_string();
    }

    // Bridge agent events into the main loop via an mpsc channel
    // instead of having a separate task take the TuiState mutex.
    let (agent_event_tx, mut agent_event_rx) =
        tokio::sync::mpsc::channel::<pipit_core::AgentEvent>(1024);
    let mut event_rx_owned = event_rx.resubscribe();
    let _event_bridge = tokio::spawn(async move {
        while let Ok(event) = event_rx_owned.recv().await {
            if agent_event_tx.send(event).await.is_err() {
                break;
            }
        }
    });

    // Channel for sending prompts to the agent task
    let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::channel::<String>(8);

    // Shared cancellation token — Escape key cancels the current run
    let cancel_token: Arc<std::sync::Mutex<CancellationToken>> =
        Arc::new(std::sync::Mutex::new(CancellationToken::new()));
    let cancel_for_agent = cancel_token.clone();

    // Spawn agent runner as a separate task so the TUI keeps redrawing
    let tui_state_for_agent = tui_state.clone();
    let agent_handle = tokio::spawn(async move {
        while let Some(prompt) = prompt_rx.recv().await {
            {
                let mut s = tui_state_for_agent.lock().unwrap();
                s.begin_working("Thinking…");
                s.run_finished = false;
                s.ui_mode = pipit_io::app::UiMode::Task;
                s.focused_pane = pipit_io::app::PaneFocus::Response;
            }
            let cancel = cancel_for_agent.lock().unwrap().clone();
            let outcome = agent.run(prompt, cancel).await;
            {
                let mut s = tui_state_for_agent.lock().unwrap();
                s.run_finished = true;
                s.finish_working();
                match &outcome {
                    AgentOutcome::Completed { turns, cost, .. } => {
                        s.push_activity(
                            "✓",
                            Color::Green,
                            format!("Done — {} turns, ${:.4}", turns, cost),
                        );
                        s.completion_status = Some(pipit_io::app::CompletionBanner {
                            icon: "✓".to_string(),
                            message: format!("Completed — {} turns, ${:.4}", turns, cost),
                            color: Color::Green,
                        });
                    }
                    AgentOutcome::Error(e) => {
                        let short = if e.len() > 80 {
                            format!("{}…", &e[..78])
                        } else {
                            e.clone()
                        };
                        s.push_activity("✗", Color::Red, format!("Error: {}", short));
                        s.completion_status = Some(pipit_io::app::CompletionBanner {
                            icon: "✗".to_string(),
                            message: format!("Error: {}", short),
                            color: Color::Red,
                        });
                    }
                    AgentOutcome::Cancelled => {
                        s.push_activity("·", Color::DarkGray, "Cancelled".to_string());
                        s.completion_status = Some(pipit_io::app::CompletionBanner {
                            icon: "·".to_string(),
                            message: "Cancelled".to_string(),
                            color: Color::DarkGray,
                        });
                    }
                    AgentOutcome::MaxTurnsReached(n) => {
                        s.push_activity("⚠", Color::Yellow, format!("Max turns ({})", n));
                        s.completion_status = Some(pipit_io::app::CompletionBanner {
                            icon: "⚠".to_string(),
                            message: format!("Max turns reached ({})", n),
                            color: Color::Yellow,
                        });
                    }
                    AgentOutcome::BudgetExhausted { cost, budget, .. } => {
                        s.push_activity(
                            "$",
                            Color::Yellow,
                            format!("Budget exhausted: ${:.4}/${:.2}", cost, budget),
                        );
                        s.completion_status = Some(pipit_io::app::CompletionBanner {
                            icon: "$".to_string(),
                            message: format!(
                                "Budget exhausted: ${:.4} / ${:.2} limit",
                                cost, budget
                            ),
                            color: Color::Yellow,
                        });
                    }
                }
                // Stay in Task mode so the user can see results.
                // They can press 'g' to return to Shell.
            }
        }
    });

    dbg_log("[tui] spawned event handler + agent runner, entering main loop");

    // Main TUI event loop — optimized for responsiveness.
    //
    // Previous design: crossterm::poll(16ms) blocked the thread, preventing
    // agent events from being processed. Draw was called every cycle even
    // when nothing changed.
    //
    // New design:
    //   1. Poll crossterm with 0ms timeout (non-blocking)
    //   2. Drain ALL pending agent events in a batch
    //   3. Only draw when something actually changed (dirty flag)
    //   4. Use a short sleep (8ms) to yield CPU when idle
    //   5. Never hold the mutex during draw preparation — only during state mutation
    let mut needs_redraw = true; // Force initial draw

    loop {
        // ── Phase 1: Drain agent events (batch) ──
        // Process ALL pending events before drawing — this prevents
        // the "frozen" feeling when events pile up.
        let mut events_processed = 0u32;
        while let Ok(event) = agent_event_rx.try_recv() {
            let mut state = tui_state.lock().unwrap();
            apply_agent_event(&mut state, &event);
            events_processed += 1;
            // Cap per-frame event processing to prevent UI starvation
            // during rapid-fire events (e.g. streaming deltas)
            if events_processed >= 64 {
                break;
            }
        }
        if events_processed > 0 {
            needs_redraw = true;
        }

        // ── Phase 2: Handle terminal input (non-blocking) ──
        // Poll with 0ms — never blocks. This ensures agent events
        // are processed immediately on the next iteration.
        while crossterm_event::poll(std::time::Duration::ZERO)? {
            let event = crossterm_event::read()?;
            needs_redraw = true;
            match event {
                Event::Paste(text) => {
                    let mut state = tui_state.lock().unwrap();
                    state.composer.handle_paste(&text);
                }
                Event::Key(key) => {
                    let mut state = tui_state.lock().unwrap();
                    app::handle_key(&mut state, key);

                    if state.should_quit {
                        break; // Exit poll loop — Phase 3 handles quit
                    }

                    // Escape cancels the current agent run
                    if key.code == crossterm::event::KeyCode::Esc && state.is_working {
                        let mut token = cancel_token.lock().unwrap();
                        token.cancel();
                        *token = CancellationToken::new();
                        state.finish_working();
                        state.push_activity("⏹", Color::Yellow, "Stopped".to_string());
                    }

                    // Check if composer submitted input
                    if let Some(submitted) = state.composer.submitted.take() {
                        let input = submitted.text.clone();

                        // Update task label for every submission
                        state.has_received_input = true;
                        state.task_label = if input.len() > 80 {
                            format!("{}…", &input.chars().take(78).collect::<String>())
                        } else {
                            input.clone()
                        };

                        let display = if input.len() > 120 {
                            format!(
                                "{}… [{} chars]",
                                &input.chars().take(100).collect::<String>(),
                                input.chars().count()
                            )
                        } else {
                            input.clone()
                        };
                        state.push_activity("›", Color::Green, display);

                        drop(state); // Release lock before async

                        let classified = pipit_io::input::classify_input(&input);
                        match classified {
                            pipit_io::input::UserInput::Command(cmd) => {
                                match cmd {
                                    pipit_io::input::SlashCommand::Quit => {
                                        let mut s = tui_state.lock().unwrap();
                                        s.should_quit = true;
                                        break;
                                    }
                                    pipit_io::input::SlashCommand::Help => {
                                        let mut s = tui_state.lock().unwrap();
                                        s.push_activity("?", Color::Cyan, "/help".to_string());
                                        s.content_lines.clear();
                                        s.content_scroll_offset = 0;
                                        let help = vec![
                                            "## Commands",
                                            "",
                                            "### Navigation",
                                            "",
                                            "- `/help` — Show this help",
                                            "- `/status` — Show repo, model, tokens, cost",
                                            "- `/cost` — Show token cost summary",
                                            "- `/clear` — Reset context and chat history",
                                            "- `/quit` `/q` — Exit pipit",
                                            "",
                                            "### Configuration",
                                            "",
                                            "- `/config` — Show current configuration",
                                            "- `/setup` — Setup wizard instructions",
                                            "- `/model <name>` — Switch model",
                                            "- `/permissions <mode>` — suggest / auto_edit / full_auto",
                                            "",
                                            "### Context",
                                            "",
                                            "- `/context` `/ctx` — Show files in working set",
                                            "- `/tokens` `/tok` — Token usage breakdown",
                                            "- `/compact` — Compress context to free tokens",
                                            "- `/add <file>` — Add file to working set",
                                            "- `/drop <file>` — Remove file from working set",
                                            "",
                                            "### Git & Version Control",
                                            "",
                                            "- `/diff` — Show uncommitted changes",
                                            "- `/commit [msg]` — Commit with AI-generated message",
                                            "- `/undo` — Undo last agent edits",
                                            "- `/branch [name]` — Create branch or show current",
                                            "- `/branches` — List all branches",
                                            "- `/switch <branch>` — Switch branch",
                                            "",
                                            "### Workflows",
                                            "",
                                            "- `/plan [goal]` — Enter plan-first mode",
                                            "- `/verify [scope]` — Run build/lint/test checks",
                                            "- `/aside <question>` — Quick side question",
                                            "- `/spec [file]` — Spec-driven development",
                                            "- `/tdd [topic]` — Test-driven workflow",
                                            "- `/review` — Code review uncommitted changes",
                                            "- `/fix` — Auto-fix build errors",
                                            "- `/search <query>` — Search codebase",
                                            "- `/loop [N] <prompt>` — Repeat every N seconds",
                                            "- `/bg <prompt>` — Background task via daemon",
                                            "",
                                            "### Session & Memory",
                                            "",
                                            "- `/save [name]` — Save current session",
                                            "- `/resume [name]` — Resume saved session",
                                            "- `/memory [add|list|clear]` — Persistent knowledge",
                                            "",
                                            "### System",
                                            "",
                                            "- `/doctor` — System health check",
                                            "- `/skills` — List available skills",
                                            "- `/hooks` — List active hooks",
                                            "- `/mcp` — MCP server status",
                                            "- `/deps` — Dependency health scan",
                                            "",
                                            "### Advanced",
                                            "",
                                            "- `/bench [run|list|history]` — Benchmark runner",
                                            "- `/browse <url>` — Headless browser testing",
                                            "- `/mesh [status|nodes|join]` — Distributed mesh",
                                            "- `/watch [start|deps|tests]` — Ambient monitor",
                                            "",
                                            "### Grammar",
                                            "",
                                            "- `@file.rs` — Attach file as context",
                                            "- `!ls -la` — Run shell command directly",
                                            "- `Ctrl-J` — Insert newline (multiline)",
                                            "- `Tab` — Tab-complete commands and files",
                                            "- `↑ ↓` — History recall",
                                            "- `Alt-↑/↓` — Scroll content pane",
                                        ];
                                        for line in help {
                                            s.content_lines.push(line.to_string());
                                        }
                                        s.has_received_input = true;
                                        s.ui_mode = pipit_io::app::UiMode::Task;
                                    }
                                    pipit_io::input::SlashCommand::Clear => {
                                        let _ = prompt_tx.send("/clear".to_string()).await;
                                        let mut s = tui_state.lock().unwrap();
                                        s.activity_lines.clear();
                                        s.content_lines.clear();
                                        s.scroll_offset = 0;
                                        s.content_scroll_offset = 0;
                                        s.push_activity(
                                            "·",
                                            Color::DarkGray,
                                            "Context cleared".to_string(),
                                        );
                                        s.ui_mode = pipit_io::app::UiMode::Shell;
                                    }
                                    pipit_io::input::SlashCommand::Cost => {
                                        let s = tui_state.lock().unwrap();
                                        let cost_msg = format!(
                                            "${:.4} · {}% tokens",
                                            s.status.cost,
                                            s.status.token_pct()
                                        );
                                        drop(s);
                                        let mut s = tui_state.lock().unwrap();
                                        s.push_activity("$", Color::Green, cost_msg);
                                        s.ui_mode = pipit_io::app::UiMode::Task;
                                    }
                                    pipit_io::input::SlashCommand::Status => {
                                        let s = tui_state.lock().unwrap();
                                        let info = format!(
                                            "{} · {} · {} · {}% tokens · ${:.4}",
                                            s.status.repo_name,
                                            s.status.branch,
                                            s.status.model,
                                            s.status.token_pct(),
                                            s.status.cost
                                        );
                                        drop(s);
                                        let mut s = tui_state.lock().unwrap();
                                        s.push_activity("·", Color::Cyan, info);
                                        s.ui_mode = pipit_io::app::UiMode::Task;
                                    }
                                    pipit_io::input::SlashCommand::Config(ref _key) => {
                                        let mut s = tui_state.lock().unwrap();
                                        s.push_activity("⚙", Color::Cyan, "/config".to_string());
                                        s.content_lines.clear();
                                        s.content_scroll_offset = 0;

                                        // Snapshot status values before borrowing content_lines
                                        let config_path = pipit_config::user_config_path()
                                            .map(|p| p.display().to_string())
                                            .unwrap_or_else(|| {
                                                "~/.config/pipit/config.toml".to_string()
                                            });
                                        let has_config = pipit_config::has_user_config();
                                        let provider = s.status.provider_kind.clone();
                                        let model = s.status.model.clone();
                                        let base_url = s.status.base_url.clone();
                                        let mode = s.status.agent_mode.clone();
                                        let approval = format!("{}", s.status.approval_mode);
                                        let max_turns = s.status.max_turns;

                                        let lines: Vec<String> = vec![
                                            "## Current Configuration".into(),
                                            String::new(),
                                            format!("**Config file:** `{}`", config_path),
                                            format!(
                                                "**Exists:** {}",
                                                if has_config { "✓ yes" } else { "✗ no" }
                                            ),
                                            String::new(),
                                            "### Active Settings".into(),
                                            String::new(),
                                            format!("- **Provider:** `{}`", provider),
                                            format!("- **Model:** `{}`", model),
                                            if !base_url.is_empty() {
                                                format!("- **Base URL:** `{}`", base_url)
                                            } else {
                                                String::new()
                                            },
                                            format!("- **Mode:** `{}`", mode),
                                            format!("- **Approval:** `{}`", approval),
                                            format!("- **Max turns:** `{}`", max_turns),
                                            String::new(),
                                            "### Config Sources (priority order)".into(),
                                            String::new(),
                                            "1. CLI flags (`--provider`, `--model`, etc.)".into(),
                                            "2. Environment variables (`PIPIT_PROVIDER`, etc.)"
                                                .into(),
                                            "3. Project config (`.pipit/config.toml`)".into(),
                                            format!("4. User config (`{}`)", config_path),
                                            String::new(),
                                            "### Quick Actions".into(),
                                            String::new(),
                                            "- `/setup` — Re-run interactive setup wizard".into(),
                                            "- `/model <name>` — Switch model".into(),
                                            "- `/permissions <mode>` — Switch approval mode".into(),
                                        ];
                                        s.content_lines.extend(
                                            lines.into_iter().filter(|l| !l.is_empty() || true),
                                        );

                                        s.has_received_input = true;
                                        s.ui_mode = pipit_io::app::UiMode::Task;
                                    }
                                    pipit_io::input::SlashCommand::Setup => {
                                        let mut s = tui_state.lock().unwrap();
                                        s.push_activity("⚙", Color::Yellow, "/setup".to_string());
                                        s.content_lines.clear();
                                        s.content_scroll_offset = 0;

                                        s.content_lines.push("## Setup Wizard".to_string());
                                        s.content_lines.push(String::new());
                                        s.content_lines.push(
                                            "The interactive setup wizard runs outside the TUI."
                                                .to_string(),
                                        );
                                        s.content_lines.push(String::new());
                                        s.content_lines.push("### To reconfigure:".to_string());
                                        s.content_lines.push(String::new());
                                        s.content_lines
                                            .push("1. Press `Ctrl-C` to exit".to_string());
                                        s.content_lines.push("2. Run `pipit setup`".to_string());
                                        s.content_lines.push(
                                            "3. Run `pipit` to start with new config".to_string(),
                                        );
                                        s.content_lines.push(String::new());
                                        s.content_lines.push(
                                            "### Quick changes (no restart needed):".to_string(),
                                        );
                                        s.content_lines.push(String::new());
                                        s.content_lines
                                            .push("- `/model <name>` — Switch model".to_string());
                                        s.content_lines.push("- `/permissions <mode>` — suggest / auto_edit / full_auto".to_string());
                                        s.content_lines.push(String::new());
                                        s.content_lines.push("### Config file:".to_string());
                                        s.content_lines.push(String::new());
                                        let cfg_path = pipit_config::user_config_path()
                                            .map(|p| p.display().to_string())
                                            .unwrap_or_else(|| {
                                                "~/.config/pipit/config.toml".to_string()
                                            });
                                        s.content_lines.push(format!("  `{}`", cfg_path));
                                        s.content_lines.push(String::new());
                                        s.content_lines.push(
                                            "Edit this file directly for advanced settings."
                                                .to_string(),
                                        );

                                        s.has_received_input = true;
                                        s.ui_mode = pipit_io::app::UiMode::Task;
                                    }
                                    pipit_io::input::SlashCommand::Doctor => {
                                        let mut s = tui_state.lock().unwrap();
                                        s.push_activity("🏥", Color::Cyan, "/doctor".to_string());
                                        s.content_lines.clear();
                                        s.content_scroll_offset = 0;

                                        let provider = s.status.provider_kind.clone();
                                        let model = s.status.model.clone();
                                        let base_url = s.status.base_url.clone();
                                        let tokens_used = s.status.tokens_used;
                                        let tokens_limit = s.status.tokens_limit;

                                        let lines: Vec<String> = vec![
                                            "## System Health Check".into(),
                                            String::new(),
                                            "### Provider".into(),
                                            String::new(),
                                            format!("- **Provider:** `{}`", provider),
                                            format!("- **Model:** `{}`", model),
                                            if !base_url.is_empty() { format!("- **Endpoint:** `{}`", base_url) } else { "- **Endpoint:** default".into() },
                                            format!("- **Status:** ✓ connected (you're using it right now)"),
                                            String::new(),
                                            "### Context Budget".into(),
                                            String::new(),
                                            format!("- **Tokens used:** `{}`", tokens_used),
                                            format!("- **Token limit:** `{}`", tokens_limit),
                                            format!("- **Usage:** `{}%`", if tokens_limit > 0 { tokens_used * 100 / tokens_limit } else { 0 }),
                                            String::new(),
                                            "### Extensions".into(),
                                            String::new(),
                                            "- Use `/skills` to list available skills".into(),
                                            "- Use `/hooks` to list active hooks".into(),
                                            "- Use `/mcp` to show MCP server status".into(),
                                            String::new(),
                                            "> To test provider connectivity from terminal: `pipit setup`".into(),
                                        ];
                                        s.content_lines.extend(lines);
                                        s.has_received_input = true;
                                        s.ui_mode = pipit_io::app::UiMode::Task;
                                    }
                                    pipit_io::input::SlashCommand::Skills => {
                                        // Delegate to agent — it will list skills
                                        let _ = prompt_tx.send("/skills".to_string()).await;
                                    }
                                    pipit_io::input::SlashCommand::Hooks => {
                                        let _ = prompt_tx.send("/hooks".to_string()).await;
                                    }
                                    pipit_io::input::SlashCommand::Mcp => {
                                        let _ = prompt_tx.send("/mcp".to_string()).await;
                                    }
                                    pipit_io::input::SlashCommand::Undo
                                    | pipit_io::input::SlashCommand::Rewind => {
                                        let mut s = tui_state.lock().unwrap();
                                        s.push_activity("↩", Color::Yellow, "/undo".to_string());
                                        s.ui_mode = pipit_io::app::UiMode::Task;
                                        // Check git for recently modified files by the agent
                                        let output = std::process::Command::new("git")
                                            .args(["diff", "--name-only", "HEAD~1"])
                                            .current_dir(project_root)
                                            .output();
                                        match output {
                                            Ok(o) if o.status.success() => {
                                                let stdout =
                                                    String::from_utf8_lossy(&o.stdout).to_string();
                                                let files: Vec<&str> = stdout.lines().collect();
                                                if files.is_empty() {
                                                    s.push_activity(
                                                        "·",
                                                        Color::DarkGray,
                                                        "Nothing to undo".to_string(),
                                                    );
                                                } else {
                                                    drop(s); // release lock for git ops

                                                    // Safety check: verify the HEAD commit was made by pipit
                                                    // (committer = ForgeCode / PipitCode). If the user made
                                                    // their own commits between agent runs, /undo would
                                                    // silently revert the user's work — which is destructive.
                                                    let committer =
                                                        std::process::Command::new("git")
                                                            .args(["log", "-1", "--format=%cn"])
                                                            .current_dir(project_root)
                                                            .output()
                                                            .ok()
                                                            .and_then(|o| {
                                                                if o.status.success() {
                                                                    Some(
                                                                        String::from_utf8_lossy(
                                                                            &o.stdout,
                                                                        )
                                                                        .trim()
                                                                        .to_lowercase(),
                                                                    )
                                                                } else {
                                                                    None
                                                                }
                                                            })
                                                            .unwrap_or_default();
                                                    let is_agent_commit = committer
                                                        .contains("forge")
                                                        || committer.contains("pipit")
                                                        || committer.is_empty(); // initial commit edge case

                                                    if !is_agent_commit {
                                                        let mut s = tui_state.lock().unwrap();
                                                        s.push_activity("⚠", Color::Yellow,
                                                            format!("HEAD commit was made by '{}', not pipit. Use `git revert` manually to be safe.", committer));
                                                    } else {
                                                        let head =
                                                            std::process::Command::new("git")
                                                                .args(["rev-parse", "HEAD~1"])
                                                                .current_dir(project_root)
                                                                .output()
                                                                .ok()
                                                                .and_then(|o| {
                                                                    if o.status.success() {
                                                                        Some(
                                                                    String::from_utf8_lossy(
                                                                        &o.stdout,
                                                                    )
                                                                    .trim()
                                                                    .to_string(),
                                                                )
                                                                    } else {
                                                                        None
                                                                    }
                                                                });
                                                        if let Some(sha) = head {
                                                            let mut restored = 0;
                                                            let mut removed = 0;
                                                            for file in &files {
                                                                // Check if the file existed at the rollback point.
                                                                // Files that were newly created by the agent won't
                                                                // exist in HEAD~1, so `git checkout` won't handle them.
                                                                let existed_before =
                                                                    std::process::Command::new(
                                                                        "git",
                                                                    )
                                                                    .args([
                                                                        "cat-file",
                                                                        "-e",
                                                                        &format!(
                                                                            "{}:{}",
                                                                            sha, file
                                                                        ),
                                                                    ])
                                                                    .current_dir(project_root)
                                                                    .output()
                                                                    .map(|o| o.status.success())
                                                                    .unwrap_or(false);

                                                                if existed_before {
                                                                    // File existed — restore to previous version
                                                                    let r =
                                                                        std::process::Command::new(
                                                                            "git",
                                                                        )
                                                                        .args([
                                                                            "checkout", &sha, "--",
                                                                            file,
                                                                        ])
                                                                        .current_dir(project_root)
                                                                        .output();
                                                                    if r.map(|o| o.status.success())
                                                                        .unwrap_or(false)
                                                                    {
                                                                        restored += 1;
                                                                    }
                                                                } else {
                                                                    // File was newly created — remove it
                                                                    let file_path =
                                                                        std::path::Path::new(
                                                                            project_root,
                                                                        )
                                                                        .join(file);
                                                                    if file_path.exists() {
                                                                        if std::fs::remove_file(
                                                                            &file_path,
                                                                        )
                                                                        .is_ok()
                                                                        {
                                                                            removed += 1;
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            let mut s = tui_state.lock().unwrap();
                                                            let mut msg = format!(
                                                                "Restored {} file(s) to {}",
                                                                restored,
                                                                &sha[..8]
                                                            );
                                                            if removed > 0 {
                                                                msg.push_str(&format!(
                                                                    ", removed {} new file(s)",
                                                                    removed
                                                                ));
                                                            }
                                                            s.push_activity("✓", Color::Green, msg);
                                                        } else {
                                                            let mut s = tui_state.lock().unwrap();
                                                            s.push_activity("✗", Color::Red, "Could not determine rollback point (initial commit?)".to_string());
                                                        }
                                                    }
                                                }
                                            }
                                            _ => {
                                                s.push_activity(
                                                    "✗",
                                                    Color::Red,
                                                    "Not in a git repo".to_string(),
                                                );
                                            }
                                        }
                                    }
                                    pipit_io::input::SlashCommand::Branch(ref name) => {
                                        let mut s = tui_state.lock().unwrap();
                                        s.ui_mode = pipit_io::app::UiMode::Task;
                                        if let Some(branch_name) = name {
                                            drop(s);
                                            let output = std::process::Command::new("git")
                                                .args(["checkout", "-b", branch_name])
                                                .current_dir(project_root)
                                                .output();
                                            let mut s = tui_state.lock().unwrap();
                                            match output {
                                                Ok(o) if o.status.success() => {
                                                    s.push_activity(
                                                        "🌿",
                                                        Color::Green,
                                                        format!("Created branch '{}'", branch_name),
                                                    );
                                                    s.status.branch = branch_name.clone();
                                                }
                                                Ok(o) => {
                                                    let err = String::from_utf8_lossy(&o.stderr);
                                                    s.push_activity(
                                                        "✗",
                                                        Color::Red,
                                                        err.trim().to_string(),
                                                    );
                                                }
                                                Err(e) => s.push_activity(
                                                    "✗",
                                                    Color::Red,
                                                    format!("git: {}", e),
                                                ),
                                            }
                                        } else {
                                            let branch = s.status.branch.clone();
                                            s.push_activity(
                                                "🌿",
                                                Color::Cyan,
                                                format!("Current branch: {}", branch),
                                            );
                                        }
                                    }
                                    pipit_io::input::SlashCommand::BranchList => {
                                        let output = std::process::Command::new("git")
                                            .args(["branch", "-a", "--no-color"])
                                            .current_dir(project_root)
                                            .output();
                                        let mut s = tui_state.lock().unwrap();
                                        s.content_lines.clear();
                                        s.content_scroll_offset = 0;
                                        s.content_lines.push("## Branches".to_string());
                                        s.content_lines.push(String::new());
                                        match output {
                                            Ok(o) => {
                                                let branches = String::from_utf8_lossy(&o.stdout);
                                                for line in branches.lines() {
                                                    s.content_lines
                                                        .push(format!("`{}`", line.trim()));
                                                }
                                            }
                                            Err(e) => s.content_lines.push(format!("Error: {}", e)),
                                        }
                                        s.has_received_input = true;
                                        s.ui_mode = pipit_io::app::UiMode::Task;
                                    }
                                    pipit_io::input::SlashCommand::BranchSwitch(ref target) => {
                                        if target.is_empty() {
                                            let mut s = tui_state.lock().unwrap();
                                            s.ui_mode = pipit_io::app::UiMode::Task;
                                            s.push_activity(
                                                "⚠",
                                                Color::Yellow,
                                                "Usage: /switch <branch>".to_string(),
                                            );
                                        } else {
                                            // Check for dirty state before switching
                                            let dirty = std::process::Command::new("git")
                                                .args(["status", "--porcelain"])
                                                .current_dir(project_root)
                                                .output()
                                                .map(|o| !o.stdout.is_empty())
                                                .unwrap_or(false);
                                            let mut stashed = false;
                                            if dirty {
                                                let stash_result =
                                                    std::process::Command::new("git")
                                                        .args([
                                                            "stash",
                                                            "push",
                                                            "-m",
                                                            "pipit-auto-stash",
                                                        ])
                                                        .current_dir(project_root)
                                                        .output();
                                                stashed = stash_result
                                                    .map(|o| o.status.success())
                                                    .unwrap_or(false);
                                            }

                                            let output = std::process::Command::new("git")
                                                .args(["checkout", target])
                                                .current_dir(project_root)
                                                .output();
                                            let mut s = tui_state.lock().unwrap();
                                            s.ui_mode = pipit_io::app::UiMode::Task;
                                            match output {
                                                Ok(o) if o.status.success() => {
                                                    let mut msg =
                                                        format!("Switched to '{}'", target);
                                                    if stashed {
                                                        msg.push_str(" (changes stashed — use `!git stash pop` to restore)");
                                                    }
                                                    s.push_activity("✓", Color::Green, msg);
                                                    s.status.branch = target.clone();
                                                }
                                                Ok(o) => {
                                                    let err = String::from_utf8_lossy(&o.stderr);
                                                    s.push_activity(
                                                        "✗",
                                                        Color::Red,
                                                        err.trim().to_string(),
                                                    );
                                                    // Auto-recover stash on failure
                                                    if stashed {
                                                        drop(s);
                                                        let _ = std::process::Command::new("git")
                                                            .args(["stash", "pop"])
                                                            .current_dir(project_root)
                                                            .output();
                                                        let mut s = tui_state.lock().unwrap();
                                                        s.push_activity(
                                                            "↩",
                                                            Color::Yellow,
                                                            "Auto-stash restored".to_string(),
                                                        );
                                                    }
                                                }
                                                Err(e) => {
                                                    s.push_activity(
                                                        "✗",
                                                        Color::Red,
                                                        format!("git: {}", e),
                                                    );
                                                    // Auto-recover stash on error
                                                    if stashed {
                                                        drop(s);
                                                        let _ = std::process::Command::new("git")
                                                            .args(["stash", "pop"])
                                                            .current_dir(project_root)
                                                            .output();
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    pipit_io::input::SlashCommand::Diff => {
                                        let mut s = tui_state.lock().unwrap();
                                        s.push_activity("±", Color::Cyan, "/diff".to_string());
                                        s.content_lines.clear();
                                        s.content_scroll_offset = 0;
                                        s.content_lines.push("## Uncommitted Changes".to_string());
                                        s.content_lines.push(String::new());
                                        drop(s);

                                        let staged = std::process::Command::new("git")
                                            .args(["diff", "--staged"])
                                            .current_dir(project_root)
                                            .output();
                                        let unstaged = std::process::Command::new("git")
                                            .args(["diff"])
                                            .current_dir(project_root)
                                            .output();

                                        let mut s = tui_state.lock().unwrap();
                                        if let Ok(ref o) = staged {
                                            let text = String::from_utf8_lossy(&o.stdout);
                                            if !text.trim().is_empty() {
                                                s.content_lines.push("### Staged".to_string());
                                                s.content_lines.push("```diff".to_string());
                                                for line in text.lines().take(200) {
                                                    s.content_lines.push(line.to_string());
                                                }
                                                s.content_lines.push("```".to_string());
                                                s.content_lines.push(String::new());
                                            }
                                        }
                                        if let Ok(ref o) = unstaged {
                                            let text = String::from_utf8_lossy(&o.stdout);
                                            if !text.trim().is_empty() {
                                                s.content_lines.push("### Unstaged".to_string());
                                                s.content_lines.push("```diff".to_string());
                                                for line in text.lines().take(200) {
                                                    s.content_lines.push(line.to_string());
                                                }
                                                s.content_lines.push("```".to_string());
                                            }
                                        }
                                        let has_content = s.content_lines.len() > 2;
                                        if !has_content {
                                            s.content_lines
                                                .push("*No uncommitted changes*".to_string());
                                        }
                                        s.has_received_input = true;
                                        s.ui_mode = pipit_io::app::UiMode::Task;
                                    }
                                    pipit_io::input::SlashCommand::Commit(ref msg) => {
                                        let mut s = tui_state.lock().unwrap();
                                        s.push_activity("📝", Color::Green, "/commit".to_string());
                                        s.ui_mode = pipit_io::app::UiMode::Task;
                                        drop(s);

                                        // Check for staged changes
                                        let staged = std::process::Command::new("git")
                                            .args(["diff", "--cached", "--stat"])
                                            .env("GIT_PAGER", "cat")
                                            .current_dir(project_root)
                                            .output();
                                        let has_staged = staged
                                            .as_ref()
                                            .map(|o| !o.stdout.is_empty())
                                            .unwrap_or(false);
                                        if !has_staged {
                                            // Auto-stage all changes
                                            let _ = std::process::Command::new("git")
                                                .args(["add", "-A"])
                                                .current_dir(project_root)
                                                .output();
                                        }

                                        if let Some(message) = msg {
                                            // Direct commit with provided message
                                            let output = std::process::Command::new("git")
                                                .args(["commit", "-m", message])
                                                .current_dir(project_root)
                                                .output();
                                            let mut s = tui_state.lock().unwrap();
                                            match output {
                                                Ok(o) if o.status.success() => {
                                                    s.push_activity(
                                                        "✓",
                                                        Color::Green,
                                                        format!("Committed: {}", message),
                                                    );
                                                }
                                                Ok(o) => {
                                                    let err = String::from_utf8_lossy(&o.stderr);
                                                    s.push_activity(
                                                        "✗",
                                                        Color::Red,
                                                        err.trim().to_string(),
                                                    );
                                                }
                                                Err(e) => s.push_activity(
                                                    "✗",
                                                    Color::Red,
                                                    format!("git: {}", e),
                                                ),
                                            }
                                        } else {
                                            // No message — delegate to agent for LLM-generated commit message
                                            let _ = prompt_tx.send(
                                                "Run `git diff --cached` and generate a conventional commit message \
                                                 (type(scope): description). Then run `git commit -m \"<your message>\"` \
                                                 to commit. Do NOT use --no-edit.".to_string()
                                            ).await;
                                        }
                                    }
                                    other => {
                                        let cmd_str = format!("/{}", slash_command_to_str(&other));
                                        let _ = prompt_tx.send(cmd_str).await;
                                    }
                                }
                            }
                            pipit_io::input::UserInput::Prompt(prompt) => {
                                let _ = prompt_tx.send(prompt).await;
                            }
                            pipit_io::input::UserInput::ShellPassthrough(cmd) => {
                                // Push to composer's shell history for !-completion
                                {
                                    let mut s = tui_state.lock().unwrap();
                                    s.composer.push_shell_history(&cmd);
                                    s.push_activity("$", Color::Green, format!("$ {}", cmd));
                                }
                                // Execute directly in shell — NOT through the AI
                                let output = tokio::process::Command::new("sh")
                                    .arg("-c")
                                    .arg(&cmd)
                                    .current_dir(project_root)
                                    .output()
                                    .await;
                                let mut s = tui_state.lock().unwrap();
                                // Add a visual header in the content pane
                                s.content_lines.push(String::new());
                                s.content_lines.push(format!("$ {}", cmd));
                                match output {
                                    Ok(o) => {
                                        let stdout = String::from_utf8_lossy(&o.stdout);
                                        let stderr = String::from_utf8_lossy(&o.stderr);
                                        if !stdout.is_empty() {
                                            for line in stdout.lines() {
                                                s.content_lines.push(line.to_string());
                                            }
                                        }
                                        if !stderr.is_empty() {
                                            s.content_lines.push("[stderr]".to_string());
                                            for line in stderr.lines() {
                                                s.content_lines.push(line.to_string());
                                            }
                                        }
                                        if stdout.is_empty() && stderr.is_empty() {
                                            s.content_lines.push("(no output)".to_string());
                                        }
                                        if !o.status.success() {
                                            if let Some(code) = o.status.code() {
                                                s.content_lines
                                                    .push(format!("exit code: {}", code));
                                                s.push_activity(
                                                    "✗",
                                                    Color::Red,
                                                    format!("exit {}", code),
                                                );
                                            }
                                        } else {
                                            s.push_activity("✓", Color::Green, "done".to_string());
                                        }
                                    }
                                    Err(e) => {
                                        s.content_lines.push(format!("Error: {}", e));
                                        s.push_activity("✗", Color::Red, format!("error: {}", e));
                                    }
                                }
                                s.has_received_input = true;
                                s.ui_mode = pipit_io::app::UiMode::Task;
                                s.auto_scroll_content();
                            }
                            pipit_io::input::UserInput::PromptWithFiles { prompt, files } => {
                                let enriched = format!(
                                    "First read these files: {}. Then: {}",
                                    files.join(", "),
                                    prompt
                                );
                                let _ = prompt_tx.send(enriched).await;
                            }
                            pipit_io::input::UserInput::PromptWithImages {
                                prompt,
                                image_paths,
                            } => {
                                let enriched = format!(
                                    "Analyze these image files: {}. {}",
                                    image_paths.join(", "),
                                    prompt
                                );
                                let _ = prompt_tx.send(enriched).await;
                            }
                        }
                    }
                }
                Event::Mouse(mouse) => {
                    let mut state = tui_state.lock().unwrap();
                    let size = terminal.size().ok();
                    let width = size.map(|s| s.width).unwrap_or(80);
                    let height = size.map(|s| s.height).unwrap_or(24);
                    app::handle_mouse(&mut state, mouse, width, height);
                }
                Event::Resize(cols, rows) => {
                    let mut state = tui_state.lock().unwrap();
                    app::handle_resize(&mut state, cols, rows);
                }
                _ => {}
            }
        }

        // ── Phase 3: Draw (only when dirty) ──
        // Skip redundant draws when nothing changed — saves CPU in idle state.
        // Always redraw when agent is working (spinner animation).
        {
            let mut state = tui_state.lock().unwrap();
            let is_animating = state.is_working || state.is_thinking;
            if is_animating {
                needs_redraw = true;
            }

            if needs_redraw && state.should_redraw() {
                state.spinner_frame = state.spinner_frame.wrapping_add(1);
                terminal.draw(|f| app::draw(f, &state))?;
                needs_redraw = false;
            }

            if state.should_quit {
                cancel_token.lock().unwrap().cancel();
                break;
            }
        }

        // ── Phase 4: Yield ──
        // Short sleep to prevent busy-spinning when idle.
        // During active work, use a shorter interval for responsive spinners.
        let sleep_ms = {
            let state = tui_state.lock().unwrap();
            if state.is_working || state.is_thinking {
                16
            } else {
                33
            }
        };
        tokio::time::sleep(std::time::Duration::from_millis(sleep_ms)).await;
    }

    // Cleanup
    dbg_log("[tui] exiting main loop, restoring terminal");
    drop(prompt_tx);
    let _ = agent_handle.await;
    let _ = extensions.on_session_end().await;
    app::restore_terminal(&mut terminal)?;
    Ok(())
}

/// Check if `s` ends with a prefix of `<think>` or `</think>`.
/// Returns the matching suffix (which might complete a tag with later input).
fn think_tag_suffix(s: &str) -> &str {
    // All possible prefixes of `<think>` and `</think>` (excluding the full tag)
    const PREFIXES: &[&str] = &[
        "</thin", "</thi", "</th", "</t", "</", "<think", "<thin", "<thi", "<th", "<t", "<",
    ];
    for prefix in PREFIXES {
        if s.ends_with(prefix) {
            return &s[s.len() - prefix.len()..];
        }
    }
    ""
}

/// Shorten a file path for display. Keeps at most the last 3 components.
/// "backend/src/routes/auth.ts" stays as-is.
/// "/Users/sushanth/test-web/backend/src/routes/auth.ts" → "backend/src/routes/auth.ts"
fn shorten_path(path: &str) -> String {
    let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if components.len() <= 3 {
        return path.to_string();
    }
    components[components.len() - 3..].join("/")
}

fn push_content_block(lines: &mut Vec<String>, text: &str) {
    if text.is_empty() {
        lines.push(String::new());
        return;
    }

    for line in text.split('\n') {
        lines.push(line.trim_end_matches('\r').to_string());
    }
}

fn inline_heading_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"([^\n])\s*(#{1,3}\s+)").expect("valid inline heading regex"))
}

fn inline_numbered_list_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"([^\n])\s+(\d+\.\s+)").expect("valid inline numbered-list regex")
    })
}

fn inline_field_list_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\s+(?:—|–|-)\s+([A-Za-z_][A-Za-z0-9_ ]*:\s)")
            .expect("valid inline field-list regex")
    })
}

fn markdown_table_break_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"([^\n])\s+(\|[^\n]*\|[^\n]*\|[^\n]*)")
            .expect("valid markdown table regex")
    })
}

fn normalize_dense_table_runs(line: &str) -> String {
    if line.matches('|').count() >= 6 && line.contains("||") {
        line.replace("||", "|\n|")
    } else {
        line.to_string()
    }
}

fn looks_like_internal_paragraph(paragraph: &str) -> bool {
    let lower = paragraph.trim().to_lowercase();
    let obvious_prefixes = [
        "the user is asking",
        "the user asked",
        "i need to ",
        "let me ",
        "i already ",
        "i have read ",
        "i've read ",
        "i've completed ",
        "this was a simple ",
        "the task is complete",
        "i can now summarize",
        "i should ",
        "i'll ",
    ];
    obvious_prefixes.iter().any(|prefix| lower.starts_with(prefix))
        || lower.contains("let me provide a final summary")
}

fn collapse_blank_lines(text: &str) -> String {
    let mut out = Vec::new();
    let mut previous_blank = false;
    for line in text.lines() {
        let blank = line.trim().is_empty();
        if blank {
            if !previous_blank {
                out.push(String::new());
            }
        } else {
            out.push(line.trim_end().to_string());
        }
        previous_blank = blank;
    }
    out.join("\n").trim().to_string()
}

fn strip_internal_paragraphs(text: &str) -> String {
    let mut kept = Vec::new();
    let mut current = Vec::new();

    let flush = |current: &mut Vec<String>, kept: &mut Vec<String>| {
        if current.is_empty() {
            return;
        }
        let paragraph = current.join("\n");
        if !looks_like_internal_paragraph(&paragraph) {
            kept.push(paragraph);
        }
        current.clear();
    };

    for line in text.lines() {
        if line.trim().is_empty() {
            flush(&mut current, &mut kept);
        } else {
            current.push(line.to_string());
        }
    }
    flush(&mut current, &mut kept);

    kept.join("\n\n")
}

fn normalize_response_markdown(text: &str) -> String {
    let mut cleaned = text.replace("\r\n", "\n").replace('\r', "\n");
    cleaned = cleaned.replace("<think>", "").replace("</think>", "");
    cleaned = inline_heading_re()
        .replace_all(&cleaned, "$1\n\n$2")
        .into_owned();
    cleaned = inline_numbered_list_re()
        .replace_all(&cleaned, "$1\n$2")
        .into_owned();
    cleaned = inline_field_list_re()
        .replace_all(&cleaned, "\n- $1")
        .into_owned();
    cleaned = markdown_table_break_re()
        .replace_all(&cleaned, "$1\n\n$2")
        .into_owned();
    cleaned = cleaned
        .lines()
        .map(normalize_dense_table_runs)
        .collect::<Vec<_>>()
        .join("\n");
    cleaned = strip_internal_paragraphs(&cleaned);
    collapse_blank_lines(&cleaned)
}

fn parse_applied_edit_content(content: &str) -> Option<(&str, &str)> {
    let rest = content.strip_prefix("Applied edit to ")?;
    rest.split_once(":\n")
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or(text)
}

fn mutation_activity_summary(tool_name: &str, content: &str) -> String {
    if tool_name == "edit_file" {
        if let Some((path, _diff)) = parse_applied_edit_content(content) {
            return format!("Edited {}", shorten_path(path));
        }
    }

    first_line(content).chars().take(100).collect()
}

/// Pure function: map an AgentEvent to TuiState mutations.
/// Extracted from the inline closure for testability.
fn apply_agent_event(state: &mut TuiState, event: &pipit_core::AgentEvent) {
    use pipit_core::AgentEvent;
    match event {
        AgentEvent::TurnStart { turn_number } => {
            state.begin_turn(*turn_number);
            state.begin_working(&format!("Turn {}", turn_number));
        }
        AgentEvent::ContentDelta { text } => {
            // Handle <think> tags: toggle thinking mode, strip tags.
            // Prepend any buffered partial tag from a previous delta.
            let combined = if state.tag_buffer.is_empty() {
                text.clone()
            } else {
                let mut c = std::mem::take(&mut state.tag_buffer);
                c.push_str(&text);
                c
            };
            let mut remaining = combined.as_str();
            while !remaining.is_empty() {
                if state.is_thinking {
                    if let Some(end) = remaining.find("</think>") {
                        // Thinking portion before close tag — discard (it's reasoning)
                        remaining = &remaining[end + 8..];
                        state.is_thinking = false;
                    } else {
                        // Check if we have a partial </think> at the end
                        let suffix = think_tag_suffix(remaining);
                        if !suffix.is_empty() {
                            state.tag_buffer = suffix.to_string();
                        }
                        // Still thinking — discard
                        break;
                    }
                } else if let Some(start) = remaining.find("<think>") {
                    // Content before <think> is real response
                    let before = remaining[..start].replace("</think>", "");
                    if !before.trim().is_empty() {
                        state.push_content(&before);
                    }
                    remaining = &remaining[start + 7..];
                    state.is_thinking = true;
                } else {
                    // No open tags — strip stray </think> and check for partial tags
                    let cleaned = remaining.replace("</think>", "");
                    // Check if input ends with a potential partial tag
                    let suffix = think_tag_suffix(remaining);
                    if !suffix.is_empty() {
                        // Buffer the potential partial tag; emit text before it
                        let safe = &cleaned[..cleaned.len().saturating_sub(suffix.len())];
                        if !safe.trim().is_empty() {
                            state.push_content(safe);
                        }
                        state.tag_buffer = suffix.to_string();
                    } else if !cleaned.trim().is_empty() {
                        state.push_content(&cleaned);
                    }
                    break;
                }
            }
        }
        AgentEvent::ContentComplete { full_text } => {
            state.tag_buffer.clear();
            let cleaned = normalize_response_markdown(full_text);
            state.replace_current_turn_content(&cleaned);
            state.finish_working();
        }
        AgentEvent::ToolCallStart { name, args, .. } => {
            state.finish_working();
            if !state.current_turn_had_tool_calls {
                state.current_turn_had_tool_calls = true;
                state.discard_current_turn_content();
            }
            let summary = match name.as_str() {
                "read_file" => {
                    let path = args["path"].as_str().unwrap_or("?");
                    let start = args["start_line"].as_u64();
                    let end = args["end_line"].as_u64();
                    match (start, end) {
                        (Some(s), Some(e)) => {
                            format!("Read {} (lines {}-{})", shorten_path(path), s, e)
                        }
                        _ => format!("Read {}", shorten_path(path)),
                    }
                }
                "edit_file" => format!(
                    "Edit {}",
                    shorten_path(args["path"].as_str().unwrap_or("?"))
                ),
                "write_file" => {
                    let path = args["path"].as_str().unwrap_or("?");
                    let content = args["content"].as_str().unwrap_or("");
                    let lines = content.lines().count();
                    format!("Write {} ({} lines)", shorten_path(path), lines)
                }
                "multi_edit" => format!(
                    "MultiEdit {}",
                    shorten_path(args["path"].as_str().unwrap_or("?"))
                ),
                "bash" => {
                    let cmd = args["command"].as_str().unwrap_or("?");
                    format!("$ {}", cmd.chars().take(72).collect::<String>())
                }
                "grep" => format!(
                    "Grep '{}'",
                    args["pattern"]
                        .as_str()
                        .unwrap_or("?")
                        .chars()
                        .take(40)
                        .collect::<String>()
                ),
                "glob" => format!(
                    "Glob '{}'",
                    args["pattern"]
                        .as_str()
                        .unwrap_or("?")
                        .chars()
                        .take(40)
                        .collect::<String>()
                ),
                "list_directory" => {
                    format!("ls {}", shorten_path(args["path"].as_str().unwrap_or(".")))
                }
                "scaffold_project" => {
                    let root = args["project_root"].as_str().unwrap_or("?");
                    let file_count = args["files"].as_array().map(|a| a.len()).unwrap_or(0);
                    format!("Scaffold {} ({} files)", shorten_path(root), file_count)
                }
                _ => format!("{} …", name),
            };
            let icon = match name.as_str() {
                "read_file" | "grep" | "glob" | "list_directory" => "○",
                "edit_file" | "write_file" => "●",
                "bash" => "▸",
                _ => "·",
            };
            let color = match name.as_str() {
                "edit_file" | "write_file" => Color::Green,
                "bash" => Color::Cyan,
                _ => Color::DarkGray,
            };
            state.push_activity(icon, color, summary.clone());
            state.active_tool = Some(pipit_io::app::ActiveToolInfo {
                tool_name: name.clone(),
                args_summary: summary,
                started_at: std::time::Instant::now(),
            });
            state.begin_working(&format!("Running {}…", name));
        }
        AgentEvent::ToolCallEnd { name, result, .. } => {
            state.finish_working();
            let tool_name = name.clone();
            state.active_tool = None;
            match result {
                pipit_core::ToolCallOutcome::Success {
                    content,
                    mutated: true,
                    ..
                } => {
                    // Show the file operation result in content pane
                    // Distinguish Created vs Updated from the write_file result
                    let (icon, color) = if content.starts_with("Created") {
                        ("+", Color::Green)
                    } else if content.starts_with("Updated") {
                        ("~", Color::Yellow)
                    } else {
                        ("●", Color::Green)
                    };
                    state.push_activity(
                        icon,
                        color,
                        mutation_activity_summary(&tool_name, content),
                    );
                    state.ensure_turn_separator();

                    if tool_name == "edit_file" {
                        if let Some((path, diff)) = parse_applied_edit_content(content) {
                            state.content_lines.push(format!("### Edited `{}`", path));
                            state.content_lines.push(String::new());
                            push_content_block(&mut state.content_lines, diff);
                            state.content_lines.push(String::new());
                            return;
                        }
                    }

                    push_content_block(
                        &mut state.content_lines,
                        &format!("  {} {}", icon, content),
                    );
                }
                pipit_core::ToolCallOutcome::Success { mutated: false, .. } => {
                    // Keep tool chatter in the activity stream so the response pane
                    // can stay focused on the assistant's markdown answer.
                }
                pipit_core::ToolCallOutcome::Error { message } => {
                    let msg = if message.len() > 100 {
                        format!("{}…", &message.chars().take(100).collect::<String>())
                    } else {
                        message.clone()
                    };
                    state.push_activity("✗", Color::Red, format!("{}: {}", tool_name, msg));
                    state.ensure_turn_separator();
                    push_content_block(
                        &mut state.content_lines,
                        &format!("  ✗ {} failed: {}", tool_name, msg),
                    );
                }
                pipit_core::ToolCallOutcome::PolicyBlocked { message, .. } => {
                    let msg = if message.len() > 80 {
                        format!("{}…", &message.chars().take(80).collect::<String>())
                    } else {
                        message.clone()
                    };
                    state.push_activity(
                        "⚠",
                        Color::Yellow,
                        format!("{} blocked: {}", tool_name, msg),
                    );
                }
            }
        }
        AgentEvent::TokenUsageUpdate { used, limit, cost } => {
            state.status.tokens_used = *used;
            state.status.tokens_limit = *limit;
            state.status.cost = *cost;
        }
        AgentEvent::PlanSelected {
            strategy,
            rationale,
            ..
        } => {
            state.push_activity("◆", Color::Blue, format!("{} — {}", strategy, rationale));
        }
        AgentEvent::LoopDetected { tool_name, count } => {
            state.push_activity(
                "⚠",
                Color::Yellow,
                format!("{} repeated {}×", tool_name, count),
            );
        }
        AgentEvent::PhaseTransition { to, mode, .. } => {
            state.push_activity("◇", Color::Magenta, format!("{} · {}", mode, to));
            state.begin_working(&format!("{}…", to));
        }
        AgentEvent::VerifierVerdict {
            verdict,
            confidence,
            ..
        } => {
            let color = match verdict.as_str() {
                "PASS" => Color::Green,
                "REPAIRABLE" => Color::Yellow,
                _ => Color::Red,
            };
            state.push_activity(
                "◈",
                color,
                format!("verify: {} ({:.0}%)", verdict, confidence),
            );
        }
        AgentEvent::RepairStarted { attempt, reason } => {
            state.push_activity(
                "↻",
                Color::Yellow,
                format!("repair #{}: {}", attempt, reason),
            );
            state.begin_working("Repairing…");
        }
        AgentEvent::Waiting { label } => {
            state.begin_working(label);
        }
        AgentEvent::ThinkingDelta { .. } => {
            // Provider is sending dedicated thinking events (e.g. Anthropic extended thinking).
            // Mark as thinking so the content pane shows the reasoning animation.
            if !state.is_thinking {
                state.is_thinking = true;
                if state.working_since.is_none() {
                    state.working_since = Some(std::time::Instant::now());
                }
            }
        }
        AgentEvent::TurnEnd { turn_number, .. } => {
            state.finish_working();
            state.push_activity(
                "·",
                Color::DarkGray,
                format!("turn {} complete", turn_number),
            );
        }
        AgentEvent::BudgetExtended { new_approved } => {
            state.max_turns = *new_approved;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_agent_event, mutation_activity_summary, normalize_response_markdown,
        parse_applied_edit_content, push_content_block,
    };
    use pipit_config::ApprovalMode;
    use pipit_core::{AgentEvent, ToolCallOutcome, TurnEndReason};
    use pipit_io::{StatusBarState, app::TuiState};
    use serde_json::json;
    use std::path::PathBuf;

    fn test_state() -> TuiState {
        TuiState::new(
            StatusBarState::new(
                "repo".to_string(),
                "model".to_string(),
                ApprovalMode::Suggest,
            ),
            PathBuf::from("."),
        )
    }

    #[test]
    fn parses_applied_edit_content() {
        let content = "Applied edit to frontend/src/pages/Booking.tsx:\n--- a/frontend/src/pages/Booking.tsx\n+++ b/frontend/src/pages/Booking.tsx";
        let (path, diff) = parse_applied_edit_content(content).expect("edit content should parse");
        assert_eq!(path, "frontend/src/pages/Booking.tsx");
        assert!(diff.starts_with("--- a/frontend/src/pages/Booking.tsx"));
    }

    #[test]
    fn splits_multiline_blocks_into_lines() {
        let mut lines = Vec::new();
        push_content_block(&mut lines, "first\nsecond\n");
        assert_eq!(lines, vec!["first", "second", ""]);
    }

    #[test]
    fn edit_file_activity_summary_is_concise() {
        let summary = mutation_activity_summary(
            "edit_file",
            "Applied edit to /Users/test/project/frontend/src/pages/Booking.tsx:\n--- a/file\n+++ b/file",
        );
        assert_eq!(summary, "Edited src/pages/Booking.tsx");
    }

    #[test]
    fn content_separator_is_markdown_friendly() {
        let mut state = test_state();
        state.content_lines.push("First response".to_string());
        state.begin_turn(2);
        state.ensure_turn_separator();
        assert_eq!(state.content_lines, vec!["First response", "", "---", ""]);
    }

    #[test]
    fn tool_turn_discards_planning_chatter_from_response_pane() {
        let mut state = test_state();

        apply_agent_event(&mut state, &AgentEvent::TurnStart { turn_number: 1 });
        apply_agent_event(
            &mut state,
            &AgentEvent::ContentDelta {
                text: "I need to check the package.json files first.".to_string(),
            },
        );
        apply_agent_event(
            &mut state,
            &AgentEvent::ToolCallStart {
                call_id: "call-1".to_string(),
                name: "read_file".to_string(),
                args: json!({
                    "path": "frontend/package.json",
                    "start_line": 1,
                    "end_line": 40
                }),
            },
        );

        assert!(state.streaming_text.is_empty());
        assert!(state.content_lines.is_empty());

        apply_agent_event(
            &mut state,
            &AgentEvent::ToolCallEnd {
                call_id: "call-1".to_string(),
                name: "read_file".to_string(),
                result: ToolCallOutcome::Success {
                    content: "{\n  \"scripts\": { \"dev\": \"vite\" }\n}".to_string(),
                    mutated: false,
                },
            },
        );
        apply_agent_event(
            &mut state,
            &AgentEvent::TurnEnd {
                turn_number: 1,
                reason: TurnEndReason::ToolsExecuted,
            },
        );

        apply_agent_event(&mut state, &AgentEvent::TurnStart { turn_number: 2 });
        apply_agent_event(
            &mut state,
            &AgentEvent::ContentDelta {
                text: "Use `npm run dev` from `frontend`.".to_string(),
            },
        );
        apply_agent_event(
            &mut state,
            &AgentEvent::ContentComplete {
                full_text: "Use `npm run dev` from `frontend`.".to_string(),
            },
        );

        assert_eq!(
            state.content_lines,
            vec!["Use `npm run dev` from `frontend`."]
        );
    }

    #[test]
    fn tool_only_turn_does_not_leave_duplicate_separator_before_next_answer() {
        let mut state = test_state();

        apply_agent_event(&mut state, &AgentEvent::TurnStart { turn_number: 1 });
        apply_agent_event(
            &mut state,
            &AgentEvent::ContentDelta {
                text: "First answer".to_string(),
            },
        );
        apply_agent_event(
            &mut state,
            &AgentEvent::ContentComplete {
                full_text: "First answer".to_string(),
            },
        );
        apply_agent_event(
            &mut state,
            &AgentEvent::TurnEnd {
                turn_number: 1,
                reason: TurnEndReason::Complete,
            },
        );

        apply_agent_event(&mut state, &AgentEvent::TurnStart { turn_number: 2 });
        apply_agent_event(
            &mut state,
            &AgentEvent::ContentDelta {
                text: "I need to inspect something.".to_string(),
            },
        );
        apply_agent_event(
            &mut state,
            &AgentEvent::ToolCallStart {
                call_id: "call-2".to_string(),
                name: "read_file".to_string(),
                args: json!({ "path": "package.json" }),
            },
        );
        apply_agent_event(
            &mut state,
            &AgentEvent::ToolCallEnd {
                call_id: "call-2".to_string(),
                name: "read_file".to_string(),
                result: ToolCallOutcome::Success {
                    content: "{}".to_string(),
                    mutated: false,
                },
            },
        );
        apply_agent_event(
            &mut state,
            &AgentEvent::TurnEnd {
                turn_number: 2,
                reason: TurnEndReason::ToolsExecuted,
            },
        );

        apply_agent_event(&mut state, &AgentEvent::TurnStart { turn_number: 3 });
        apply_agent_event(
            &mut state,
            &AgentEvent::ContentDelta {
                text: "Second answer".to_string(),
            },
        );
        apply_agent_event(
            &mut state,
            &AgentEvent::ContentComplete {
                full_text: "Second answer".to_string(),
            },
        );

        assert_eq!(
            state.content_lines,
            vec![
                "First answer".to_string(),
                "".to_string(),
                "---".to_string(),
                "".to_string(),
                "Second answer".to_string(),
            ]
        );
    }

    #[test]
    fn normalize_response_markdown_repairs_inline_blocks_and_strips_worklog() {
        let raw = "The user is asking me to verify the schema details and I already read the file.\n\nThe backend schema details are present. ## User Schema - id: string - email: string\n| Schema | Fields | Status ||------|--------|--------|| User | id, email | Complete |";

        let cleaned = normalize_response_markdown(raw);

        assert!(!cleaned.contains("The user is asking me"));
        assert!(cleaned.contains("The backend schema details are present."));
        assert!(cleaned.contains("## User Schema"));
        assert!(cleaned.contains("- id: string"));
        assert!(cleaned.contains("- email: string"));
        assert!(cleaned.contains("| Schema | Fields | Status |"));
        assert!(cleaned.contains("|------|--------|--------|"));
    }

    #[test]
    fn content_complete_replaces_streamed_chatter_with_clean_markdown() {
        let mut state = test_state();

        apply_agent_event(&mut state, &AgentEvent::TurnStart { turn_number: 1 });
        apply_agent_event(
            &mut state,
            &AgentEvent::ContentDelta {
                text: "The user is asking me to verify the schema details.".to_string(),
            },
        );
        apply_agent_event(
            &mut state,
            &AgentEvent::ContentComplete {
                full_text: "The user is asking me to verify the schema details.\n\nThe backend schema details are present. ## User Schema - id: string - email: string".to_string(),
            },
        );

        assert_eq!(
            state.content_lines,
            vec![
                "The backend schema details are present.".to_string(),
                "".to_string(),
                "## User Schema".to_string(),
                "- id: string".to_string(),
                "- email: string".to_string(),
            ]
        );
    }
}
