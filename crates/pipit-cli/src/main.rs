mod auth;
mod persistence;
mod prompt_builder;
mod setup;
mod tui;
mod update;
mod workflow;

use persistence::{LoadedPlanningState, PlanningStateSource};

use anyhow::{Context, Result};
use clap::Parser;
use pipit_config::{ApprovalMode, CliOverrides, ProviderKind};
use pipit_context::{ContextManager, budget::ContextSettings};
use pipit_core::{AgentLoop, AgentLoopConfig, AgentOutcome, PlanningState};
use pipit_extensions::HookExtensionRunner;
use pipit_intelligence::RepoMap;
use pipit_io::input::{classify_input, read_input, SlashCommand, UserInput};
use pipit_io::{PipitUi, InteractiveApprovalHandler, StatusBarState};
use pipit_provider::LlmProvider;
use pipit_skills::SkillRegistry;
use pipit_tools::ToolRegistry;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use workflow::WorkflowAssets;

// ── Debug logger (writes to /tmp/pipit-debug.log when --debug is set) ────
use std::sync::atomic::{AtomicBool, Ordering};

static DEBUG_ENABLED: AtomicBool = AtomicBool::new(false);

/// Write a timestamped diagnostic line to /tmp/pipit-debug.log.
/// No-op unless `--debug` was passed.
pub(crate) fn dbg_log(msg: &str) {
    if !DEBUG_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    use std::io::Write;
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let ts = format!("{}.{:03}", elapsed.as_secs(), elapsed.subsec_millis());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/pipit-debug.log")
    {
        let _ = writeln!(f, "[{}] {}", ts, msg);
    }
}

#[derive(Parser, Debug)]
#[command(name = "pipit", version = env!("CARGO_PKG_VERSION"), about = "AI coding agent")]
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

    /// Use classic REPL mode instead of the full-screen TUI
    #[arg(long, default_value_t = false)]
    classic: bool,

    /// Write detailed startup diagnostics to /tmp/pipit-debug.log
    #[arg(long, default_value_t = false)]
    debug: bool,

    /// Agent mode: fast, balanced, guarded, custom
    ///
    /// fast     — direct execution, no verification overhead
    /// balanced — plans before acting, heuristic verification  
    /// guarded  — full plan/execute/verify with repair loops
    /// custom   — guarded with user-specified role models
    #[arg(long, default_value = "fast")]
    mode: String,

    // ── Expert: role model overrides (hidden from default --help) ──

    /// [expert] Planner model override (for custom mode)
    #[arg(long, hide = true)]
    planner_model: Option<String>,

    /// [expert] Planner provider override (for custom mode)
    #[arg(long, hide = true)]
    planner_provider: Option<String>,

    /// [expert] Planner base URL override (for custom mode)
    #[arg(long, hide = true)]
    planner_base_url: Option<String>,

    /// [expert] Verifier model override (for custom mode)
    #[arg(long, hide = true)]
    verifier_model: Option<String>,

    /// [expert] Verifier provider override (for custom mode)
    #[arg(long, hide = true)]
    verifier_provider: Option<String>,

    /// [expert] Verifier base URL override (for custom mode)
    #[arg(long, hide = true)]
    verifier_base_url: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    /// Manage provider authentication
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Update pipit to the latest version
    Update,
    /// Interactive setup wizard — configure provider, model, and preferences
    Setup,
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
    // Initialize tracing with a TUI-safe writer that suppresses output
    // while the full-screen ratatui alternate screen is active.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("pipit=info".parse().unwrap()),
        )
        .with_target(false)
        .with_writer(|| pipit_io::TuiSafeStderr)
        .init();

    let cli = Cli::parse();

    // Enable debug logging if requested
    if cli.debug {
        DEBUG_ENABLED.store(true, Ordering::Relaxed);
        // Truncate previous log
        let _ = std::fs::write("/tmp/pipit-debug.log", "");
        dbg_log("=== pipit startup (--debug) ===");
        dbg_log(&format!("version: {}", env!("CARGO_PKG_VERSION")));
        dbg_log(&format!("classic: {}, mode: {}, repomap: {}", cli.classic, cli.mode, cli.repomap));
    }

    // Handle subcommands early (before provider resolution)
    match &cli.command {
        Some(Commands::Auth { action }) => return auth::handle(action).await,
        Some(Commands::Update) => return update::self_update().await,
        Some(Commands::Setup) => return setup::run(),
        None => {}
    }

    dbg_log("[1/12] subcommand dispatch done");

    // Background version check (non-blocking)
    let update_msg = tokio::spawn(update::check_for_update_background());

    // First-run hint: if no config exists and no provider flag, guide the user
    if !pipit_config::has_user_config() && cli.provider.is_none() {
        eprintln!();
        eprintln!("  \x1b[1;33mFirst time?\x1b[0m Run \x1b[1mpipit setup\x1b[0m for interactive configuration.");
        eprintln!("  \x1b[90mOr pass flags: pipit --provider openai --model gpt-4o\x1b[0m");
        eprintln!();
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

    dbg_log(&format!("[2/12] project_root={}", project_root.display()));

    let config =
        pipit_config::resolve_config(Some(&project_root), overrides).context("Config resolution failed")?;

    let provider_kind = config.provider.default;
    dbg_log(&format!("[3/12] config resolved, provider={}", provider_kind));

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
            anyhow::anyhow!(
                "No API key found for {}.\n\n\
                 Quick fix (pick one):\n\
                 1. pipit setup            Interactive config wizard\n\
                 2. export {}=<key>   Environment variable\n\
                 3. pipit auth login {}    Store in credentials\n\
                 4. pipit --api-key <key>  One-time flag\n\n\
                 Config is saved to ~/.config/pipit/config.toml",
                provider_kind, env_var, provider_kind
            )
        })?;

    dbg_log("[4/12] api_key resolved");

    // Resolve model
    let model = cli.model.unwrap_or(config.model.default_model.clone());

    // Resolve base URL: CLI flag > config file
    let base_url = cli.base_url.or(config.provider.custom_base_url.clone());

    // Create provider
    let provider: Arc<dyn LlmProvider> = Arc::from(
        pipit_provider::create_provider(provider_kind, &model, &api_key, base_url.as_deref())
            .map_err(|e| anyhow::anyhow!("Provider creation failed for '{}': {}", model, e))?,

    );

    dbg_log(&format!("[5/12] provider created: {} / {}", provider_kind, model));

    // Build model router based on agent mode
    let agent_mode: pipit_core::AgentMode = cli.mode.parse()
        .map_err(|e: String| anyhow::anyhow!("{}", e))?;

    // Auto-promote to Custom if role overrides are specified
    let agent_mode = if agent_mode != pipit_core::AgentMode::Custom
        && (cli.planner_model.is_some() || cli.planner_provider.is_some()
            || cli.verifier_model.is_some() || cli.verifier_provider.is_some()
            || cli.planner_base_url.is_some() || cli.verifier_base_url.is_some())
    {
        pipit_core::AgentMode::Custom
    } else {
        agent_mode
    };

    let pev_config = agent_mode.to_pev_config();

    let models = if agent_mode == pipit_core::AgentMode::Custom {
        use pipit_core::{ModelRouter, RoleProvider, ModelRole};

        let planner_model_id = cli.planner_model.as_deref().unwrap_or(&model);
        let verifier_model_id = cli.verifier_model.as_deref().unwrap_or(&model);

        // Warn if planner/verifier uses a non-reasoning model
        let non_reasoning_hints = ["non-reasoning", "fast", "mini", "instant", "flash", "haiku"];
        for (role, model_id) in [("planner", planner_model_id), ("verifier", verifier_model_id)] {
            let lower = model_id.to_lowercase();
            if non_reasoning_hints.iter().any(|h| lower.contains(h)) {
                eprintln!(
                    "  \x1b[1;33m⚠ Warning:\x1b[0m {} model '{}' may lack reasoning capability.",
                    role, model_id
                );
                eprintln!(
                    "  \x1b[90m  The {} role works best with a thinking model (e.g. claude-sonnet, gpt-4o, deepseek-chat).\x1b[0m",
                    role
                );
                eprintln!(
                    "  \x1b[90m  Non-reasoning models are better suited for the executor role.\x1b[0m"
                );
                eprintln!();
            }
        }

        let make_provider = |role_model: &str, role_provider_str: Option<&str>, role_base_url: Option<&str>| -> Result<Arc<dyn LlmProvider>, anyhow::Error> {
            let rp_kind: ProviderKind = if let Some(p) = role_provider_str {
                p.parse().map_err(|e: String| anyhow::anyhow!("{}", e))?
            } else {
                provider_kind
            };
            let rp_key = pipit_config::resolve_api_key(rp_kind)
                .unwrap_or_else(|| api_key.clone());
            let rp_base = role_base_url.or(base_url.as_deref());
            Ok(Arc::from(pipit_provider::create_provider(rp_kind, role_model, &rp_key, rp_base)
                .map_err(|e| anyhow::anyhow!("Provider creation for {} failed: {}", role_model, e))?))
        };

        let planner_provider = if cli.planner_model.is_some() || cli.planner_provider.is_some() || cli.planner_base_url.is_some() {
            make_provider(planner_model_id, cli.planner_provider.as_deref(), cli.planner_base_url.as_deref())?
        } else {
            provider.clone()
        };

        let verifier_provider = if cli.verifier_model.is_some() || cli.verifier_provider.is_some() || cli.verifier_base_url.is_some() {
            make_provider(verifier_model_id, cli.verifier_provider.as_deref(), cli.verifier_base_url.as_deref())?
        } else {
            provider.clone()
        };

        let router = ModelRouter::new(
            RoleProvider { provider: planner_provider, model_id: planner_model_id.to_string(), role: ModelRole::Planner },
            RoleProvider { provider: provider.clone(), model_id: model.clone(), role: ModelRole::Executor },
            RoleProvider { provider: verifier_provider, model_id: verifier_model_id.to_string(), role: ModelRole::Verifier },
        );

        eprintln!("pipit› mode: custom | planner: {} | executor: {} | verifier: {}",
            planner_model_id, model, verifier_model_id);

        router
    } else {
        if agent_mode != pipit_core::AgentMode::Fast {
            eprintln!("pipit› mode: {} — {}", agent_mode, agent_mode.description());
        }
        pipit_core::ModelRouter::single(provider.clone(), model.clone())
    };

    dbg_log(&format!("[6/12] model_router built, mode={}", agent_mode));

    // Build tool registry
    let tools = ToolRegistry::with_builtins();

    let workflow_assets = WorkflowAssets::discover(&project_root);

    // Discover skills (#21: progressive disclosure)
    let skill_paths: Vec<PathBuf> = workflow_assets.skill_search_paths();
    let mut skills = SkillRegistry::discover(&skill_paths);
    if skills.count() > 0 {
        tracing::info!("Skills: {} discovered", skills.count());
    }

    dbg_log(&format!("[7/12] tools={}, skills={}, workflow_assets loaded", tools.tool_names().len(), skills.count()));

    // Build system prompt (with skill index injected as Tier 1)
    let system_prompt = prompt_builder::build_system_prompt(
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

    dbg_log("[8/12] system_prompt + context_manager built");

    // Build RepoMap — skip if project_root is not a git repo (e.g. user's home dir)
    // to avoid scanning millions of files and hanging forever.
    let is_git_repo = project_root.join(".git").exists();
    let repo_map_text = if cli.repomap && is_git_repo {
        dbg_log(&format!("[8.5] building repomap for {}", project_root.display()));
        let intelligence_config = pipit_intelligence::IntelligenceConfig::default();
        let repo_map = RepoMap::build(&project_root, intelligence_config);
        if repo_map.file_count() > 0 {
            let map = repo_map.render(&[], 4096);
            tracing::info!("RepoMap: {} files indexed", repo_map.file_count());
            dbg_log(&format!("[8.5] repomap: {} files indexed", repo_map.file_count()));
            context.update_repo_map_tokens((map.len() as u64) / 4);
            Some(map)
        } else {
            None
        }
    } else {
        if cli.repomap && !is_git_repo {
            dbg_log("[8.5] skipping repomap — not a git repo");
            tracing::info!("RepoMap skipped — {} is not a git repository", project_root.display());
        }
        None
    };

    dbg_log("[9/12] repomap done");

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
        pev: pev_config,
        ..Default::default()
    };

    dbg_log("[10/12] agent_config built");

    // Build approval handler
    let approval_handler: Arc<dyn pipit_core::ApprovalHandler> =
        Arc::new(InteractiveApprovalHandler);

    let (mut agent, mut event_rx, _steering_tx) = AgentLoop::new(
        models,
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

    dbg_log("[11/12] agent + event_rx created");

    // Create UI
    let mut ui = PipitUi::new(show_thinking, true, trace_ui, status.clone());

    // Show update notification if available.
    // Use a short timeout so a slow/unreachable GitHub API never stalls startup.
    let _update_notice = match tokio::time::timeout(
        std::time::Duration::from_millis(800),
        update_msg,
    ).await {
        Ok(Ok(Some(msg))) => {
            eprintln!("\x1b[33m{}\x1b[0m\n", msg);
            Some(msg)
        }
        _ => None,
    };

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
                let proof_path = persistence::persist_proof_packet(&project_root, &proof).ok();
                if let Some(planning_state) = agent.planning_state() {
                    persistence::persist_planning_snapshot(
                        &project_root,
                        &planning_state,
                        persistence::planning_proof_summary(&proof, proof_path.as_ref()),
                    )
                    .ok();
                }
                persistence::print_proof_summary(&proof);
                eprintln!("\n\x1b[2m({} turns, ${:.4})\x1b[0m", turns, cost);
            }
            AgentOutcome::Error(e) => {
                if let Some(planning_state) = agent.planning_state() {
                    persistence::persist_planning_snapshot(&project_root, &planning_state, None).ok();
                }
                eprintln!("\n\x1b[31mError: {}\x1b[0m", e);
                std::process::exit(1);
            }
            _ => {
                if let Some(planning_state) = agent.planning_state() {
                    persistence::persist_planning_snapshot(&project_root, &planning_state, None).ok();
                }
            }
        }

        return Ok(());
    }

    dbg_log("[12/12] pre-TUI: all init done, entering TUI/REPL");

    // ── TUI mode (default) vs classic REPL ───────────────────────────────
    if !cli.classic {
        return tui::run(
            agent,
            &mut event_rx,
            &project_root,
            &mut skills,
            &workflow_assets,
            &extensions_for_lifecycle,
            status,
            trace_ui,
            agent_mode,
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
                            .or_else(|| persistence::load_planning_snapshot(&project_root).ok().flatten());
                       persistence::print_plans(state);
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
            let proof_path = persistence::persist_proof_packet(project_root, &proof).ok();
            if let Some(planning_state) = agent.planning_state() {
                persistence::persist_planning_snapshot(
                    project_root,
                    &planning_state,
                    persistence::planning_proof_summary(&proof, proof_path.as_ref()),
                )
                .ok();
            }
            persistence::print_proof_summary(&proof);
            eprintln!("\x1b[2m({} turns, ${:.4})\x1b[0m", turns, cost);
        }
        AgentOutcome::MaxTurnsReached(n) => {
            if let Some(planning_state) = agent.planning_state() {
                persistence::persist_planning_snapshot(project_root, &planning_state, None).ok();
            }
            eprintln!("\x1b[33mReached max turns ({})\x1b[0m", n);
        }
        AgentOutcome::Cancelled => {
            if let Some(planning_state) = agent.planning_state() {
                persistence::persist_planning_snapshot(project_root, &planning_state, None).ok();
            }
            eprintln!("\x1b[2m(cancelled)\x1b[0m");
        }
        AgentOutcome::Error(e) => {
            if let Some(planning_state) = agent.planning_state() {
                persistence::persist_planning_snapshot(project_root, &planning_state, None).ok();
            }
            eprintln!("\x1b[31mError: {}\x1b[0m", e);
        }
    }
}
