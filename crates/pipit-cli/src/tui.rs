//! Full-screen TUI mode — extracted from main.rs.
//!
//! Owns the ratatui terminal lifecycle, crossterm event dispatch,
//! agent event mapping, slash-command interpretation, and the
//! Composer-based input widget.

use anyhow::{Context, Result};
use pipit_core::{AgentLoop, AgentOutcome};
use pipit_io::app::{self, TuiState};
use pipit_io::StatusBarState;
use pipit_skills::SkillRegistry;
use ratatui::style::Color;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
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
        Model(s) => if s.is_empty() { "model".to_string() } else { format!("model {}", s) },
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
        Aside(s) => if s.is_empty() { "aside".to_string() } else { format!("aside {}", s) },
        Checkpoint(Some(s)) => format!("checkpoint {}", s),
        Checkpoint(None) => "checkpoint".to_string(),
        Tdd(Some(s)) => format!("tdd {}", s),
        Tdd(None) => "tdd".to_string(),
        CodeReview => "code-review".to_string(),
        BuildFix => "build-fix".to_string(),
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

    let tui_state = Arc::new(Mutex::new(TuiState::new(status, project_root.clone())));
    dbg_log("[tui] TuiState created, calling init_terminal…");
    let mut terminal = app::init_terminal().context("Failed to init TUI")?;
    dbg_log("[tui] init_terminal OK (alternate screen active)");

    // Set agent mode
    {
        let mut state = tui_state.lock().unwrap();
        state.agent_mode = agent_mode.to_string();
    }

    // Spawn agent event handler that updates TUI state
    let tui_state_for_events = tui_state.clone();
    let mut event_rx_owned = event_rx.resubscribe();
    let _event_handle = tokio::spawn(async move {
        use pipit_core::AgentEvent;
        while let Ok(event) = event_rx_owned.recv().await {
            let mut state = tui_state_for_events.lock().unwrap();
            apply_agent_event(&mut state, &event);
        }
    });

    // Channel for sending prompts to the agent task
    let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::channel::<String>(8);

    // Shared cancellation token — Escape key cancels the current run
    let cancel_token: Arc<Mutex<CancellationToken>> = Arc::new(Mutex::new(CancellationToken::new()));
    let cancel_for_agent = cancel_token.clone();

    // Spawn agent runner as a separate task so the TUI keeps redrawing
    let tui_state_for_agent = tui_state.clone();
    let agent_handle = tokio::spawn(async move {
        while let Some(prompt) = prompt_rx.recv().await {
            {
                let mut s = tui_state_for_agent.lock().unwrap();
                s.begin_working("Thinking…");
            }
            let cancel = cancel_for_agent.lock().unwrap().clone();
            let outcome = agent.run(prompt, cancel).await;
            {
                let mut s = tui_state_for_agent.lock().unwrap();
                s.finish_working();
                match &outcome {
                    AgentOutcome::Completed { turns, cost, .. } => {
                        s.push_activity("✓", Color::Green, format!("Done — {} turns, ${:.4}", turns, cost));
                    }
                    AgentOutcome::Error(e) => {
                        s.push_activity("✗", Color::Red, format!("Error: {}", e));
                    }
                    AgentOutcome::Cancelled => {
                        s.push_activity("·", Color::DarkGray, "Cancelled".to_string());
                    }
                    AgentOutcome::MaxTurnsReached(n) => {
                        s.push_activity("⚠", Color::Yellow, format!("Max turns ({})", n));
                    }
                }
            }
        }
    });

    dbg_log("[tui] spawned event handler + agent runner, entering main loop");

    // Main TUI event loop
    loop {
        {
            let mut state = tui_state.lock().unwrap();
            state.spinner_frame = state.spinner_frame.wrapping_add(1);
            terminal.draw(|f| app::draw(f, &state))?;
        }

        if crossterm_event::poll(std::time::Duration::from_millis(16))? {
            let event = crossterm_event::read()?;
            match event {
                Event::Paste(text) => {
                    let mut state = tui_state.lock().unwrap();
                    state.composer.handle_paste(&text);
                }
                Event::Key(key) => {
                    let mut state = tui_state.lock().unwrap();
                    app::handle_key(&mut state, key);

                    if state.should_quit {
                        cancel_token.lock().unwrap().cancel();
                        break;
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

                        // Set task label from first input
                        if !state.has_received_input {
                            state.has_received_input = true;
                            state.task_label = if input.len() > 80 {
                                format!("{}…", &input.chars().take(78).collect::<String>())
                            } else {
                                input.clone()
                            };
                            state.content_lines.clear();
                        }

                        let display = if input.len() > 120 {
                            format!("{}… [{} chars]", &input.chars().take(100).collect::<String>(), input.chars().count())
                        } else {
                            input.clone()
                        };
                        state.push_activity("›", Color::Green, display);

                        drop(state); // Release lock before async

                        let classified = pipit_io::input::classify_input(&input);
                        match classified {
                            pipit_io::input::UserInput::Command(cmd) => {
                                match cmd {
                                    pipit_io::input::SlashCommand::Quit => break,
                                    pipit_io::input::SlashCommand::Help => {
                                        let mut s = tui_state.lock().unwrap();
                                        s.push_activity("?", Color::Cyan, "/help".to_string());
                                        s.content_lines.clear();
                                        s.content_scroll_offset = 0;
                                        let help = vec![
                                            "Commands",
                                            "",
                                            "  /help              Show this help",
                                            "  /status            Show repo, model, tokens, cost",
                                            "  /cost              Show token cost summary",
                                            "  /clear             Reset context and chat history",
                                            "  /quit  /q          Exit pipit",
                                            "",
                                            "  /plans             Show proof-packet plan history",
                                            "  /context  /ctx     Show files in working set",
                                            "  /tokens  /tok      Token usage breakdown",
                                            "  /compact           Compress context to free tokens",
                                            "",
                                            "  /add <file>        Add file to working set",
                                            "  /drop <file>       Remove file from working set",
                                            "  /plan [goal]       Enter plan-first mode",
                                            "  /verify [scope]    Run build/lint/test checks",
                                            "  /aside <question>  Quick side question",
                                            "",
                                            "Grammar",
                                            "",
                                            "  /command           Slash commands (see above)",
                                            "  @file.rs           Attach file as context",
                                            "  !ls -la            Run shell command directly",
                                            "  Ctrl-J             Insert newline (multiline)",
                                            "  Tab                Tab-complete /commands, @files",
                                            "  ↑ ↓                History recall (empty input)",
                                            "  Alt-↑/↓            Scroll content pane",
                                            "",
                                            "Examples",
                                            "",
                                            "  explain this codebase",
                                            "  @src/main.rs fix the panic on line 42",
                                            "  !cargo test -- --nocapture",
                                            "  /add src/lib.rs",
                                            "  /verify cargo test",
                                        ];
                                        for line in help {
                                            s.content_lines.push(line.to_string());
                                        }
                                        s.has_received_input = true;
                                    }
                                    pipit_io::input::SlashCommand::Clear => {
                                        let _ = prompt_tx.send("/clear".to_string()).await;
                                        let mut s = tui_state.lock().unwrap();
                                        s.activity_lines.clear();
                                        s.content_lines.clear();
                                        s.scroll_offset = 0;
                                        s.content_scroll_offset = 0;
                                        s.push_activity("·", Color::DarkGray, "Context cleared".to_string());
                                    }
                                    pipit_io::input::SlashCommand::Cost => {
                                        let s = tui_state.lock().unwrap();
                                        let cost_msg = format!("${:.4} · {}% tokens", s.status.cost, s.status.token_pct());
                                        drop(s);
                                        let mut s = tui_state.lock().unwrap();
                                        s.push_activity("$", Color::Green, cost_msg);
                                    }
                                    pipit_io::input::SlashCommand::Status => {
                                        let s = tui_state.lock().unwrap();
                                        let info = format!(
                                            "{} · {} · {} · {}% tokens · ${:.4}",
                                            s.status.repo_name, s.status.branch, s.status.model,
                                            s.status.token_pct(), s.status.cost
                                        );
                                        drop(s);
                                        let mut s = tui_state.lock().unwrap();
                                        s.push_activity("·", Color::Cyan, info);
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
                                }
                                let enriched = format!("Run this shell command and show the output: `{}`", cmd);
                                let _ = prompt_tx.send(enriched).await;
                            }
                            pipit_io::input::UserInput::PromptWithFiles { prompt, files } => {
                                let enriched = format!("First read these files: {}. Then: {}", files.join(", "), prompt);
                                let _ = prompt_tx.send(enriched).await;
                            }
                            pipit_io::input::UserInput::PromptWithImages { prompt, image_paths } => {
                                let enriched = format!("Analyze these image files: {}. {}", image_paths.join(", "), prompt);
                                let _ = prompt_tx.send(enriched).await;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Cleanup
    dbg_log("[tui] exiting main loop, restoring terminal");
    drop(prompt_tx);
    let _ = agent_handle.await;
    let _ = extensions.on_session_end().await;
    app::restore_terminal(&mut terminal)?;
    Ok(())
}

/// Pure function: map an AgentEvent to TuiState mutations.
/// Extracted from the inline closure for testability.
fn apply_agent_event(state: &mut TuiState, event: &pipit_core::AgentEvent) {
    use pipit_core::AgentEvent;
    match event {
        AgentEvent::TurnStart { turn_number } => {
            state.finish_working();
            state.begin_working(&format!("Turn {}", turn_number));
        }
        AgentEvent::ContentDelta { text } => {
            let cleaned = text.replace("</think>", "").replace("<think>", "");
            if !cleaned.trim().is_empty() || !text.contains("think>") {
                state.push_content(&cleaned);
            }
        }
        AgentEvent::ContentComplete { .. } => {
            state.finish_working();
        }
        AgentEvent::ToolCallStart { name, args, .. } => {
            state.finish_working();
            let summary = match name.as_str() {
                "read_file" => format!("Read {}", args["path"].as_str().unwrap_or("?")),
                "edit_file" => format!("Edit {}", args["path"].as_str().unwrap_or("?")),
                "write_file" => format!("Write {}", args["path"].as_str().unwrap_or("?")),
                "bash" => format!("$ {}", args["command"].as_str().unwrap_or("?").chars().take(60).collect::<String>()),
                "grep" => format!("Grep '{}'", args["pattern"].as_str().unwrap_or("?")),
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
            state.push_activity(icon, color, summary);
            state.begin_working(&format!("Running {}…", name));
        }
        AgentEvent::ToolCallEnd { name, result, .. } => {
            state.finish_working();
            match result {
                pipit_core::ToolCallOutcome::Success { mutated: true, .. } => {
                    state.push_activity("✓", Color::Green, format!("{} done", name));
                }
                pipit_core::ToolCallOutcome::Error { message } => {
                    let msg = if message.len() > 80 {
                        format!("{}…", &message.chars().take(80).collect::<String>())
                    } else {
                        message.clone()
                    };
                    state.push_activity("✗", Color::Red, format!("{}: {}", name, msg));
                }
                _ => {}
            }
        }
        AgentEvent::TokenUsageUpdate { used, limit, cost } => {
            state.status.tokens_used = *used;
            state.status.tokens_limit = *limit;
            state.status.cost = *cost;
        }
        AgentEvent::PlanSelected { strategy, rationale, .. } => {
            state.push_activity("◆", Color::Blue, format!("{} — {}", strategy, rationale));
        }
        AgentEvent::LoopDetected { tool_name, count } => {
            state.push_activity("⚠", Color::Yellow, format!("{} repeated {}×", tool_name, count));
        }
        AgentEvent::PhaseTransition { to, mode, .. } => {
            state.push_activity("◇", Color::Magenta, format!("{} · {}", mode, to));
            state.begin_working(&format!("{}…", to));
        }
        AgentEvent::VerifierVerdict { verdict, confidence, .. } => {
            let color = match verdict.as_str() {
                "PASS" => Color::Green,
                "REPAIRABLE" => Color::Yellow,
                _ => Color::Red,
            };
            state.push_activity("◈", color, format!("verify: {} ({:.0}%)", verdict, confidence));
        }
        AgentEvent::RepairStarted { attempt, reason } => {
            state.push_activity("↻", Color::Yellow, format!("repair #{}: {}", attempt, reason));
            state.begin_working("Repairing…");
        }
        AgentEvent::TurnEnd { turn_number, .. } => {
            state.finish_working();
            state.push_activity("·", Color::DarkGray, format!("turn {} complete", turn_number));
        }
        _ => {}
    }
}
