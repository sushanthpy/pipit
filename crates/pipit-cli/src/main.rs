mod workflow;

use anyhow::{Context, Result};
use clap::Parser;
use pipit_config::{ApprovalMode, CliOverrides, ProviderKind};
use pipit_context::{ContextManager, budget::ContextSettings};
use pipit_core::{AgentLoop, AgentLoopConfig, AgentOutcome, PlanningState, ProofPacket};
use pipit_extensions::HookExtensionRunner;
use pipit_intelligence::RepoMap;
use pipit_io::input::{classify_input, read_input, SlashCommand, UserInput};
use pipit_io::{PipitUi, InteractiveApprovalHandler, StatusBarState};
use pipit_provider::LlmProvider;
use pipit_skills::SkillRegistry;
use pipit_tools::ToolRegistry;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use workflow::WorkflowAssets;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PlanningSnapshot {
    planning_state: PlanningState,
    proof_summary: Option<PlanningProofSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PlanningProofSummary {
    objective: String,
    confidence: f32,
    risk_score: f32,
    proof_file: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum PlanningStateSource {
    Live,
    Disk,
}

struct LoadedPlanningState {
    state: PlanningState,
    source: PlanningStateSource,
    proof_summary: Option<PlanningProofSummary>,
}

#[derive(Parser, Debug)]
#[command(name = "pipit", version = "0.1.0", about = "AI coding agent")]
struct Cli {
    /// Initial prompt (if provided, runs non-interactively)
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,

    /// LLM provider
    #[arg(short, long)]
    provider: Option<String>,

    /// Model name
    #[arg(short, long)]
    model: Option<String>,

    /// API key (defaults to env var)
    #[arg(long)]
    api_key: Option<String>,

    /// Approval mode: suggest, auto_edit, full_auto
    #[arg(short, long)]
    approval: Option<String>,

    /// Project root (defaults to auto-detect)
    #[arg(long)]
    root: Option<PathBuf>,

    /// Show thinking/reasoning output
    #[arg(long, default_value_t = true)]
    thinking: bool,

    /// Show detailed tool/compression trace lines in the interactive UI
    #[arg(long, default_value_t = false)]
    trace_ui: bool,

    /// Maximum number of turns
    #[arg(long)]
    max_turns: Option<u32>,

    /// Enable RepoMap
    #[arg(long, default_value_t = true)]
    repomap: bool,

    /// Base URL for the LLM endpoint (for local/custom models)
    #[arg(long)]
    base_url: Option<String>,

    /// Use full-screen TUI mode (alternate screen with fixed layout)
    #[arg(long, default_value_t = false)]
    tui: bool,

    #[command(subcommand)]
    command: Option<AuthCommands>,
}

#[derive(clap::Subcommand, Debug)]
enum AuthCommands {
    /// Manage provider authentication
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
}

#[derive(clap::Subcommand, Debug)]
enum AuthAction {
    /// Log in to a provider (stores credential in ~/.pipit/credentials.json)
    Login {
        /// Provider name (e.g. openai, anthropic, google, deepseek)
        provider: String,
        /// API key (if not provided, will prompt or use OAuth device flow)
        #[arg(long)]
        api_key: Option<String>,
        /// Use OAuth device-code flow (if supported by provider)
        #[arg(long)]
        device: bool,
        /// Set up Google ADC marker (for google provider)
        #[arg(long)]
        adc: bool,
    },
    /// Remove stored credentials for a provider
    Logout {
        /// Provider name
        provider: String,
    },
    /// Show authentication status for all providers
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("pipit=info".parse().unwrap()),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    // Handle auth subcommands early (before provider resolution)
    if let Some(AuthCommands::Auth { action }) = &cli.command {
        return handle_auth_command(action).await;
    }

    let cli_provider = cli
        .provider
        .as_deref()
        .map(str::parse)
        .transpose()
.map_err(|e: String| anyhow::anyhow!("Invalid provider: {}", e))?;

    // Resolve config
    let project_root = cli
        .root
        .or_else(pipit_config::detect_project_root)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let overrides = CliOverrides {
        provider: cli_provider,
        model: cli.model.clone(),
        approval_mode: cli
            .approval
            .as_deref()
            .map(str::parse)
            .transpose()
            .map_err(|e: String| anyhow::anyhow!(e))?,
        api_key: cli.api_key.clone(),
    };

    let config =
        pipit_config::resolve_config(Some(&project_root), overrides).context("Config resolution failed")?;

    let provider_kind = config.provider.default;

    // Resolve API key
    let api_key = cli
        .api_key
        .or_else(|| pipit_config::resolve_api_key(provider_kind))
        .ok_or_else(|| {
            let env_var = match provider_kind {
                ProviderKind::Anthropic | ProviderKind::AnthropicCompatible => "ANTHROPIC_API_KEY",
                ProviderKind::OpenAi | ProviderKind::OpenAiCompatible => "OPENAI_API_KEY",
                ProviderKind::DeepSeek => "DEEPSEEK_API_KEY",
                ProviderKind::Google => "GOOGLE_API_KEY",
                ProviderKind::OpenRouter => "OPENROUTER_API_KEY",
                ProviderKind::XAi => "XAI_API_KEY",
                ProviderKind::Cerebras => "CEREBRAS_API_KEY",
                ProviderKind::Groq => "GROQ_API_KEY",
                ProviderKind::Mistral => "MISTRAL_API_KEY",
                ProviderKind::Ollama => "OLLAMA_API_KEY (not usually needed)",
            };
            anyhow::anyhow!("No API key found. Set {} or pass --api-key", env_var)
        })?;

    // Resolve model
    let model = cli.model.unwrap_or(config.model.default_model.clone());

    // Create provider
    let provider: Arc<dyn LlmProvider> = Arc::from(
        pipit_provider::create_provider(provider_kind, &model, &api_key, cli.base_url.as_deref())
            .map_err(|e| anyhow::anyhow!("Provider creation failed for '{}': {}", model, e))?,

    );

    // Build tool registry
    let tools = ToolRegistry::with_builtins();

    let workflow_assets = WorkflowAssets::discover(&project_root);

    // Discover skills (#21: progressive disclosure)
    let skill_paths: Vec<PathBuf> = workflow_assets.skill_search_paths();
    let mut skills = SkillRegistry::discover(&skill_paths);
    if skills.count() > 0 {
        tracing::info!("Skills: {} discovered", skills.count());
    }

    // Build system prompt (with skill index injected as Tier 1)
    let system_prompt = build_system_prompt(
        &project_root,
        &tools,
        config.approval,
        provider_kind,
        &skills,
        &workflow_assets,
    );

    // Build context manager
    let mut context = ContextManager::with_settings(
        system_prompt.clone(),
        config.model.context_window,
        ContextSettings {
            output_reserve: config.context.output_reserve,
            tool_result_reserve: config.context.tool_result_reserve,
            compression_threshold: config.context.compression_threshold,
            preserve_recent_messages: config.context.preserve_recent_messages,
        },
    );

    // Build RepoMap
    let repo_map_text = if cli.repomap {
        let intelligence_config = pipit_intelligence::IntelligenceConfig::default();
        let repo_map = RepoMap::build(&project_root, intelligence_config);
        if repo_map.file_count() > 0 {
            let map = repo_map.render(&[], 4096);
            tracing::info!("RepoMap: {} files indexed", repo_map.file_count());
            context.update_repo_map_tokens((map.len() as u64) / 4);
            Some(map)
        } else {
            None
        }
    } else {
        None
    };

    // Build extensions
    let extensions: Arc<dyn pipit_extensions::ExtensionRunner> = Arc::new(
        HookExtensionRunner::from_hook_files(project_root.clone(), &workflow_assets.hook_files),
    );
    let extensions_for_lifecycle = extensions.clone();

    // Build agent
    let agent_config = AgentLoopConfig {
        max_turns: cli.max_turns.unwrap_or(config.context.max_turns),
        max_reflections: config.context.max_reflections,
        tool_timeout_secs: config.tools.shell_timeout_secs,
        approval_mode: config.approval,
        pricing: config.pricing.clone(),
        test_command: config.project.test_command.clone(),
        lint_command: config.project.lint_command.clone(),
        ..Default::default()
    };

    // Build approval handler
    let approval_handler: Arc<dyn pipit_core::ApprovalHandler> =
        Arc::new(InteractiveApprovalHandler);

    let (mut agent, mut event_rx, _steering_tx) = AgentLoop::new(
        provider.clone(),
        tools,
        context,
        extensions,
        approval_handler,
        agent_config,
        project_root.clone(),
    );

    if let Some(map) = &repo_map_text {
        agent.set_repo_map(map.clone());
    }

    let show_thinking = cli.thinking;
    let trace_ui = cli.trace_ui;

    // Derive project name for status bar
    let project_name = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string();

    let approval_mode = config.approval;

    // Create status bar state
    let status = StatusBarState::new(project_name.clone(), model.clone(), approval_mode);

    // Create UI
    let mut ui = PipitUi::new(show_thinking, true, trace_ui, status.clone());

    // Single-shot mode
    if let Some(prompt) = cli.prompt {
        let cancel = CancellationToken::new();

        // Spawn event handler
        let _ui_handle = tokio::spawn(async move {
            let mut ui = PipitUi::new(true, true, trace_ui, status);
            while let Ok(event) = event_rx.recv().await {
                ui.handle_event(&event);
            }
        });

        let outcome = agent.run(prompt, cancel).await;

        match outcome {
            AgentOutcome::Completed { turns, cost, proof, .. } => {
                let proof_path = persist_proof_packet(&project_root, &proof).ok();
                if let Some(planning_state) = agent.planning_state() {
                    persist_planning_snapshot(
                        &project_root,
                        &planning_state,
                        planning_proof_summary(&proof, proof_path.as_ref()),
                    )
                    .ok();
                }
                print_proof_summary(&proof);
                eprintln!("\n\x1b[2m({} turns, ${:.4})\x1b[0m", turns, cost);
            }
            AgentOutcome::Error(e) => {
                if let Some(planning_state) = agent.planning_state() {
                    persist_planning_snapshot(&project_root, &planning_state, None).ok();
                }
                eprintln!("\n\x1b[31mError: {}\x1b[0m", e);
                std::process::exit(1);
            }
            _ => {
                if let Some(planning_state) = agent.planning_state() {
                    persist_planning_snapshot(&project_root, &planning_state, None).ok();
                }
            }
        }

        return Ok(());
    }

    // ── TUI mode vs classic REPL ─────────────────────────────────────────
    if cli.tui {
        return run_tui_mode(
            agent,
            &mut event_rx,
            &project_root,
            &mut skills,
            &workflow_assets,
            &extensions_for_lifecycle,
            status,
            trace_ui,
        )
        .await;
    }

    // Interactive REPL mode (classic)
    ui.print_header();

    // Fire SessionStart hook
    let _ = extensions_for_lifecycle.on_session_start().await;

    // Spawn event handler in background
    let _event_handle = tokio::spawn(async move {
        let mut ui = PipitUi::new(true, true, trace_ui, status);
        while let Ok(event) = event_rx.recv().await {
            ui.handle_event(&event);
        }
    });

    // Working set: tracks files explicitly added to context
    let mut files_in_context: Vec<String> = Vec::new();

    loop {
        ui.print_prompt();

        let input = match read_input() {
            Some(input) => input,
            None => break, // EOF
        };

        if input.is_empty() {
            continue;
        }

        // Classify input using the new grammar
        match classify_input(&input) {
            UserInput::Command(cmd) => {
                match cmd {
                    SlashCommand::Help => {
                        PipitUi::print_help();
                        continue;
                    }
                    SlashCommand::Status => {
                        ui.render_status_bar();
                        eprintln!();
                        for line in workflow_assets.status_lines(skills.count()) {
                            eprintln!("\x1b[2m{}\x1b[0m", line);
                        }
                        if skills.count() == 0 && workflow_assets.status_lines(0).is_empty() {
                            eprintln!("\x1b[2mNo workflow assets discovered\x1b[0m");
                        }
                        continue;
                    }
                    SlashCommand::Plans => {
                        let state = agent
                            .planning_state()
                            .map(|state| LoadedPlanningState {
                                state,
                                source: PlanningStateSource::Live,
                                proof_summary: None,
                            })
                            .or_else(|| load_planning_snapshot(&project_root).ok().flatten());
                        print_plans(state);
                        continue;
                    }
                    SlashCommand::Quit => break,
                    SlashCommand::Clear => {
                        agent.clear_context();
                        eprintln!("\x1b[2mContext cleared\x1b[0m");
                        continue;
                    }
                    SlashCommand::Compact | SlashCommand::Summarize => {
                        let cancel = CancellationToken::new();
                        match agent.compact_context(cancel).await {
                            Ok(stats) => {
                                eprintln!(
                                    "\x1b[2mContext compacted: removed {} messages, freed {} tokens\x1b[0m",
                                    stats.messages_removed,
                                    stats.tokens_freed,
                                );
                            }
                            Err(err) => {
                                eprintln!("\x1b[31mCompaction failed: {}\x1b[0m", err);
                            }
                        }
                        continue;
                    }
                    SlashCommand::Cost | SlashCommand::Tokens => {
                        let usage = agent.context_usage();
                        let pct = if usage.limit > 0 {
                            (usage.total * 100) / usage.limit
                        } else {
                            0
                        };
                        eprintln!(
                            "\x1b[2mTokens: {} / {} ({}%) | Cost: ${:.4}\x1b[0m",
                            usage.total, usage.limit, pct, usage.cost
                        );
                        continue;
                    }
                    SlashCommand::Context => {
                        // Show working set summary
                        let usage = agent.context_usage();
                        ui.print_context_summary(&files_in_context, usage.total, usage.limit);
                        continue;
                    }
                    SlashCommand::Permissions(mode_arg) => {
                        if let Some(mode_str) = mode_arg {
                            match mode_str.parse::<ApprovalMode>() {
                                Ok(new_mode) => {
                                    // Wire into actual agent runtime state
                                    agent.set_approval_mode(new_mode);
                                    ui.status_mut().approval_mode = new_mode;
                                    eprintln!(
                                        "\x1b[32mSwitched to {} mode\x1b[0m",
                                        new_mode.label()
                                    );
                                }
                                Err(e) => {
                                    eprintln!("\x1b[31m{}\x1b[0m", e);
                                }
                            }
                        } else {
                            ui.print_permissions();
                        }
                        continue;
                    }
                    SlashCommand::Plan(topic) => {
                        let prompt = if let Some(t) = topic {
                            format!("Create a plan for: {}. Do NOT make any changes yet — only discuss the approach, list the files involved, and outline the steps.", t)
                        } else {
                            "Summarize the current plan and what the next steps are. Do NOT make any changes.".to_string()
                        };
                        let cancel = CancellationToken::new();
                        let cancel_clone = cancel.clone();
                        let ctrlc_handle = tokio::spawn(async move {
                            tokio::signal::ctrl_c().await.ok();
                            cancel_clone.cancel();
                        });
                        let _ = agent.run(prompt, cancel).await;
                        ctrlc_handle.abort();
                        continue;
                    }
                    SlashCommand::Add(path) => {
                        if path.is_empty() {
                            eprintln!("\x1b[33mUsage: /add <file_path>\x1b[0m");
                        } else {
                            if !files_in_context.contains(&path) {
                                files_in_context.push(path.clone());
                            }
                            // Read the file through the agent so it enters the context window
                            let prompt = format!("Read the file {} and keep it in context for our discussion.", path);
                            let cancel = CancellationToken::new();
                            let _ = agent.run(prompt, cancel).await;
                        }
                        continue;
                    }
                    SlashCommand::Drop(path) => {
                        if path.is_empty() {
                            eprintln!("\x1b[33mUsage: /drop <file_path>\x1b[0m");
                        } else {
                            files_in_context.retain(|f| f != &path);
                            eprintln!("\x1b[2mDropped {} from working set\x1b[0m", path);
                        }
                        continue;
                    }
                    SlashCommand::Undo | SlashCommand::Rewind => {
                        eprintln!("\x1b[33m/rewind: stepping back is not yet available\x1b[0m");
                        continue;
                    }
                    SlashCommand::Verify(scope) => {
                        let scope_label = scope.as_deref().unwrap_or("full");
                        let prompt = match scope_label {
                            "quick" => "Run a quick verification: build and type-check only. Report pass/fail for each.".to_string(),
                            "full" => "Run full verification: build, lint, type-check, and tests. Report pass/fail for each step. If any step fails, analyze the error and suggest a fix.".to_string(),
                            "pre-commit" => "Run pre-commit checks: lint, type-check, and look for any debug statements or console.log calls in modified files. Report results.".to_string(),
                            custom => format!("Run this verification command: {}. Report the results.", custom),
                        };
                        let cancel = CancellationToken::new();
                        let cancel_clone = cancel.clone();
                        let ctrlc_handle = tokio::spawn(async move {
                            tokio::signal::ctrl_c().await.ok();
                            cancel_clone.cancel();
                        });
                        let outcome = agent.run(prompt, cancel).await;
                        ctrlc_handle.abort();
                        handle_agent_outcome(&project_root, &mut agent, outcome);
                        continue;
                    }
                    SlashCommand::Aside(question) => {
                        if question.is_empty() {
                            eprintln!("\x1b[33mUsage: /aside <question>\x1b[0m");
                        } else {
                            let prompt = format!(
                                "ASIDE: Answer this quick question without losing our current task context. \
                                 After answering, remind me what we were working on.\n\nQuestion: {}",
                                question
                            );
                            let cancel = CancellationToken::new();
                            let cancel_clone = cancel.clone();
                            let ctrlc_handle = tokio::spawn(async move {
                                tokio::signal::ctrl_c().await.ok();
                                cancel_clone.cancel();
                            });
                            let _ = agent.run(prompt, cancel).await;
                            ctrlc_handle.abort();
                        }
                        continue;
                    }
                    SlashCommand::Checkpoint(action) => {
                        let action = action.as_deref().unwrap_or("create");
                        let prompt = match action {
                            "create" | "save" => {
                                "Create a checkpoint of the current state: \
                                 1. Run `git add -A && git stash push -m 'pipit-checkpoint'` to save current changes. \
                                 2. Report what files were stashed. \
                                 3. Confirm the checkpoint was created.".to_string()
                            }
                            "restore" | "load" => {
                                "Restore the most recent checkpoint: \
                                 1. Run `git stash list` to find the latest pipit-checkpoint. \
                                 2. Apply it with `git stash pop`. \
                                 3. Report what was restored.".to_string()
                            }
                            "list" => {
                                "List all checkpoints: run `git stash list` and show any entries with 'pipit-checkpoint' in the message.".to_string()
                            }
                            _ => format!("Checkpoint action: {}", action),
                        };
                        let cancel = CancellationToken::new();
                        let cancel_clone = cancel.clone();
                        let ctrlc_handle = tokio::spawn(async move {
                            tokio::signal::ctrl_c().await.ok();
                            cancel_clone.cancel();
                        });
                        let outcome = agent.run(prompt, cancel).await;
                        ctrlc_handle.abort();
                        handle_agent_outcome(&project_root, &mut agent, outcome);
                        continue;
                    }
                    SlashCommand::Tdd(topic) => {
                        let prompt = if let Some(t) = topic {
                            format!(
                                "Enforce TDD workflow for: {}\n\
                                 1. Write a failing test FIRST that describes the desired behavior.\n\
                                 2. Run the test to confirm it FAILS (RED).\n\
                                 3. Write the MINIMAL code to make the test pass (GREEN).\n\
                                 4. Run the test again to confirm it PASSES.\n\
                                 5. Refactor if needed while keeping tests green.\n\
                                 Aim for 80%+ coverage.", t
                            )
                        } else {
                            "Show the current test coverage and suggest what tests are missing. Do NOT write code yet — just analyze.".to_string()
                        };
                        let cancel = CancellationToken::new();
                        let cancel_clone = cancel.clone();
                        let ctrlc_handle = tokio::spawn(async move { tokio::signal::ctrl_c().await.ok(); cancel_clone.cancel(); });
                        let outcome = agent.run(prompt, cancel).await;
                        ctrlc_handle.abort();
                        handle_agent_outcome(&project_root, &mut agent, outcome);
                        continue;
                    }
                    SlashCommand::CodeReview => {
                        let prompt = "Run a comprehensive code review of uncommitted changes:\n\
                            1. Run `git diff` and `git diff --staged` to see all changes.\n\
                            2. Review for: CRITICAL (security issues, data loss, crashes), HIGH (bugs, wrong logic), MEDIUM (style, patterns).\n\
                            3. For each finding: file, line, severity, description, suggested fix.\n\
                            4. Summary: total findings by severity, overall assessment, ready-to-merge verdict.".to_string();
                        let cancel = CancellationToken::new();
                        let cancel_clone = cancel.clone();
                        let ctrlc_handle = tokio::spawn(async move { tokio::signal::ctrl_c().await.ok(); cancel_clone.cancel(); });
                        let outcome = agent.run(prompt, cancel).await;
                        ctrlc_handle.abort();
                        handle_agent_outcome(&project_root, &mut agent, outcome);
                        continue;
                    }
                    SlashCommand::BuildFix => {
                        let prompt = "Fix build errors incrementally:\n\
                            1. Detect the build system (cargo, npm, tsc, make, gradle, go, etc.).\n\
                            2. Run the build command and capture errors.\n\
                            3. Fix ONE error at a time — the first/root error.\n\
                            4. Re-run the build to verify the fix.\n\
                            5. Repeat until the build succeeds or report what's unresolvable.\n\
                            Make minimal, surgical fixes. Do not refactor.".to_string();
                        let cancel = CancellationToken::new();
                        let cancel_clone = cancel.clone();
                        let ctrlc_handle = tokio::spawn(async move { tokio::signal::ctrl_c().await.ok(); cancel_clone.cancel(); });
                        let outcome = agent.run(prompt, cancel).await;
                        ctrlc_handle.abort();
                        handle_agent_outcome(&project_root, &mut agent, outcome);
                        continue;
                    }
                    SlashCommand::SaveSession(alias) => {
                        let session_dir = project_root.join(".pipit").join("sessions");
                        let _ = std::fs::create_dir_all(&session_dir);
                        let timestamp = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        let name = alias.unwrap_or_else(|| format!("{}", timestamp));
                        let session_subdir = session_dir.join(&name);

                        // Save conversation history (messages)
                        match agent.save_session(&session_subdir) {
                            Ok(_) => {
                                // Also save metadata
                                let meta = serde_json::json!({
                                    "name": name,
                                    "timestamp": timestamp,
                                    "model": model,
                                    "approval_mode": format!("{}", ui.status_mut().approval_mode),
                                    "files_in_context": files_in_context,
                                    "token_usage": {
                                        "total": agent.context_usage().total,
                                        "cost": agent.context_usage().cost,
                                    },
                                });
                                let meta_file = session_subdir.join("metadata.json");
                                let _ = std::fs::write(&meta_file, serde_json::to_string_pretty(&meta).unwrap_or_default());
                                let msg_count = agent.context_usage().total;
                                eprintln!("\x1b[32mSession '{}' saved ({} tokens)\x1b[0m", name, msg_count);
                            }
                            Err(e) => eprintln!("\x1b[31mFailed to save session: {}\x1b[0m", e),
                        }
                        continue;
                    }
                    SlashCommand::ResumeSession(name) => {
                        let session_dir = project_root.join(".pipit").join("sessions");
                        if let Some(name) = name {
                            let session_subdir = session_dir.join(&name);
                            // Restore conversation history
                            match agent.load_session(&session_subdir) {
                                Ok(msg_count) => {
                                    eprintln!("\x1b[32mRestored {} messages from session '{}'\x1b[0m", msg_count, name);
                                    // Also restore metadata (files_in_context)
                                    let meta_file = session_subdir.join("metadata.json");
                                    if let Ok(content) = std::fs::read_to_string(&meta_file) {
                                        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&content) {
                                            if let Some(files) = data.get("files_in_context").and_then(|v| v.as_array()) {
                                                for f in files {
                                                    if let Some(path) = f.as_str() {
                                                        if !files_in_context.contains(&path.to_string()) {
                                                            files_in_context.push(path.to_string());
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(e) => eprintln!("\x1b[31mFailed to resume session: {}\x1b[0m", e),
                            }
                        } else {
                            // List available sessions
                            if session_dir.exists() {
                                eprintln!("\x1b[1mSaved sessions:\x1b[0m");
                                if let Ok(entries) = std::fs::read_dir(&session_dir) {
                                    let mut sessions: Vec<_> = entries.flatten().collect();
                                    sessions.sort_by_key(|e| e.file_name());
                                    for entry in sessions {
                                        let path = entry.path();
                                        if path.is_dir() {
                                            let name = path.file_name().unwrap_or_default().to_string_lossy();
                                            let meta_file = path.join("metadata.json");
                                            let detail = if let Ok(c) = std::fs::read_to_string(&meta_file) {
                                                if let Ok(d) = serde_json::from_str::<serde_json::Value>(&c) {
                                                    let model = d.get("model").and_then(|m| m.as_str()).unwrap_or("?");
                                                    let cost = d.get("token_usage").and_then(|t| t.get("cost")).and_then(|c| c.as_f64()).unwrap_or(0.0);
                                                    format!(" ({}, ${:.4})", model, cost)
                                                } else { String::new() }
                                            } else { String::new() };
                                            eprintln!("  {}{}", name, detail);
                                        }
                                    }
                                }
                            } else {
                                eprintln!("\x1b[2mNo saved sessions\x1b[0m");
                            }
                        }
                        continue;
                    }
                    SlashCommand::Model(_) | SlashCommand::Branch(_) | SlashCommand::BranchList | SlashCommand::BranchSwitch(_) => {
                        eprintln!("\x1b[33mNot available in this build\x1b[0m");
                        continue;
                    }
                    SlashCommand::Unknown(cmd) => {
                        let args = input.strip_prefix(&format!("/{}", cmd))
                            .unwrap_or("").trim();

                        // 1. Try skill system first
                        if skills.has_skill(&cmd) {
                            match skills.load(&cmd) {
                                Ok(skill) => {
                                    let injection = skill.as_injection(args);
                                    let cancel = CancellationToken::new();
                                    let outcome = agent.run(injection, cancel).await;
                                    handle_agent_outcome(&project_root, &mut agent, outcome);
                                }
                                Err(e) => {
                                    eprintln!("\x1b[31mFailed to load skill: {}\x1b[0m", e);
                                }
                            }
                            continue;
                        }

                        // 2. Try custom commands from .pipit/commands/
                        let custom_commands = workflow_assets.discover_commands();
                        if let Some((_, _, cmd_path)) = custom_commands.iter().find(|(name, _, _)| name == &cmd) {
                            match std::fs::read_to_string(cmd_path) {
                                Ok(content) => {
                                    let body = workflow::strip_command_frontmatter(&content);
                                    let expanded = body
                                        .replace("$ARGUMENTS", args)
                                        .replace("${ARGUMENTS}", args);
                                    let injection = format!(
                                        "[Command: /{}]\n{}\n\nUser request: {}",
                                        cmd, expanded, args
                                    );
                                    let cancel = CancellationToken::new();
                                    let outcome = agent.run(injection, cancel).await;
                                    handle_agent_outcome(&project_root, &mut agent, outcome);
                                }
                                Err(e) => {
                                    eprintln!("\x1b[31mFailed to load command: {}\x1b[0m", e);
                                }
                            }
                            continue;
                        }

                        eprintln!("\x1b[33mUnknown command: /{}\x1b[0m", cmd);
                        continue;
                    }
                }
            }
            UserInput::ShellPassthrough(cmd) => {
                // Direct shell execution — run through the agent's bash tool
                let prompt = format!("Run this shell command and show me the output: {}", cmd);
                let cancel = CancellationToken::new();
                let cancel_clone = cancel.clone();
                let ctrlc_handle = tokio::spawn(async move {
                    tokio::signal::ctrl_c().await.ok();
                    cancel_clone.cancel();
                });
                let _ = agent.run(prompt, cancel).await;
                ctrlc_handle.abort();
                continue;
            }
            UserInput::PromptWithFiles { prompt, files } => {
                // Add @file mentions to context, then run the prompt
                let file_list = files.join(", ");
                let enriched = format!(
                    "First read these files: {}. Then: {}",
                    file_list, prompt
                );
                let cancel = CancellationToken::new();
                let cancel_clone = cancel.clone();
                let ctrlc_handle = tokio::spawn(async move {
                    tokio::signal::ctrl_c().await.ok();
                    cancel_clone.cancel();
                });
                let outcome = agent.run(enriched, cancel).await;
                ctrlc_handle.abort();
                handle_agent_outcome(&project_root, &mut agent, outcome);
                println!();
                continue;
            }
            UserInput::PromptWithImages { prompt, image_paths } => {
                // Read image files and send as vision prompt
                let mut image_descriptions = Vec::new();
                for img_path in &image_paths {
                    match pipit_io::input::read_image_file(img_path) {
                        Ok((media_type, data)) => {
                            let size_kb = data.len() / 1024;
                            image_descriptions.push(format!("{} ({}KB, {})", img_path, size_kb, media_type));
                            // Inject the image into context as a user message with image content block
                            agent.inject_image(&media_type, data);
                        }
                        Err(e) => {
                            eprintln!("\x1b[31m{}\x1b[0m", e);
                        }
                    }
                }
                let enriched = if prompt.is_empty() {
                    format!("I've attached {} image(s): {}. Please analyze what you see.", image_paths.len(), image_descriptions.join(", "))
                } else {
                    format!("I've attached image(s): {}. {}", image_descriptions.join(", "), prompt)
                };
                let cancel = CancellationToken::new();
                let cancel_clone = cancel.clone();
                let ctrlc_handle = tokio::spawn(async move {
                    tokio::signal::ctrl_c().await.ok();
                    cancel_clone.cancel();
                });
                let outcome = agent.run(enriched, cancel).await;
                ctrlc_handle.abort();
                handle_agent_outcome(&project_root, &mut agent, outcome);
                println!();
                continue;
            }
            UserInput::Prompt(prompt) => {
                // Regular prompt — run through agent
                let cancel = CancellationToken::new();
                let cancel_clone = cancel.clone();
                let ctrlc_handle = tokio::spawn(async move {
                    tokio::signal::ctrl_c().await.ok();
                    cancel_clone.cancel();
                });
                let outcome = agent.run(prompt, cancel).await;
                ctrlc_handle.abort();
                handle_agent_outcome(&project_root, &mut agent, outcome);
                println!();
            }
        }
    }

    // Fire SessionEnd hook
    let _ = extensions_for_lifecycle.on_session_end().await;

    Ok(())
}

/// Full-screen TUI mode using ratatui.
#[allow(clippy::too_many_arguments)]
async fn run_tui_mode(
    mut agent: AgentLoop,
    event_rx: &mut tokio::sync::broadcast::Receiver<pipit_core::AgentEvent>,
    project_root: &PathBuf,
    skills: &mut SkillRegistry,
    workflow_assets: &workflow::WorkflowAssets,
    extensions: &Arc<dyn pipit_extensions::ExtensionRunner>,
    status: StatusBarState,
    _trace_ui: bool,
) -> Result<()> {
    use pipit_io::app::{self, TuiState};
    use std::sync::{Arc, Mutex};
    use crossterm::event::{self as crossterm_event, Event, KeyEvent};

    let _ = extensions.on_session_start().await;

    let tui_state = Arc::new(Mutex::new(TuiState::new(status)));
    let mut terminal = app::init_terminal().context("Failed to init TUI")?;

    // Welcome message
    {
        let mut state = tui_state.lock().unwrap();
        state.push_activity("·", ratatui::style::Color::Cyan, "pipit v0.1.0 — /help for commands".to_string());
    }

    // Spawn agent event handler that updates TUI state
    let tui_state_for_events = tui_state.clone();
    let mut event_rx_owned = event_rx.resubscribe();
    let _event_handle = tokio::spawn(async move {
        use pipit_core::AgentEvent;
        while let Ok(event) = event_rx_owned.recv().await {
            let mut state = tui_state_for_events.lock().unwrap();
            match &event {
                AgentEvent::TurnStart { turn_number } => {
                    // Commit any previous streaming text
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
                    // Streaming done — commit to activity log
                    state.finish_working();
                }
                AgentEvent::ToolCallStart { name, args, .. } => {
                    // Commit streaming before showing tool
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
                        "edit_file" | "write_file" => ratatui::style::Color::Green,
                        "bash" => ratatui::style::Color::Cyan,
                        _ => ratatui::style::Color::DarkGray,
                    };
                    state.push_activity(icon, color, summary);
                    state.begin_working(&format!("Running {}…", name));
                }
                AgentEvent::ToolCallEnd { name, result, .. } => {
                    state.finish_working();
                    match result {
                        pipit_core::ToolCallOutcome::Success { mutated: true, .. } => {
                            state.push_activity("✓", ratatui::style::Color::Green, format!("{} done", name));
                        }
                        pipit_core::ToolCallOutcome::Error { message } => {
                            let msg = if message.len() > 80 { format!("{}…", &message.chars().take(80).collect::<String>()) } else { message.clone() };
                            state.push_activity("✗", ratatui::style::Color::Red, format!("{}: {}", name, msg));
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
                    state.push_activity("◆", ratatui::style::Color::Blue, format!("{} — {}", strategy, rationale));
                }
                AgentEvent::LoopDetected { tool_name, count } => {
                    state.push_activity("⚠", ratatui::style::Color::Yellow, format!("{} repeated {}×", tool_name, count));
                }
                AgentEvent::TurnEnd { turn_number, .. } => {
                    state.finish_working();
                    state.push_activity("·", ratatui::style::Color::DarkGray, format!("turn {} complete", turn_number));
                }
                _ => {}
            }
        }
    });

    // Channel for sending prompts to the agent task
    let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::channel::<String>(8);

    // Spawn agent runner as a separate task so the TUI keeps redrawing
    let tui_state_for_agent = tui_state.clone();
    let agent_handle = tokio::spawn(async move {
        while let Some(prompt) = prompt_rx.recv().await {
            {
                let mut s = tui_state_for_agent.lock().unwrap();
                s.begin_working("Thinking…");
            }
            let cancel = CancellationToken::new();
            let outcome = agent.run(prompt, cancel).await;
            {
                let mut s = tui_state_for_agent.lock().unwrap();
                s.finish_working();
                match &outcome {
                    AgentOutcome::Completed { turns, cost, .. } => {
                        s.push_activity("✓", ratatui::style::Color::Green, format!("Done — {} turns, ${:.4}", turns, cost));
                    }
                    AgentOutcome::Error(e) => {
                        s.push_activity("✗", ratatui::style::Color::Red, format!("Error: {}", e));
                    }
                    AgentOutcome::Cancelled => {
                        s.push_activity("·", ratatui::style::Color::DarkGray, "Cancelled".to_string());
                    }
                    AgentOutcome::MaxTurnsReached(n) => {
                        s.push_activity("⚠", ratatui::style::Color::Yellow, format!("Max turns ({})", n));
                    }
                }
            }
        }
    });

    // Main TUI event loop — never blocks on agent work
    loop {
        // Draw
        {
            let state = tui_state.lock().unwrap();
            terminal.draw(|f| app::draw(f, &state))?;
        }

        // Poll for crossterm events (16ms ≈ 60fps for responsive typing)
        if crossterm_event::poll(std::time::Duration::from_millis(16))? {
            let event = crossterm_event::read()?;
            match event {
                Event::Paste(text) => {
                    // Handle pasted text as a single block (newlines → spaces)
                    let mut state = tui_state.lock().unwrap();
                    state.handle_paste(&text);
                }
                Event::Key(key) => {
                let mut state = tui_state.lock().unwrap();
                app::handle_key(&mut state, key);

                if state.should_quit {
                    break;
                }

                // Check if input was submitted
                if let Some(input) = state.submitted_input.take() {
                    drop(state); // Release lock

                    let classified = pipit_io::input::classify_input(&input);
                    match classified {
                        pipit_io::input::UserInput::Command(cmd) => {
                            match cmd {
                                pipit_io::input::SlashCommand::Quit => break,
                                pipit_io::input::SlashCommand::Help => {
                                    let mut s = tui_state.lock().unwrap();
                                    s.push_activity("·", ratatui::style::Color::Cyan, "/help /status /plans /context /tokens /permissions /plan /add /drop".to_string());
                                    s.push_activity("·", ratatui::style::Color::Cyan, "/compact /verify /aside /checkpoint /save /resume /clear /cost /quit".to_string());
                                    s.push_activity("·", ratatui::style::Color::Cyan, "Grammar: /command  @file  !shell  ↑↓ scroll".to_string());
                                }
                                pipit_io::input::SlashCommand::Clear => {
                                    let _ = prompt_tx.send("/clear".to_string()).await;
                                    let mut s = tui_state.lock().unwrap();
                                    s.activity_lines.clear();
                                    s.scroll_offset = 0;
                                    s.push_activity("·", ratatui::style::Color::DarkGray, "Context cleared".to_string());
                                }
                                _ => {
                                    let mut s = tui_state.lock().unwrap();
                                    s.push_activity("·", ratatui::style::Color::Yellow, "Use classic mode for this command".to_string());
                                }
                            }
                        }
                        pipit_io::input::UserInput::Prompt(prompt)
                        | pipit_io::input::UserInput::ShellPassthrough(prompt) => {
                            let _ = prompt_tx.send(prompt).await;
                        }
                        pipit_io::input::UserInput::PromptWithFiles { prompt, files } => {
                            let enriched = format!("First read these files: {}. Then: {}", files.join(", "), prompt);
                            let _ = prompt_tx.send(enriched).await;
                        }
                        pipit_io::input::UserInput::PromptWithImages { prompt, image_paths } => {
                            // In TUI mode, send a description prompt (image injection needs agent access)
                            let enriched = format!("Analyze these image files: {}. {}", image_paths.join(", "), prompt);
                            let _ = prompt_tx.send(enriched).await;
                        }
                    }
                }
                } // end Event::Key
                _ => {} // ignore resize, focus, etc.
            } // end match event
        }
    }

    // Cleanup
    drop(prompt_tx); // Signal agent task to stop
    let _ = agent_handle.await;
    let _ = extensions.on_session_end().await;
    app::restore_terminal(&mut terminal)?;
    Ok(())
}

/// Handle the outcome of an agent run — persist proofs, print summaries, show errors.
fn handle_agent_outcome(
    project_root: &PathBuf,
    agent: &mut AgentLoop,
    outcome: AgentOutcome,
) {
    match outcome {
        AgentOutcome::Completed {
            turns, cost, proof, ..
        } => {
            let proof_path = persist_proof_packet(project_root, &proof).ok();
            if let Some(planning_state) = agent.planning_state() {
                persist_planning_snapshot(
                    project_root,
                    &planning_state,
                    planning_proof_summary(&proof, proof_path.as_ref()),
                )
                .ok();
            }
            print_proof_summary(&proof);
            eprintln!("\x1b[2m({} turns, ${:.4})\x1b[0m", turns, cost);
        }
        AgentOutcome::MaxTurnsReached(n) => {
            if let Some(planning_state) = agent.planning_state() {
                persist_planning_snapshot(project_root, &planning_state, None).ok();
            }
            eprintln!("\x1b[33mReached max turns ({})\x1b[0m", n);
        }
        AgentOutcome::Cancelled => {
            if let Some(planning_state) = agent.planning_state() {
                persist_planning_snapshot(project_root, &planning_state, None).ok();
            }
            eprintln!("\x1b[2m(cancelled)\x1b[0m");
        }
        AgentOutcome::Error(e) => {
            if let Some(planning_state) = agent.planning_state() {
                persist_planning_snapshot(project_root, &planning_state, None).ok();
            }
            eprintln!("\x1b[31mError: {}\x1b[0m", e);
        }
    }
}

// ─── Auth subcommand handling ───

async fn handle_auth_command(action: &AuthAction) -> Result<()> {
    use pipit_config::{
        CredentialStore, StoredCredential, OAuthFlow,
        oauth_device_flow, oauth_device_config_for,
    };

    match action {
        AuthAction::Login {
            provider,
            api_key,
            device,
            adc,
        } => {
            let provider_kind: ProviderKind = provider
                .parse()
                .map_err(|e: String| anyhow::anyhow!(e))?;
            let mut store = CredentialStore::load();

            if *adc {
                // Google ADC marker
                if provider_kind != ProviderKind::Google {
                    anyhow::bail!("--adc is only valid for the google provider");
                }
                // Verify gcloud works
                eprint!("Verifying Google ADC... ");
                match store.resolve_token(ProviderKind::Google) {
                    Some(_) => {
                        store.set(
                            &provider_kind.to_string(),
                            StoredCredential::GoogleAdc,
                        );
                        store.save().context("Failed to save credentials")?;
                        eprintln!("✓ Google ADC configured");
                        eprintln!(
                            "  Using: gcloud auth application-default print-access-token"
                        );
                    }
                    None => {
                        // Store the marker anyway — user might configure gcloud later
                        store.set(
                            &provider_kind.to_string(),
                            StoredCredential::GoogleAdc,
                        );
                        store.save().context("Failed to save credentials")?;
                        eprintln!("⚠ gcloud ADC not available yet");
                        eprintln!("  Run: gcloud auth application-default login");
                        eprintln!("  Marker saved — pipit will retry at runtime.");
                    }
                }
                return Ok(());
            }

            if *device {
                // OAuth device-code flow
                if let Some(config) = oauth_device_config_for(provider_kind) {
                    eprintln!("Starting OAuth device-code flow for {}...", provider);
                    let token = oauth_device_flow(&config)
                        .await
                        .map_err(|e| anyhow::anyhow!(e))?;

                    let expires_at = token.expires_in.map(|secs| {
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs() + secs)
                            .unwrap_or(0)
                    });

                    store.set(
                        &provider_kind.to_string(),
                        StoredCredential::OAuthToken {
                            access_token: token.access_token,
                            refresh_token: token.refresh_token,
                            expires_at,
                            flow: OAuthFlow::DeviceCode,
                        },
                    );
                    store.save().context("Failed to save credentials")?;
                    eprintln!("Credentials saved to ~/.pipit/credentials.json");
                } else {
                    anyhow::bail!(
                        "OAuth device flow not configured for {}. Use --api-key instead.",
                        provider
                    );
                }
                return Ok(());
            }

            // API key flow
            let key = if let Some(k) = api_key {
                k.clone()
            } else {
                // Prompt interactively
                eprint!("Enter API key for {}: ", provider);
                let mut input = String::new();
                std::io::stdin()
                    .read_line(&mut input)
                    .context("Failed to read input")?;
                let trimmed = input.trim().to_string();
                if trimmed.is_empty() {
                    anyhow::bail!("No API key provided");
                }
                trimmed
            };

            store.set(
                &provider_kind.to_string(),
                StoredCredential::ApiKey { api_key: key },
            );
            store.save().context("Failed to save credentials")?;
            eprintln!("✓ API key stored for {}", provider);
            if let Some(path) = CredentialStore::path() {
                eprintln!("  Saved to: {}", path.display());
            }
        }

        AuthAction::Logout { provider } => {
            let provider_kind: ProviderKind = provider
                .parse()
                .map_err(|e: String| anyhow::anyhow!(e))?;
            let mut store = CredentialStore::load();
            if store.remove(&provider_kind.to_string()) {
                store.save().context("Failed to save credentials")?;
                eprintln!("✓ Credentials removed for {}", provider);
            } else {
                eprintln!("No credentials found for {}", provider);
            }
        }

        AuthAction::Status => {
            let store = CredentialStore::load();
            let entries = store.list();

            if entries.is_empty() {
                eprintln!("No stored credentials.");
                eprintln!();
                eprintln!("Use `pipit auth login <provider>` to add credentials.");
                eprintln!("Or set environment variables (e.g. OPENAI_API_KEY).");
            } else {
                eprintln!("Stored credentials:");
                eprintln!();
                for (provider, kind) in &entries {
                    let status = match kind {
                        &"api_key" => "API key".to_string(),
                        &"oauth_device" => "OAuth (device flow)".to_string(),
                        &"oauth_code" => "OAuth (auth code)".to_string(),
                        &"google_adc" => {
                            // Check if ADC actually works
                            let provider_kind: Result<ProviderKind, _> = provider.parse();
                            if let Ok(pk) = provider_kind {
                                if store.resolve_token(pk).is_some() {
                                    "Google ADC ✓".to_string()
                                } else {
                                    "Google ADC ✗ (run: gcloud auth application-default login)".to_string()
                                }
                            } else {
                                "Google ADC".to_string()
                            }
                        }
                        other => other.to_string(),
                    };
                    eprintln!("  {:20} {}", provider, status);
                }
            }

            // Also check env vars
            eprintln!();
            eprintln!("Environment variables:");
            let env_checks = [
                ("ANTHROPIC_API_KEY", "anthropic"),
                ("OPENAI_API_KEY", "openai"),
                ("DEEPSEEK_API_KEY", "deepseek"),
                ("GOOGLE_API_KEY", "google"),
                ("OPENROUTER_API_KEY", "openrouter"),
                ("XAI_API_KEY", "xai"),
                ("CEREBRAS_API_KEY", "cerebras"),
                ("GROQ_API_KEY", "groq"),
                ("MISTRAL_API_KEY", "mistral"),
            ];
            let mut found_env = false;
            for (var, label) in &env_checks {
                if std::env::var(var).is_ok() {
                    eprintln!("  {:20} {} ✓", label, var);
                    found_env = true;
                }
            }
            if !found_env {
                eprintln!("  (none set)");
            }
        }
    }

    Ok(())
}

// Fix #20: Composable system prompt builder
fn build_system_prompt(
    project_root: &PathBuf,
    tools: &ToolRegistry,
    approval_mode: pipit_config::ApprovalMode,
    _provider: ProviderKind,
    skills: &SkillRegistry,
    workflow_assets: &WorkflowAssets,
) -> String {
    let project_name = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project");

    let mut prompt = format!(
        r#"You are Pipit, an expert AI coding agent working in the terminal.

## Core capabilities
- Read, write, and edit code files with surgical precision
- Execute shell commands
- Search codebases with grep and glob
- Navigate and understand project structure

## Rules
1. Always read a file before editing it to understand the full context.
2. Make minimal, focused changes — don't refactor code you weren't asked to change.
3. Use the edit_file tool for surgical edits, not write_file (which rewrites the whole file).
4. When executing shell commands, explain what they do.
5. If you encounter an error, analyze it and try a different approach.
6. Never guess at file contents — always read first.
7. Prefer using existing patterns and conventions found in the codebase.

## Project
Working directory: {root}
Project: {name}
"#,
        root = project_root.display(),
        name = project_name,
    );

    // Add tool-specific instructions with approval annotations
    prompt.push_str("\n## Available Tools\n");
    for (decl, needs_approval) in tools.declarations_annotated(approval_mode) {
        if needs_approval {
            prompt.push_str(&format!("- **{}** *(requires approval)*: {}\n", decl.name, decl.description));
        } else {
            prompt.push_str(&format!("- **{}**: {}\n", decl.name, decl.description));
        }
    }

    // Add edit format instructions
    prompt.push_str("\n## Edit format\n");
    prompt.push_str("Use the edit_file tool for surgical code edits. Provide the exact search text and replacement.\n");
    prompt.push_str("The search text must match the file exactly (fuzzy whitespace matching is used as fallback).\n");

    // Load project conventions if present
    let conventions_path = project_root.join(".pipit").join("CONVENTIONS.md");
    if conventions_path.exists() {
        if let Ok(conventions) = std::fs::read_to_string(&conventions_path) {
            prompt.push_str("\n## Project Conventions\n");
            prompt.push_str(&conventions);
            prompt.push_str("\n");
        }
    }

    // #21: Inject skill index (Tier 1 — names + descriptions only)
    prompt.push_str(&skills.prompt_section());
    prompt.push_str(&workflow_assets.prompt_section());

    prompt
}

fn print_proof_summary(proof: &ProofPacket) {
    eprintln!("\n\x1b[2mProof packet\x1b[0m");
    eprintln!("  Objective: {}", proof.objective.statement);
    eprintln!(
        "  Selected plan: {:?} ({})",
        proof.selected_plan.strategy,
        proof.selected_plan.rationale
    );
    if !proof.candidate_plans.is_empty() {
        eprintln!("  Top candidate plans:");
        for (index, plan) in proof.candidate_plans.iter().take(3).enumerate() {
            let score = plan.expected_value - plan.estimated_cost;
            eprintln!(
                "    {}. {:?} | score {:.2} | expected {:.2} | cost {:.2}",
                index + 1,
                plan.strategy,
                score,
                plan.expected_value,
                plan.estimated_cost
            );
            eprintln!("       {}", plan.rationale);
        }
    }
    eprintln!(
        "  Confidence: {:.2} | Risk score: {:.4}",
        proof.confidence.overall(),
        proof.risk.score
    );
    eprintln!("  Evidence artifacts: {}", proof.evidence.len());
    if !proof.plan_pivots.is_empty() {
        eprintln!("  Plan pivots:");
        for pivot in &proof.plan_pivots {
            eprintln!(
                "    - turn {}: {:?} -> {:?} ({})",
                pivot.turn_number,
                pivot.from.strategy,
                pivot.to.strategy,
                pivot.trigger
            );
        }
    }
    if let Some(checkpoint_id) = &proof.rollback_checkpoint.checkpoint_id {
        eprintln!("  Rollback checkpoint: {}", checkpoint_id);
    }
    if !proof.realized_edits.is_empty() {
        eprintln!("  Realized edits:");
        for edit in &proof.realized_edits {
            eprintln!("    - {}: {}", edit.path, edit.summary);
        }
    }
    if !proof.unresolved_assumptions.is_empty() {
        eprintln!("  Unresolved assumptions:");
        for assumption in &proof.unresolved_assumptions {
            eprintln!("    - {}", assumption.description);
        }
    }
}

fn print_plans(loaded: Option<LoadedPlanningState>) {
    let Some(LoadedPlanningState {
        state,
        source,
        proof_summary,
    }) = loaded else {
        eprintln!("\x1b[2mNo planning state yet. Run a task first.\x1b[0m");
        return;
    };

    eprintln!("\x1b[2mRanked plans\x1b[0m");
    let source = match source {
        PlanningStateSource::Live => "live session",
        PlanningStateSource::Disk => "persisted snapshot",
    };
    eprintln!("  source: {}", source);
    if let Some(summary) = proof_summary {
        eprintln!(
            "  latest proof: confidence {:.2} | risk {:.4}",
            summary.confidence,
            summary.risk_score
        );
        eprintln!("  objective: {}", summary.objective);
        if let Some(path) = summary.proof_file {
            eprintln!("  proof file: {}", path);
        }
    }
    for (index, plan) in state.candidate_plans.iter().enumerate() {
        let score = plan.expected_value - plan.estimated_cost;
        let marker = if plan == &state.selected_plan { "*" } else { " " };
        eprintln!(
            "{} {}. {:?} | score {:.2} | expected {:.2} | cost {:.2}",
            marker,
            index + 1,
            plan.strategy,
            score,
            plan.expected_value,
            plan.estimated_cost
        );
        eprintln!("    {}", plan.rationale);
    }

    if !state.plan_pivots.is_empty() {
        eprintln!("\n\x1b[2mPivot history\x1b[0m");
        for pivot in &state.plan_pivots {
            eprintln!(
                "  turn {}: {:?} -> {:?} | {}",
                pivot.turn_number,
                pivot.from.strategy,
                pivot.to.strategy,
                pivot.trigger
            );
        }
    }
}

fn persist_proof_packet(project_root: &PathBuf, proof: &ProofPacket) -> Result<PathBuf> {
    let proofs_dir = project_root.join(".pipit").join("proofs");
    std::fs::create_dir_all(&proofs_dir)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let file_path = proofs_dir.join(format!("proof-{}.json", timestamp));
    let json = serde_json::to_string_pretty(proof)?;
    std::fs::write(&file_path, json)?;
    Ok(file_path)
}

fn planning_proof_summary(
    proof: &ProofPacket,
    proof_path: Option<&PathBuf>,
) -> Option<PlanningProofSummary> {
    Some(PlanningProofSummary {
        objective: proof.objective.statement.clone(),
        confidence: proof.confidence.overall(),
        risk_score: proof.risk.score,
        proof_file: proof_path.map(|path| path.display().to_string()),
    })
}

fn persist_planning_snapshot(
    project_root: &PathBuf,
    planning_state: &PlanningState,
    proof_summary: Option<PlanningProofSummary>,
) -> Result<()> {
    let plans_dir = project_root.join(".pipit").join("plans");
    std::fs::create_dir_all(&plans_dir)?;
    let file_path = plans_dir.join("latest.json");
    let snapshot = PlanningSnapshot {
        planning_state: planning_state.clone(),
        proof_summary,
    };
    let json = serde_json::to_string_pretty(&snapshot)?;
    std::fs::write(file_path, json)?;
    Ok(())
}

fn load_planning_snapshot(project_root: &PathBuf) -> Result<Option<LoadedPlanningState>> {
    let file_path = project_root.join(".pipit").join("plans").join("latest.json");
    if !file_path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(file_path)?;
    if let Ok(snapshot) = serde_json::from_str::<PlanningSnapshot>(&raw) {
        return Ok(Some(LoadedPlanningState {
            state: snapshot.planning_state,
            source: PlanningStateSource::Disk,
            proof_summary: snapshot.proof_summary,
        }));
    }

    let planning_state = serde_json::from_str::<PlanningState>(&raw)?;
    Ok(Some(LoadedPlanningState {
        state: planning_state,
        source: PlanningStateSource::Disk,
        proof_summary: None,
    }))
}
