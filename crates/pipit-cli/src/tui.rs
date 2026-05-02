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

/// Commands sent from the TUI event loop to the agent task.
enum TuiCommand {
    /// A user prompt to send to the agent.
    Prompt(String),
    /// Switch provider: (provider_kind, model, api_key, base_url).
    SwitchProvider {
        kind: pipit_config::ProviderKind,
        model: String,
        api_key: String,
        base_url: Option<String>,
        label: String,
    },
}

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
        Registry(Some(s)) => format!("registry {}", s),
        Registry(None) => "registry".to_string(),
        Vim => "vim".to_string(),
        Provider(Some(s)) => format!("provider {}", s),
        Provider(None) => "provider".to_string(),
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
    tmux_enabled: bool,
    vim_mode: bool,
    provider_roster: pipit_config::provider_roster::ProviderRoster,
) -> Result<()> {
    use crossterm::event::{self as crossterm_event, Event};

    dbg_log("[tui] entering tui::run()");
    let _ = extensions.on_session_start().await;
    dbg_log("[tui] on_session_start done");

    // ── Tmux bridge initialization ──
    let tmux_session = if tmux_enabled {
        if !pipit_tmux::is_tmux_available() {
            // Will show in the Agents tab as "not installed".
            None
        } else {
            match pipit_tmux::TmuxSession::create(project_root, None) {
                Ok(session) => {
                    dbg_log(&format!("[tui] tmux session created: {}", session.name()));
                    Some(session)
                }
                Err(e) => {
                    dbg_log(&format!("[tui] tmux session creation failed: {}", e));
                    None
                }
            }
        }
    } else {
        None
    };

    // Shared bridge for mirroring bash commands to the tmux shell pane.
    let tmux_bridge: Option<Arc<std::sync::Mutex<pipit_tmux::TmuxBridge>>> =
        if tmux_session.is_some() {
            Some(Arc::new(std::sync::Mutex::new(pipit_tmux::TmuxBridge::new())))
        } else {
            None
        };
    let tmux_shell_pane_id: Option<String> = tmux_session
        .as_ref()
        .and_then(|s| s.shell_pane().map(|p| p.to_string()));

    let tui_state = Arc::new(std::sync::Mutex::new(TuiState::new(
        status,
        project_root.clone(),
    )));
    dbg_log("[tui] TuiState created, calling init_terminal…");
    let mut terminal = app::init_terminal().context("Failed to init TUI")?;
    dbg_log("[tui] init_terminal OK (alternate screen active)");

    // Set agent mode and tmux state
    {
        let mut state = tui_state.lock().unwrap();
        state.agent_mode = agent_mode.to_string();
        if vim_mode {
            state.composer.enable_vim();
        }
        state.tmux_state.tmux_available = pipit_tmux::is_tmux_available();
        if let Some(ref session) = tmux_session {
            state.tmux_state.enabled = true;
            state.tmux_state.session_name = Some(session.name().to_string());
            // Populate initial pane snapshots.
            if let Ok(panes) = session.list_panes() {
                state.tmux_state.panes = panes
                    .into_iter()
                    .map(|p| pipit_io::app::TmuxPaneSnapshot {
                        pane_id: p.id,
                        role: p.role.to_string(),
                        width: p.width,
                        height: p.height,
                        current_command: p.current_command,
                        current_path: p.current_path.to_string_lossy().to_string(),
                        is_active: p.is_active,
                    })
                    .collect();
            }
        } else if tmux_enabled {
            // --tmux was requested but session creation failed.
            state.push_activity(
                "⚠",
                Color::Yellow,
                "tmux session creation failed — running without tmux".to_string(),
            );
        }
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

    // Provider roster wrapped in Arc<Mutex> for shared access between
    // the TUI event loop (listing) and the agent task (switching)
    let provider_roster = Arc::new(std::sync::Mutex::new(provider_roster));
    let roster_for_agent = provider_roster.clone();

    // Channel for sending prompts to the agent task
    let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::channel::<TuiCommand>(8);

    // Shared cancellation token — Escape key cancels the current run
    let cancel_token: Arc<std::sync::Mutex<CancellationToken>> =
        Arc::new(std::sync::Mutex::new(CancellationToken::new()));
    let cancel_for_agent = cancel_token.clone();

    // Spawn agent runner as a separate task so the TUI keeps redrawing
    let tui_state_for_agent = tui_state.clone();
    let agent_handle = tokio::spawn(async move {
        while let Some(cmd) = prompt_rx.recv().await {
            match cmd {
                TuiCommand::Prompt(prompt) => {
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
                    AgentOutcome::Completed { cost, .. } => {
                        s.push_activity(
                            "✓",
                            Color::Green,
                            format!("Done — ${:.4}", cost),
                        );
                        s.completion_status = Some(pipit_io::app::CompletionBanner {
                            icon: "✓".to_string(),
                            message: format!("Completed — ${:.4}", cost),
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
                } // end TuiCommand::Prompt
                TuiCommand::SwitchProvider { kind, model, api_key, base_url, label } => {
                    match agent.set_model(kind, &model, &api_key, base_url.as_deref()) {
                        Ok(()) => {
                            let mut s = tui_state_for_agent.lock().unwrap();
                            s.status.model = label.clone();
                            s.status.provider_kind = format!("{}", kind);
                            if let Some(ref url) = base_url {
                                s.status.base_url = url.clone();
                            }
                            s.push_activity("✓", Color::Green, format!("Switched to: {}", label));
                        }
                        Err(e) => {
                            let mut s = tui_state_for_agent.lock().unwrap();
                            s.push_activity("✗", Color::Red, format!("Provider switch failed: {}", e));
                            // Revert the roster to previous position
                            let mut roster = roster_for_agent.lock().unwrap();
                            roster.prev();
                        }
                    }
                }
            } // end match cmd
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

    // Track the working directory for `!` shell passthrough commands.
    // `cd` is intercepted and updates this; all other `!` commands run in it.
    let shell_cwd = Arc::new(std::sync::Mutex::new(project_root.clone()));

    loop {
        // ── Phase 1: Drain agent events (batch) ──
        // Process ALL pending events before drawing — this prevents
        // the "frozen" feeling when events pile up.
        let mut events_processed = 0u32;
        while let Ok(event) = agent_event_rx.try_recv() {
            // Mirror bash commands to the tmux shell pane for live visibility.
            if let Some(ref bridge) = tmux_bridge {
                if let Some(ref pane_id) = tmux_shell_pane_id {
                    mirror_to_tmux(&event, bridge, pane_id);
                }
            }
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
                    // Cap at 1MB to prevent OOM from huge pastes
                    let capped = if text.len() > 1_048_576 {
                        &text[..1_048_576]
                    } else {
                        &text
                    };
                    state.composer.handle_paste(capped);
                }
                Event::Key(key) => {
                    let mut state = tui_state.lock().unwrap();
                    app::handle_key(&mut state, key);

                    if state.should_quit {
                        break; // Exit poll loop — Phase 3 handles quit
                    }

                    if state.take_kill_active_subagents_requested() {
                        let mut token = cancel_token.lock().unwrap();
                        token.cancel();
                        *token = CancellationToken::new();
                        state.push_activity(
                            "⏹",
                            Color::Yellow,
                            "Kill requested for active subagents".to_string(),
                        );
                        state.begin_working("Stopping subagents…");
                    }

                    // Escape always cancels the current agent run.
                    if key.code == crossterm::event::KeyCode::Esc
                        && state.is_working
                    {
                        let mut token = cancel_token.lock().unwrap();
                        token.cancel();
                        *token = CancellationToken::new();
                        state.finish_working();
                        state.push_activity("⏹", Color::Yellow, "Stopped".to_string());
                    }

                    // Check if composer submitted input
                    if let Some(submitted) = state.composer.submitted.take() {
                        let mut input = submitted.text.clone();

                        // Merge pasted-text attachments into the prompt by
                        // reading their temp files and prepending to context.
                        let mut pasted_context = String::new();
                        let mut file_attachments: Vec<String> = Vec::new();
                        let mut image_attachments: Vec<String> = Vec::new();
                        for att in &submitted.attachments {
                            match att.kind {
                                pipit_io::composer::AttachmentKind::PastedText => {
                                    if let Ok(content) = std::fs::read_to_string(&att.path) {
                                        // Cap at 128KB per pasted attachment
                                        let capped = if content.len() > 131_072 {
                                            format!("{}…\n[truncated at 128KB]", &content[..131_072])
                                        } else {
                                            content
                                        };
                                        pasted_context.push_str(&format!(
                                            "<pasted_text>\n{}\n</pasted_text>\n\n",
                                            capped
                                        ));
                                    }
                                    // Clean up temp file
                                    let _ = std::fs::remove_file(&att.path);
                                }
                                pipit_io::composer::AttachmentKind::File => {
                                    file_attachments.push(att.path.clone());
                                }
                                pipit_io::composer::AttachmentKind::Image => {
                                    image_attachments.push(att.path.clone());
                                }
                            }
                        }
                        if !pasted_context.is_empty() {
                            input = format!(
                                "{}{}\n{}",
                                pasted_context,
                                if input.is_empty() { "Analyze the pasted text above." } else { "" },
                                input
                            );
                        }
                        if !file_attachments.is_empty() {
                            input = format!(
                                "First read these files: {}. Then: {}",
                                file_attachments.join(", "),
                                input
                            );
                        }
                        if !image_attachments.is_empty() {
                            input = format!(
                                "Analyze these image files: {}. {}",
                                image_attachments.join(", "),
                                input
                            );
                        }

                        // Update task label for every submission
                        state.has_received_input = true;

                        let classified = pipit_io::input::classify_input(&input);

                        // Build a clean display string that replaces raw paths
                        // with short indicators like [Image #1] or [📎 file.rs]
                        let (display, task_label) = match &classified {
                            pipit_io::input::UserInput::PromptWithImages {
                                prompt,
                                image_paths,
                            } => {
                                let chips: Vec<String> = image_paths
                                    .iter()
                                    .enumerate()
                                    .map(|(i, p)| {
                                        let name = std::path::Path::new(p)
                                            .file_name()
                                            .and_then(|n| n.to_str())
                                            .unwrap_or("image");
                                        format!("[🖼 #{} {}]", i + 1, name)
                                    })
                                    .collect();
                                let chip_str = chips.join(" ");
                                let display_text = if prompt.is_empty() {
                                    chip_str.clone()
                                } else {
                                    let short_prompt = if prompt.len() > 80 {
                                        format!(
                                            "{}…",
                                            &prompt.chars().take(78).collect::<String>()
                                        )
                                    } else {
                                        prompt.clone()
                                    };
                                    format!("{} {}", chip_str, short_prompt)
                                };
                                let label = if prompt.is_empty() {
                                    format!(
                                        "{} image(s) attached",
                                        image_paths.len()
                                    )
                                } else if prompt.len() > 80 {
                                    format!(
                                        "{}…",
                                        &prompt.chars().take(78).collect::<String>()
                                    )
                                } else {
                                    prompt.clone()
                                };
                                (display_text, label)
                            }
                            pipit_io::input::UserInput::PromptWithFiles {
                                prompt,
                                files,
                            } => {
                                let chips: Vec<String> = files
                                    .iter()
                                    .map(|p| {
                                        let name = std::path::Path::new(p)
                                            .file_name()
                                            .and_then(|n| n.to_str())
                                            .unwrap_or(p);
                                        format!("[📎 {}]", name)
                                    })
                                    .collect();
                                let chip_str = chips.join(" ");
                                let short_prompt = if prompt.len() > 80 {
                                    format!(
                                        "{}…",
                                        &prompt.chars().take(78).collect::<String>()
                                    )
                                } else {
                                    prompt.clone()
                                };
                                let display_text =
                                    format!("{} {}", chip_str, short_prompt);
                                let label = if prompt.len() > 80 {
                                    format!(
                                        "{}…",
                                        &prompt.chars().take(78).collect::<String>()
                                    )
                                } else {
                                    prompt.clone()
                                };
                                (display_text, label)
                            }
                            _ => {
                                let display_text = if input.len() > 120 {
                                    format!(
                                        "{}… [{} chars]",
                                        &input.chars().take(100).collect::<String>(),
                                        input.chars().count()
                                    )
                                } else {
                                    input.clone()
                                };
                                let label = if input.len() > 80 {
                                    format!(
                                        "{}…",
                                        &input.chars().take(78).collect::<String>()
                                    )
                                } else {
                                    input.clone()
                                };
                                (display_text, label)
                            }
                        };

                        state.task_label = task_label;
                        state.inject_user_prompt(&input);
                        state.push_activity("›", Color::Green, display);

                        drop(state); // Release lock before async

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
                                        s.active_tab = pipit_io::app::TabView::Help;
                                        s.side_tab_scroll_offset = 0;
                                        s.has_received_input = true;
                                    }
                                    pipit_io::input::SlashCommand::Clear => {
                                        let _ = prompt_tx.send(TuiCommand::Prompt("/clear".to_string())).await;
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
                                        {
                                            let mut s = tui_state.lock().unwrap();
                                            s.push_activity(
                                                "⚙",
                                                Color::Yellow,
                                                "/setup".to_string(),
                                            );
                                        }

                                        // Temporarily leave the TUI to run the interactive setup wizard
                                        app::restore_terminal(&mut terminal)?;

                                        let setup_result = crate::setup::run();

                                        // Re-enter the TUI
                                        terminal = app::init_terminal()
                                            .context("Failed to re-init TUI after /setup")?;
                                        needs_redraw = true;

                                        let mut s = tui_state.lock().unwrap();
                                        s.content_lines.clear();
                                        s.content_scroll_offset = 0;

                                        match setup_result {
                                            Ok(()) => {
                                                s.push_activity(
                                                    "✓",
                                                    Color::Green,
                                                    "Setup complete — restart pipit to apply changes".to_string(),
                                                );
                                                s.content_lines
                                                    .push("## Setup Complete".to_string());
                                                s.content_lines.push(String::new());
                                                s.content_lines.push(
                                                    "Configuration saved. **Restart pipit** to apply the new settings.".to_string(),
                                                );
                                                s.content_lines.push(String::new());
                                                s.content_lines.push(
                                                    "Press `Ctrl-C` or `/quit` to exit, then run `pipit` again.".to_string(),
                                                );
                                            }
                                            Err(e) => {
                                                s.push_activity(
                                                    "✗",
                                                    Color::Red,
                                                    format!("Setup failed: {}", e),
                                                );
                                                s.content_lines.push("## Setup Failed".to_string());
                                                s.content_lines.push(String::new());
                                                s.content_lines.push(format!("Error: {}", e));
                                            }
                                        }

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
                                        let _ = prompt_tx.send(TuiCommand::Prompt("/skills".to_string())).await;
                                    }
                                    pipit_io::input::SlashCommand::Hooks => {
                                        let _ = prompt_tx.send(TuiCommand::Prompt("/hooks".to_string())).await;
                                    }
                                    pipit_io::input::SlashCommand::Mcp => {
                                        let _ = prompt_tx.send(TuiCommand::Prompt("/mcp".to_string())).await;
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
                                            let _ = prompt_tx.send(TuiCommand::Prompt(
                                                "Run `git diff --cached` and generate a conventional commit message \
                                                 (type(scope): description). Then run `git commit -m \"<your message>\"` \
                                                 to commit. Do NOT use --no-edit.".to_string()
                                            )).await;
                                        }
                                    }
                                    pipit_io::input::SlashCommand::Vim => {
                                        let mut s = tui_state.lock().unwrap();
                                        if s.composer.vim_active() {
                                            s.composer.disable_vim();
                                            s.push_activity("⌨", Color::Yellow, "Vim mode OFF".to_string());
                                        } else {
                                            s.composer.enable_vim();
                                            s.push_activity("⌨", Color::Green, "Vim mode ON (Esc → Normal, i → Insert)".to_string());
                                        }
                                    }
                                    pipit_io::input::SlashCommand::Provider(ref arg) => {
                                        let mut roster = provider_roster.lock().unwrap();
                                        match arg.as_deref() {
                                            None | Some("") | Some("list") => {
                                                // Show provider list in the content pane
                                                let mut s = tui_state.lock().unwrap();
                                                s.push_activity("⚙", Color::Cyan, "/provider".to_string());
                                                s.content_lines.clear();
                                                s.content_scroll_offset = 0;
                                                s.content_lines.push("## Provider Roster".to_string());
                                                s.content_lines.push(String::new());
                                                for line in roster.render_list().lines() {
                                                    s.content_lines.push(line.to_string());
                                                }
                                                s.content_lines.push(String::new());
                                                s.content_lines.push("Usage: `/provider next` · `/provider prev` · `/provider <name>`".to_string());
                                                s.has_received_input = true;
                                                s.ui_mode = pipit_io::app::UiMode::Task;
                                            }
                                            Some("next" | "n") => {
                                                let profile = roster.next().clone();
                                                let label = roster.status_label();
                                                drop(roster);
                                                let _ = prompt_tx.send(TuiCommand::SwitchProvider {
                                                    kind: profile.kind,
                                                    model: profile.model,
                                                    api_key: profile.api_key,
                                                    base_url: profile.base_url,
                                                    label,
                                                }).await;
                                            }
                                            Some("prev" | "p") => {
                                                let profile = roster.prev().clone();
                                                let label = roster.status_label();
                                                drop(roster);
                                                let _ = prompt_tx.send(TuiCommand::SwitchProvider {
                                                    kind: profile.kind,
                                                    model: profile.model,
                                                    api_key: profile.api_key,
                                                    base_url: profile.base_url,
                                                    label,
                                                }).await;
                                            }
                                            Some(query) => {
                                                // Try numeric index first, then label match
                                                let result = if let Ok(idx) = query.parse::<usize>() {
                                                    roster.switch_to_index(idx).cloned()
                                                } else {
                                                    roster.switch_to(query).cloned()
                                                };
                                                match result {
                                                    Ok(profile) => {
                                                        let label = roster.status_label();
                                                        drop(roster);
                                                        let _ = prompt_tx.send(TuiCommand::SwitchProvider {
                                                            kind: profile.kind,
                                                            model: profile.model,
                                                            api_key: profile.api_key,
                                                            base_url: profile.base_url,
                                                            label,
                                                        }).await;
                                                    }
                                                    Err(e) => {
                                                        drop(roster);
                                                        let mut s = tui_state.lock().unwrap();
                                                        s.push_activity("✗", Color::Red, e);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    other => {
                                        let cmd_str = format!("/{}", slash_command_to_str(&other));
                                        let _ = prompt_tx.send(TuiCommand::Prompt(cmd_str)).await;
                                    }
                                }
                            }
                            pipit_io::input::UserInput::Prompt(prompt) => {
                                let _ = prompt_tx.send(TuiCommand::Prompt(prompt)).await;
                            }
                            pipit_io::input::UserInput::ShellPassthrough(cmd) => {
                                // Push to composer's shell history for !-completion
                                {
                                    let mut s = tui_state.lock().unwrap();
                                    s.composer.push_shell_history(&cmd);
                                    s.push_activity("$", Color::Green, format!("$ {}", cmd));
                                }

                                // Intercept `cd` to persist directory changes
                                let trimmed = cmd.trim();
                                if trimmed == "cd"
                                    || (trimmed.starts_with("cd ")
                                        && !trimmed.contains("&&")
                                        && !trimmed.contains(';')
                                        && !trimmed.contains('|'))
                                {
                                    let current = shell_cwd.lock().unwrap().clone();
                                    let target = if trimmed == "cd" {
                                        std::env::var("HOME")
                                            .map(std::path::PathBuf::from)
                                            .unwrap_or_else(|_| project_root.clone())
                                    } else {
                                        let arg = trimmed.strip_prefix("cd ").unwrap().trim();
                                        let arg = arg.trim_matches('"').trim_matches('\'');
                                        let expanded = if arg.starts_with("~/") || arg == "~" {
                                            if let Ok(home) = std::env::var("HOME") {
                                                std::path::PathBuf::from(home)
                                                    .join(arg.strip_prefix("~/").unwrap_or(""))
                                            } else {
                                                std::path::PathBuf::from(arg)
                                            }
                                        } else {
                                            std::path::PathBuf::from(arg)
                                        };
                                        if expanded.is_absolute() {
                                            expanded
                                        } else {
                                            current.join(&expanded)
                                        }
                                    };
                                    let mut s = tui_state.lock().unwrap();
                                    s.content_lines.push(String::new());
                                    s.content_lines.push(format!("$ {}", cmd));
                                    match target.canonicalize() {
                                        Ok(resolved) if resolved.is_dir() => {
                                            *shell_cwd.lock().unwrap() = resolved.clone();
                                            s.content_lines.push(format!(
                                                "Changed directory to {}",
                                                resolved.display()
                                            ));
                                            s.push_activity("✓", Color::Green, "done".to_string());
                                        }
                                        Ok(resolved) => {
                                            s.content_lines.push(format!(
                                                "cd: {}: Not a directory",
                                                resolved.display()
                                            ));
                                            s.push_activity("✗", Color::Red, "not a directory".to_string());
                                        }
                                        Err(e) => {
                                            s.content_lines.push(format!("cd: {}: {}", target.display(), e));
                                            s.push_activity("✗", Color::Red, format!("cd: {}", e));
                                        }
                                    }
                                    s.has_received_input = true;
                                    s.ui_mode = pipit_io::app::UiMode::Task;
                                    s.auto_scroll_content();
                                } else {
                                    // Execute in the tracked shell_cwd
                                    let cwd = shell_cwd.lock().unwrap().clone();
                                    let output = tokio::process::Command::new("sh")
                                        .arg("-c")
                                        .arg(&cmd)
                                        .current_dir(&cwd)
                                        .output()
                                        .await;
                                    let mut s = tui_state.lock().unwrap();
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
                            }
                            pipit_io::input::UserInput::PromptWithFiles { prompt, files } => {
                                let enriched = format!(
                                    "First read these files: {}. Then: {}",
                                    files.join(", "),
                                    prompt
                                );
                                let _ = prompt_tx.send(TuiCommand::Prompt(enriched)).await;
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
                                let _ = prompt_tx.send(TuiCommand::Prompt(enriched)).await;
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

            // Refresh tmux pane snapshots periodically when Agents tab is visible.
            if state.tmux_state.enabled && state.active_tab == pipit_io::app::TabView::Agents {
                if let Some(ref session) = tmux_session {
                    // Refresh every ~2s (every 40 frames at 20fps).
                    if state.spinner_frame % 40 == 0 {
                        if let Ok(panes) = session.list_panes() {
                            state.tmux_state.panes = panes
                                .into_iter()
                                .map(|p| pipit_io::app::TmuxPaneSnapshot {
                                    pane_id: p.id,
                                    role: p.role.to_string(),
                                    width: p.width,
                                    height: p.height,
                                    current_command: p.current_command,
                                    current_path: p.current_path
                                        .to_string_lossy()
                                        .to_string(),
                                    is_active: p.is_active,
                                })
                                .collect();
                            needs_redraw = true;
                        }
                    }
                }
            }

            if needs_redraw && state.should_redraw() {
                state.spinner_frame = state.spinner_frame.wrapping_add(1);
                state.tick_animations();
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

    // Log tmux session info for the user to attach later.
    if let Some(ref session) = tmux_session {
        if session.is_alive() {
            eprintln!(
                "\x1b[2mpipit› tmux session '{}' preserved — attach with: tmux attach -t {}\x1b[0m",
                session.name(),
                session.name()
            );
        }
    }

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
        Regex::new(r"([^\n])\s+(\|[^\n]*\|[^\n]*\|[^\n]*)").expect("valid markdown table regex")
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
    obvious_prefixes
        .iter()
        .any(|prefix| lower.starts_with(prefix))
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

/// Lightweight normalization applied to the streaming buffer on each delta.
/// Fixes structural formatting (inline headings, lists, tables) so the TUI
/// renders clean markdown while tokens are still arriving. Skips expensive
/// worklog stripping (that only works reliably on the full completed text).
fn normalize_streaming_display(text: &str) -> String {
    let mut cleaned = text.replace("\r\n", "\n").replace('\r', "\n");
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

/// Mirror agent events to the tmux shell pane for live visibility.
///
/// When --tmux is active, bash commands are typed into the shell pane so
/// the user can watch them execute. File operations and agent reasoning
/// are echoed as comments so the pane shows a full activity timeline.
fn mirror_to_tmux(
    event: &pipit_core::AgentEvent,
    bridge: &Arc<std::sync::Mutex<pipit_tmux::TmuxBridge>>,
    shell_pane_id: &str,
) {
    use pipit_core::AgentEvent;
    match event {
        AgentEvent::ToolCallStart { name, args, .. } => {
            let mut b = bridge.lock().unwrap();
            match name.as_str() {
                "bash" => {
                    if let Some(cmd) = args["command"].as_str() {
                        // Type the actual command into the tmux shell pane.
                        let _ = b.type_and_enter(shell_pane_id, cmd);
                    }
                }
                "write_file" | "edit_file" => {
                    let path = args["path"].as_str().unwrap_or("?");
                    let _ = b.type_and_enter(
                        shell_pane_id,
                        &format!("# pipit: {} {}", name, shorten_path(path)),
                    );
                }
                "read_file" | "grep" | "glob" | "list_directory" => {
                    // Skip read-only tools — too noisy.
                }
                _ => {
                    let _ = b.type_and_enter(
                        shell_pane_id,
                        &format!("# pipit: {}", name),
                    );
                }
            }
        }
        AgentEvent::TurnStart { turn_number } => {
            let mut b = bridge.lock().unwrap();
            let _ = b.type_and_enter(
                shell_pane_id,
                &format!("# ── turn {} ──", turn_number),
            );
        }
        _ => {}
    }
}

/// Pure function: map an AgentEvent to TuiState mutations.
/// Extracted from the inline closure for testability.
fn apply_agent_event(state: &mut TuiState, event: &pipit_core::AgentEvent) {
    use pipit_core::AgentEvent;
    match event {
        AgentEvent::TurnStart { turn_number } => {
            state.begin_turn(*turn_number);
            state.begin_working("Thinking");
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
            let mut pushed = false;
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
                        pushed = true;
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
                            pushed = true;
                        }
                        state.tag_buffer = suffix.to_string();
                    } else if !cleaned.trim().is_empty() {
                        state.push_content(&cleaned);
                        pushed = true;
                    }
                    break;
                }
            }
            // Normalize the streaming buffer so markdown renders cleanly
            // while tokens are still arriving (fixes inline headings, lists, tables).
            if pushed && !state.streaming_text.is_empty() {
                let normalized = normalize_streaming_display(&state.streaming_text);
                if normalized != state.streaming_text {
                    state.streaming_text.clear();
                    state.streaming_text.push_str(&normalized);
                }
            }
            // Feed stalled detector so it knows tokens are flowing
            if pushed {
                state.record_stream_tokens(text.len() as u64);
            }
        }
        AgentEvent::ContentComplete { full_text } => {
            state.tag_buffer.clear();
            // Don't replace — this would wipe out interleaved tool activity.
            // Just commit whatever streaming text is buffered.
            state.commit_streaming();
            state.finish_working();
        }
        AgentEvent::ToolCallStart {
            call_id,
            name,
            args,
        } => {
            state.finish_working();
            if !state.current_turn_had_tool_calls {
                state.current_turn_had_tool_calls = true;
                // Keep planning text — it interleaves with tool actions
                // like "I'm going to inspect..." before "● Ran ls"
            }
            if name == "subagent" {
                let task = args["task"].as_str().unwrap_or("Subagent task").to_string();
                let tools = args["tools"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.as_str().map(str::to_string))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                state.note_subagent_started(call_id.clone(), task, tools);
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
                    "Edited {}",
                    shorten_path(args["path"].as_str().unwrap_or("?"))
                ),
                "write_file" => {
                    let path = args["path"].as_str().unwrap_or("?");
                    let content = args["content"].as_str().unwrap_or("");
                    let lines = content.lines().count();

                    // Show the code being written in the content pane
                    let ext = path
                        .rsplit('.')
                        .next()
                        .unwrap_or("");
                    state.ensure_turn_separator();
                    state.content_lines.push(format!("### `{}`", shorten_path(path)));
                    state.content_lines.push(String::new());
                    state.content_lines.push(format!("```{}", ext));
                    // Show up to 200 lines; truncate beyond that
                    let content_lines_vec: Vec<&str> = content.lines().collect();
                    let show_n = content_lines_vec.len().min(200);
                    for line in &content_lines_vec[..show_n] {
                        state.content_lines.push(line.to_string());
                    }
                    if content_lines_vec.len() > 200 {
                        state.content_lines.push(format!(
                            "... ({} more lines)",
                            content_lines_vec.len() - 200
                        ));
                    }
                    state.content_lines.push("```".to_string());
                    state.content_lines.push(String::new());
                    state.invalidate_content_cache();

                    format!("Wrote {} ({} lines)", shorten_path(path), lines)
                }
                "multi_edit" => format!(
                    "Edited {}",
                    shorten_path(args["path"].as_str().unwrap_or("?"))
                ),
                "bash" => {
                    let cmd = args["command"].as_str().unwrap_or("?");
                    format!("Ran {}", cmd.chars().take(72).collect::<String>())
                }
                "grep" => format!(
                    "Searched '{}'",
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
                    format!("Listed {}", shorten_path(args["path"].as_str().unwrap_or(".")))
                }
                "scaffold_project" => {
                    let root = args["project_root"].as_str().unwrap_or("?");
                    let file_count = args["files"].as_array().map(|a| a.len()).unwrap_or(0);
                    format!("Scaffolded {} ({} files)", shorten_path(root), file_count)
                }
                "subagent" => format!(
                    "Subagent {}",
                    args["task"]
                        .as_str()
                        .unwrap_or("task")
                        .chars()
                        .take(60)
                        .collect::<String>()
                ),
                _ => format!("{} …", name),
            };
            let icon = match name.as_str() {
                "read_file" | "grep" | "glob" | "list_directory" => "○",
                "edit_file" | "write_file" | "multi_edit" => "●",
                "bash" => "●",
                "subagent" => "⇢",
                _ => "·",
            };
            let color = match name.as_str() {
                "edit_file" | "write_file" | "multi_edit" => Color::Green,
                "bash" => Color::Cyan,
                "subagent" => Color::Magenta,
                _ => Color::DarkGray,
            };
            state.push_activity(icon, color, summary.clone());
            state.active_tool = Some(pipit_io::app::ActiveToolInfo {
                tool_name: name.clone(),
                args_summary: summary,
                started_at: std::time::Instant::now(),
            });

            // Track bash commands in tmux state for the Agents tab.
            if name == "bash" && state.tmux_state.enabled {
                let cmd = args["command"].as_str().unwrap_or("?").to_string();
                let pane_id = state
                    .tmux_state
                    .panes
                    .iter()
                    .find(|p| p.role == "shell")
                    .map(|p| p.pane_id.clone())
                    .unwrap_or_default();
                state.tmux_state.recent_commands.push(
                    pipit_io::app::TmuxCommandEntry {
                        command: cmd,
                        exit_code: None,
                        duration_ms: None,
                        pane_id,
                    },
                );
                // Cap recent commands at 50.
                if state.tmux_state.recent_commands.len() > 50 {
                    state.tmux_state.recent_commands.remove(0);
                }
            }

            state.begin_working(&format!("Running {}…", name));
        }
        AgentEvent::ToolCallEnd {
            call_id,
            name,
            result,
            ..
        } => {
            state.finish_working();
            let tool_name = name.clone();
            state.active_tool = None;

            // Update tmux command tracking with exit code.
            if tool_name == "bash" && state.tmux_state.enabled {
                if let Some(entry) = state.tmux_state.recent_commands.last_mut() {
                    let exit_code = match result {
                        pipit_core::ToolCallOutcome::Success { .. } => Some(0),
                        pipit_core::ToolCallOutcome::Error { .. } => Some(1),
                        pipit_core::ToolCallOutcome::PolicyBlocked { .. } => Some(-1),
                    };
                    entry.exit_code = exit_code;
                }
            }

            match result {
                pipit_core::ToolCallOutcome::Success {
                    content,
                    mutated: true,
                    ..
                } => {
                    if tool_name == "edit_file" || tool_name == "multi_edit" {
                        // For edits: show the diff in content, skip activity dupe
                        if let Some((path, diff)) = parse_applied_edit_content(content) {
                            state.push_activity(
                                "~",
                                Color::Yellow,
                                format!("Edited {}", shorten_path(&path)),
                            );
                            state.ensure_turn_separator();
                            state.content_lines.push(format!("### Edited `{}`", path));
                            state.content_lines.push(String::new());
                            push_content_block(&mut state.content_lines, diff);
                            state.content_lines.push(String::new());
                            return;
                        }
                    }

                    // For write_file / scaffold: just push activity, no content dupe
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
                }
                pipit_core::ToolCallOutcome::Success {
                    content,
                    mutated: false,
                    ..
                } => {
                    if tool_name == "subagent" {
                        let summary = first_line(content).chars().take(96).collect::<String>();
                        state.note_subagent_finished(
                            call_id,
                            pipit_io::app::SubagentStatus::Completed,
                            Some(summary.clone()),
                        );
                        state.push_activity(
                            "✓",
                            Color::Magenta,
                            format!("Subagent completed: {}", summary),
                        );
                    } else if tool_name == "bash" {
                        // Show inline bash output in content pane
                        // Skip boilerplate "no output" messages
                        let is_noise = content.trim().is_empty()
                            || content.contains("Command completed successfully")
                            || content.contains("(no output)");
                        if !is_noise {
                            let lines: Vec<&str> = content.lines().collect();
                            let show = lines.len().min(5);
                            if show > 0 {
                                state.ensure_turn_separator();
                                for line in &lines[..show] {
                                    let truncated: String =
                                        line.chars().take(90).collect();
                                    state
                                        .content_lines
                                        .push(format!("◈activity◈  └ {}", truncated));
                                }
                            }
                            if lines.len() > show {
                                state.content_lines.push(format!(
                                    "◈activity◈  └ … {} more lines",
                                    lines.len() - show
                                ));
                            }
                            state.invalidate_content_cache();
                        }
                    } else if tool_name == "read_file"
                        || tool_name == "grep"
                        || tool_name == "list_directory"
                    {
                        // Show a compact preview of read results
                        let lines: Vec<&str> = content.lines().collect();
                        let show = lines.len().min(8);
                        if show > 0 {
                            for line in &lines[..show] {
                                let truncated: String =
                                    line.chars().take(90).collect();
                                state
                                    .content_lines
                                    .push(format!("◈activity◈  └ {}", truncated));
                            }
                        }
                        if lines.len() > show {
                            state.content_lines.push(format!(
                                "◈activity◈  └ … {} more lines",
                                lines.len() - show
                            ));
                        }
                        state.invalidate_content_cache();
                    }
                    // Read/grep/glob/list results are already shown via the
                    // ToolCallStart activity label — no separate result line needed.
                }
                pipit_core::ToolCallOutcome::Error { message } => {
                    let msg = if message.len() > 100 {
                        format!("{}…", &message.chars().take(100).collect::<String>())
                    } else {
                        message.clone()
                    };
                    if tool_name == "subagent" {
                        let status = if message.to_lowercase().contains("cancel") {
                            pipit_io::app::SubagentStatus::Cancelled
                        } else {
                            pipit_io::app::SubagentStatus::Failed
                        };
                        state.note_subagent_finished(call_id, status, Some(msg.clone()));
                    }
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
                    if tool_name == "subagent" {
                        state.note_subagent_finished(
                            call_id,
                            pipit_io::app::SubagentStatus::Failed,
                            Some(msg.clone()),
                        );
                    }
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
            // Turn transitions are intentionally silent in the UI.
            // The spinner + status bar already show working state.
            let _ = turn_number;
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
    fn tool_turn_keeps_planning_content_interleaved_with_actions() {
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
        // Planning text is now kept, interleaved with activity markers
        let has_planning = state.content_lines.iter().any(|l| l.contains("check the package.json"));
        let has_activity = state.content_lines.iter().any(|l| l.starts_with("◈activity◈"));
        assert!(has_planning, "Planning content should be kept");
        assert!(has_activity, "Activity markers should be present");

        apply_agent_event(
            &mut state,
            &AgentEvent::ToolCallEnd {
                call_id: "call-1".to_string(),
                name: "read_file".to_string(),
                result: ToolCallOutcome::Success {
                    content: "{\n  \"scripts\": { \"dev\": \"vite\" }\n}".to_string(),
                    mutated: false,
                    artifacts: Vec::new(),
                    edits: Vec::new(),
                },
                duration_ms: 0,
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

        // Turn 1 planning content + activity both preserved; turn 2 content follows separator
        let response_lines: Vec<&str> = state
            .content_lines
            .iter()
            .filter(|l| !l.starts_with("◈activity◈"))
            .map(|l| l.as_str())
            .collect();
        // Planning text is now kept (first line), separator, then turn 2 response
        assert!(response_lines.iter().any(|l| l.contains("check the package.json")));
        assert!(response_lines.iter().any(|l| l.contains("npm run dev")));
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
                    artifacts: Vec::new(),
                    edits: Vec::new(),
                },
                duration_ms: 0,
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

        // Activity markers from tool turn appear inline between answers
        let response_lines: Vec<&str> = state
            .content_lines
            .iter()
            .filter(|l| !l.starts_with("◈activity◈"))
            .map(|l| l.as_str())
            .collect();
        // Planning text from turn 2 is now kept alongside tool actions
        assert!(response_lines.iter().any(|l| l.contains("First answer")));
        assert!(response_lines.iter().any(|l| l.contains("inspect something")));
        assert!(response_lines.iter().any(|l| l.contains("Second answer")));
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
    fn content_complete_commits_streamed_content() {
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

        // Content is committed from streaming, not replaced with normalized full_text
        assert!(state.content_lines.iter().any(|l| l.contains("schema details")));
    }

    #[test]
    fn subagent_events_update_tracked_runs() {
        let mut state = test_state();

        apply_agent_event(
            &mut state,
            &AgentEvent::ToolCallStart {
                call_id: "sub-1".to_string(),
                name: "subagent".to_string(),
                args: json!({
                    "task": "Review auth middleware",
                    "tools": ["read_file", "grep"]
                }),
            },
        );

        assert_eq!(state.active_subagent_count(), 1);
        assert_eq!(state.subagent_runs.len(), 1);
        assert_eq!(state.subagent_runs[0].task, "Review auth middleware");

        apply_agent_event(
            &mut state,
            &AgentEvent::ToolCallEnd {
                call_id: "sub-1".to_string(),
                name: "subagent".to_string(),
                result: ToolCallOutcome::Success {
                    content: "Found the root cause in auth.rs".to_string(),
                    mutated: false,
                    artifacts: Vec::new(),
                    edits: Vec::new(),
                },
                duration_ms: 0,
            },
        );

        assert_eq!(state.active_subagent_count(), 0);
        assert_eq!(
            state.subagent_runs[0].status,
            pipit_io::app::SubagentStatus::Completed
        );
    }
}
