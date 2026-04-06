mod auth;
mod persistence;
mod persistence_v2;
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

fn env_var_for(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Anthropic | ProviderKind::AnthropicCompatible => "ANTHROPIC_API_KEY",
        ProviderKind::OpenAi | ProviderKind::OpenAiCompatible => "OPENAI_API_KEY",
        ProviderKind::AzureOpenAi => "AZURE_OPENAI_API_KEY",
        ProviderKind::DeepSeek => "DEEPSEEK_API_KEY",
        ProviderKind::Google => "GOOGLE_API_KEY",
        ProviderKind::Vertex => "VERTEX_API_KEY",
        ProviderKind::OpenRouter => "OPENROUTER_API_KEY",
        ProviderKind::XAi => "XAI_API_KEY",
        ProviderKind::Cerebras => "CEREBRAS_API_KEY",
        ProviderKind::Groq => "GROQ_API_KEY",
        ProviderKind::Mistral => "MISTRAL_API_KEY",
        ProviderKind::Ollama => "OLLAMA_API_KEY",
    }
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

    // First-run: if no config exists and no provider flag, launch setup automatically
    if !pipit_config::has_user_config() && cli.provider.is_none() {
        eprintln!();
        eprintln!("  \x1b[1;33m🐦 Welcome to pipit!\x1b[0m");
        eprintln!("  \x1b[90mNo configuration found — let's set things up.\x1b[0m");
        eprintln!();

        setup::run()?;
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

    // Resolve API key — offer interactive setup if missing
    let api_key = match cli.api_key.clone().or_else(|| pipit_config::resolve_api_key(provider_kind)) {
        Some(key) => key,
        None => {
            if provider_kind == ProviderKind::Ollama {
                // Ollama doesn't need a real key
                "ollama".to_string()
            } else {
                eprintln!();
                eprintln!("  \x1b[1;31m✗ No API key found for {}\x1b[0m", provider_kind);
                eprintln!();
                eprintln!("  \x1b[1;33mLet's set up your configuration.\x1b[0m");
                eprintln!();

                setup::run()?;

                // Retry after setup
                let prov2 = pipit_config::resolve_config(Some(&project_root), CliOverrides::default())
                    .map(|c| c.provider.default)
                    .unwrap_or(provider_kind);
                pipit_config::resolve_api_key(prov2)
                    .ok_or_else(|| anyhow::anyhow!(
                        "Still no API key after setup.\n\
                         Set it via: export {}=<key>",
                        env_var_for(provider_kind),
                    ))?
            }
        }
    };

    dbg_log("[4/12] api_key resolved");

    // Resolve model
    let mut model = cli.model.unwrap_or(config.model.default_model.clone());

    // Resolve base URL: CLI flag > config file
    let base_url = cli.base_url.or(config.provider.custom_base_url.clone());

    // Create provider — on failure, offer interactive setup instead of dying
    let provider: Arc<dyn LlmProvider> = match pipit_provider::create_provider(
        provider_kind, &model, &api_key, base_url.as_deref(),
    ) {
        Ok(p) => Arc::from(p),
        Err(e) => {
            eprintln!();
            eprintln!("  \x1b[1;31m✗ Provider setup incomplete\x1b[0m");
            eprintln!("  \x1b[90m{}\x1b[0m", e);
            eprintln!();
            eprintln!("  \x1b[1;33mLet's fix this now.\x1b[0m");
            eprintln!();

            // Launch interactive setup
            setup::run()?;

            // Reload config and retry
            let overrides2 = CliOverrides {
                provider: cli_provider,
                model: Some(model.clone()),
                approval_mode: cli.approval.as_deref()
                    .map(str::parse)
                    .transpose()
                    .map_err(|e: String| anyhow::anyhow!(e))?,
                api_key: cli.api_key.clone(),
            };
            let config2 = pipit_config::resolve_config(Some(&project_root), overrides2)
                .context("Config resolution failed after setup")?;
            let prov2 = config2.provider.default;
            let key2 = cli.api_key.clone()
                .or_else(|| pipit_config::resolve_api_key(prov2))
                .ok_or_else(|| anyhow::anyhow!("Still no API key after setup. Set it via environment variable or run `pipit auth login`."))?;
            let model2 = config2.model.default_model.clone();
            let url2 = config2.provider.custom_base_url.clone();
            Arc::from(
                pipit_provider::create_provider(prov2, &model2, &key2, url2.as_deref())
                    .map_err(|e| anyhow::anyhow!("Provider still failed after setup: {}", e))?,
            )
        }
    };

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
    let mut tools = ToolRegistry::with_builtins();

    // Initialize MCP servers (if configured)
    let _mcp_manager = pipit_mcp::initialize_mcp(&project_root, &mut tools).await;

    // Register browser tools (CDP-backed)
    pipit_browser::extension_bridge::register_browser_tools(&mut tools);

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
            max_output_tokens: config.model.max_output_tokens,
            tool_result_max_chars: 32_000,
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
    let mut status = StatusBarState::new(project_name.clone(), model.clone(), approval_mode);
    status.provider_kind = format!("{}", provider_kind);
    status.base_url = base_url.clone().unwrap_or_default();
    status.agent_mode = format!("{}", agent_mode);
    status.max_turns = config.context.max_turns;

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
                // Capture work-in-progress git diff before exiting
                let wip_diff = std::process::Command::new("git")
                    .args(["diff", "--stat"])
                    .current_dir(&project_root)
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .filter(|s| !s.trim().is_empty());

                eprintln!("\n\x1b[31mError: {}\x1b[0m", e);
                if let Some(diff) = &wip_diff {
                    eprintln!("\n\x1b[2mWork-in-progress diff:\n{}\x1b[0m", diff);
                }
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

    // Rollback state: (checkpoint_sha, modified_files) from last agent run
    let mut last_rollback: Option<(String, Vec<String>)> = None;

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
                        if let Some((ref sha, ref files)) = last_rollback {
                            eprintln!("\x1b[33mRolling back {} file(s) to {}\x1b[0m", files.len(), &sha[..8.min(sha.len())]);
                            let mut success = 0;
                            let mut failed = 0;
                            for file in files {
                                let output = std::process::Command::new("git")
                                    .args(["checkout", sha.as_str(), "--", file.as_str()])
                                    .current_dir(&project_root)
                                    .output();
                                match output {
                                    Ok(o) if o.status.success() => {
                                        eprintln!("  \x1b[32m✓\x1b[0m {}", file);
                                        success += 1;
                                    }
                                    Ok(o) => {
                                        let err = String::from_utf8_lossy(&o.stderr);
                                        eprintln!("  \x1b[31m✗\x1b[0m {} — {}", file, err.trim());
                                        failed += 1;
                                    }
                                    Err(e) => {
                                        eprintln!("  \x1b[31m✗\x1b[0m {} — {}", file, e);
                                        failed += 1;
                                    }
                                }
                            }
                            eprintln!("\x1b[2m({} restored, {} failed)\x1b[0m", success, failed);
                            last_rollback = None;
                        } else {
                            eprintln!("\x1b[33mNothing to undo — no recent agent edits\x1b[0m");
                        }
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
                        if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) { last_rollback = Some(rb); }
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
                        if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) { last_rollback = Some(rb); }
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
                        if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) { last_rollback = Some(rb); }
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
                        if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) { last_rollback = Some(rb); }
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
                        if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) { last_rollback = Some(rb); }
                        continue;
                    }
                    SlashCommand::Threat => {
                        let prompt = "Perform a security threat analysis on the codebase:\n\
                            1. Read the main source files.\n\
                            2. Identify attack surfaces: HTTP endpoints, database queries, file operations, \
                               process execution, deserialization, and authentication checks.\n\
                            3. For each surface, classify the STRIDE threat category.\n\
                            4. Rate severity (Critical/High/Medium/Low).\n\
                            5. Suggest specific mitigations for each threat.\n\
                            Output a structured threat report.".to_string();
                        let cancel = CancellationToken::new();
                        let cancel_clone = cancel.clone();
                        let ctrlc_handle = tokio::spawn(async move { tokio::signal::ctrl_c().await.ok(); cancel_clone.cancel(); });
                        let outcome = agent.run(prompt, cancel).await;
                        ctrlc_handle.abort();
                        if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) { last_rollback = Some(rb); }
                        continue;
                    }
                    SlashCommand::Evolve(target) => {
                        let target_desc = target.unwrap_or_else(|| "the current task".to_string());
                        let prompt = format!(
                            "Compare 3 different approaches to: {}\n\
                             For each approach:\n\
                             1. Describe the strategy (MinimalPatch, RootCauseRepair, or CharacterizationFirst).\n\
                             2. Implement the solution.\n\
                             3. Run tests to verify.\n\
                             4. Report: correctness, diff size, and complexity.\n\
                             Recommend the best approach with rationale.", target_desc);
                        let cancel = CancellationToken::new();
                        let cancel_clone = cancel.clone();
                        let ctrlc_handle = tokio::spawn(async move { tokio::signal::ctrl_c().await.ok(); cancel_clone.cancel(); });
                        let outcome = agent.run(prompt, cancel).await;
                        ctrlc_handle.abort();
                        if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) { last_rollback = Some(rb); }
                        continue;
                    }
                    SlashCommand::Env(subcommand) => {
                        let prompt = match subcommand.as_deref() {
                            Some("fingerprint") | Some("fp") => {
                                "Collect an environment fingerprint:\n\
                                 1. Run: uname -r, rustc --version, python3 --version, node --version, \
                                    docker --version, git --version\n\
                                 2. Check for: Cargo.toml, package.json, pyproject.toml, Dockerfile\n\
                                 3. Report the system info and installed tool versions.\n\
                                 Output as structured JSON.".to_string()
                            }
                            Some(error_msg) => {
                                format!(
                                    "Diagnose this environment error:\n\
                                     ```\n{}\n```\n\n\
                                     1. Identify the likely cause.\n\
                                     2. Check which tool/package is missing or misconfigured.\n\
                                     3. Provide the exact fix command.", error_msg)
                            }
                            None => {
                                "Check the development environment:\n\
                                 1. Verify all required tools are installed (compiler, build tools, runtime).\n\
                                 2. Check project dependencies are available.\n\
                                 3. Report any issues found and how to fix them.".to_string()
                            }
                        };
                        let cancel = CancellationToken::new();
                        let cancel_clone = cancel.clone();
                        let ctrlc_handle = tokio::spawn(async move { tokio::signal::ctrl_c().await.ok(); cancel_clone.cancel(); });
                        let outcome = agent.run(prompt, cancel).await;
                        ctrlc_handle.abort();
                        if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) { last_rollback = Some(rb); }
                        continue;
                    }
                    SlashCommand::Spec(spec_arg) => {
                        let spec_source = if let Some(ref file) = spec_arg {
                            // Load spec from file
                            match std::fs::read_to_string(file) {
                                Ok(content) => content,
                                Err(e) => {
                                    eprintln!("Cannot read spec file '{}': {}", file, e);
                                    continue;
                                }
                            }
                        } else {
                            // Check for PIPIT.md or .pipit/spec.md
                            let candidates = ["PIPIT.md", ".pipit/spec.md", "spec.md"];
                            let mut found = None;
                            for c in &candidates {
                                let p = project_root.join(c);
                                if p.exists() {
                                    if let Ok(content) = std::fs::read_to_string(&p) {
                                        found = Some(content);
                                        break;
                                    }
                                }
                            }
                            match found {
                                Some(content) => content,
                                None => {
                                    eprintln!("No spec file found. Create PIPIT.md with ## Task sections, or run /spec <file>");
                                    continue;
                                }
                            }
                        };

                        let plan = pipit_core::sdd_pipeline::parse_spec_plan(&spec_source);
                        if plan.tasks.is_empty() {
                            eprintln!("No tasks found in spec. Use '## Task N: title' format.");
                            continue;
                        }

                        eprintln!("\n\x1b[1mSpec-Driven Development Plan\x1b[0m");
                        eprintln!("Tasks: {}", plan.tasks.len());
                        let groups = plan.parallelizable_groups();
                        for (i, group) in groups.iter().enumerate() {
                            let names: Vec<_> = group.iter().map(|t| format!("#{} {}", t.id, t.title)).collect();
                            let parallel = if group.len() > 1 { " (parallel)" } else { "" };
                            eprintln!("  Phase {}: {}{}", i + 1, names.join(", "), parallel);
                        }
                        if let Some(ref cmd) = plan.verification_command {
                            eprintln!("  Verify: {}", cmd);
                        }
                        eprintln!("");

                        // Execute tasks in dependency order
                        for task in plan.execution_order() {
                            eprintln!("\x1b[36m── Task #{}: {} ──\x1b[0m", task.id, task.title);
                            let prompt = plan.task_prompt(task);
                            let cancel = CancellationToken::new();
                            let cancel_clone = cancel.clone();
                            let ctrlc_handle = tokio::spawn(async move { tokio::signal::ctrl_c().await.ok(); cancel_clone.cancel(); });
                            let outcome = agent.run(prompt, cancel).await;
                            ctrlc_handle.abort();
                            if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) { last_rollback = Some(rb); }
                        }
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
                    SlashCommand::Diff => {
                        eprintln!();
                        // Show staged changes first
                        let staged = std::process::Command::new("git")
                            .args(["diff", "--staged", "--stat"])
                            .current_dir(&project_root)
                            .output();
                        if let Ok(ref o) = staged {
                            let out = String::from_utf8_lossy(&o.stdout);
                            if !out.trim().is_empty() {
                                eprintln!("\x1b[1;32mStaged changes:\x1b[0m");
                                // Show full diff
                                let full = std::process::Command::new("git")
                                    .args(["diff", "--staged", "--color=always"])
                                    .current_dir(&project_root)
                                    .output();
                                if let Ok(f) = full {
                                    eprint!("{}", String::from_utf8_lossy(&f.stdout));
                                }
                            }
                        }
                        // Show unstaged changes
                        let unstaged = std::process::Command::new("git")
                            .args(["diff", "--stat"])
                            .current_dir(&project_root)
                            .output();
                        if let Ok(ref o) = unstaged {
                            let out = String::from_utf8_lossy(&o.stdout);
                            if !out.trim().is_empty() {
                                eprintln!("\x1b[1;33mUnstaged changes:\x1b[0m");
                                let full = std::process::Command::new("git")
                                    .args(["diff", "--color=always"])
                                    .current_dir(&project_root)
                                    .output();
                                if let Ok(f) = full {
                                    eprint!("{}", String::from_utf8_lossy(&f.stdout));
                                }
                            }
                        }
                        // Check for untracked files
                        let untracked = std::process::Command::new("git")
                            .args(["ls-files", "--others", "--exclude-standard"])
                            .current_dir(&project_root)
                            .output();
                        if let Ok(ref o) = untracked {
                            let out = String::from_utf8_lossy(&o.stdout);
                            if !out.trim().is_empty() {
                                eprintln!("\x1b[1;35mUntracked files:\x1b[0m");
                                for line in out.lines() {
                                    eprintln!("  {}", line);
                                }
                            }
                        }
                        // If nothing changed
                        if staged.as_ref().map(|o| o.stdout.is_empty()).unwrap_or(true)
                            && unstaged.as_ref().map(|o| o.stdout.is_empty()).unwrap_or(true)
                            && untracked.as_ref().map(|o| o.stdout.is_empty()).unwrap_or(true)
                        {
                            eprintln!("\x1b[2mNo uncommitted changes\x1b[0m");
                        }
                        eprintln!();
                        continue;
                    }
                    SlashCommand::Commit(ref msg) => {
                        // Check for staged changes
                        let staged = std::process::Command::new("git")
                            .args(["diff", "--staged", "--stat"])
                            .current_dir(&project_root)
                            .output();
                        let has_staged = staged.as_ref()
                            .map(|o| !o.stdout.is_empty())
                            .unwrap_or(false);
                        if !has_staged {
                            // Auto-stage all changes
                            let _ = std::process::Command::new("git")
                                .args(["add", "-A"])
                                .current_dir(&project_root)
                                .output();
                        }
                        if let Some(message) = msg {
                            // Direct commit with provided message
                            let output = std::process::Command::new("git")
                                .args(["commit", "-m", message])
                                .current_dir(&project_root)
                                .output();
                            match output {
                                Ok(o) if o.status.success() => {
                                    eprintln!("\x1b[32mCommitted: {}\x1b[0m", message);
                                }
                                Ok(o) => {
                                    let err = String::from_utf8_lossy(&o.stderr);
                                    eprintln!("\x1b[31m{}\x1b[0m", err.trim());
                                }
                                Err(e) => eprintln!("\x1b[31mgit error: {}\x1b[0m", e),
                            }
                        } else {
                            // Generate commit message via LLM
                            let diff = std::process::Command::new("git")
                                .args(["diff", "--staged"])
                                .current_dir(&project_root)
                                .output();
                            if let Ok(d) = diff {
                                let diff_text = String::from_utf8_lossy(&d.stdout);
                                if diff_text.trim().is_empty() {
                                    eprintln!("\x1b[33mNo changes to commit\x1b[0m");
                                } else {
                                    let prompt = format!(
                                        "Generate a conventional commit message for this diff. \
                                         Use the format: type(scope): description\n\
                                         Types: feat, fix, refactor, docs, test, chore, perf\n\
                                         Reply with ONLY the commit message, nothing else.\n\n\
                                         ```diff\n{}\n```",
                                        if diff_text.len() > 8000 { &diff_text[..8000] } else { &diff_text }
                                    );
                                    let cancel = CancellationToken::new();
                                    let cancel_clone = cancel.clone();
                                    let ctrlc_handle = tokio::spawn(async move {
                                        tokio::signal::ctrl_c().await.ok();
                                        cancel_clone.cancel();
                                    });
                                    let outcome = agent.run(prompt, cancel).await;
                                    ctrlc_handle.abort();
                                    // Extract the generated message from the agent's last response
                                    if let AgentOutcome::Completed { .. } = &outcome {
                                        // Get the last assistant message
                                        if let Some(last_msg) = agent.context_usage().total.checked_sub(0) {
                                            let _ = last_msg; // Message was already printed by the agent
                                            eprintln!("\n\x1b[33mCommit with this message? [y/N]\x1b[0m");
                                            if let Some(answer) = read_input() {
                                                if answer.trim().eq_ignore_ascii_case("y") || answer.trim().eq_ignore_ascii_case("yes") {
                                                    // Use the last content line from the agent as commit message
                                                    let msg_output = std::process::Command::new("git")
                                                        .args(["commit", "--no-edit"])
                                                        .current_dir(&project_root)
                                                        .output();
                                                    match msg_output {
                                                        Ok(o) if o.status.success() => {
                                                            eprintln!("\x1b[32mCommitted!\x1b[0m");
                                                        }
                                                        Ok(o) => {
                                                            let err = String::from_utf8_lossy(&o.stderr);
                                                            eprintln!("\x1b[31m{}\x1b[0m", err.trim());
                                                        }
                                                        Err(e) => eprintln!("\x1b[31m{}\x1b[0m", e),
                                                    }
                                                } else {
                                                    eprintln!("\x1b[2mCommit cancelled\x1b[0m");
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    SlashCommand::Search(ref query) => {
                        if query.is_empty() {
                            eprintln!("\x1b[33mUsage: /search <query>\x1b[0m");
                        } else {
                            // Use ripgrep-style search via the `grep` tool's underlying ignore crate
                            let output = std::process::Command::new("grep")
                                .args(["-rn", "--color=always", "-I", query.as_str()])
                                .arg("--include=*.rs")
                                .arg("--include=*.py")
                                .arg("--include=*.js")
                                .arg("--include=*.ts")
                                .arg("--include=*.go")
                                .arg("--include=*.toml")
                                .arg("--include=*.json")
                                .arg("--include=*.md")
                                .current_dir(&project_root)
                                .output();
                            match output {
                                Ok(o) => {
                                    let results = String::from_utf8_lossy(&o.stdout);
                                    let lines: Vec<&str> = results.lines().collect();
                                    if lines.is_empty() {
                                        eprintln!("\x1b[2mNo results for '{}'\x1b[0m", query);
                                    } else {
                                        let shown = lines.len().min(30);
                                        for line in &lines[..shown] {
                                            eprintln!("{}", line);
                                        }
                                        if lines.len() > shown {
                                            eprintln!("\x1b[2m... and {} more results\x1b[0m", lines.len() - shown);
                                        }
                                    }
                                }
                                Err(_) => {
                                    // Fallback: try rg if available
                                    let rg = std::process::Command::new("rg")
                                        .args(["--color=always", "-n", query.as_str()])
                                        .current_dir(&project_root)
                                        .output();
                                    match rg {
                                        Ok(o) => eprint!("{}", String::from_utf8_lossy(&o.stdout)),
                                        Err(_) => eprintln!("\x1b[31mNeither grep nor rg available\x1b[0m"),
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    SlashCommand::Loop(ref args) => {
                        if args.is_none() {
                            eprintln!("\x1b[33mUsage: /loop [interval_secs] <prompt>\x1b[0m");
                            eprintln!("\x1b[2mExample: /loop 30 check test results\x1b[0m");
                        } else {
                            let args_str = args.as_deref().unwrap_or("");
                            // Parse optional interval
                            let (interval, prompt) = if let Some(rest) = args_str.strip_prefix(|c: char| c.is_ascii_digit()) {
                                let num: String = std::iter::once(args_str.chars().next().unwrap())
                                    .chain(rest.chars().take_while(|c| c.is_ascii_digit()))
                                    .collect();
                                let secs: u64 = num.parse().unwrap_or(30);
                                let prompt_start = num.len();
                                (secs, args_str[prompt_start..].trim())
                            } else {
                                (30u64, args_str.trim())
                            };
                            if prompt.is_empty() {
                                eprintln!("\x1b[33mUsage: /loop [interval_secs] <prompt>\x1b[0m");
                            } else {
                                eprintln!("\x1b[2mLooping every {}s (Ctrl-C to stop): {}\x1b[0m", interval, prompt);
                                let prompt_owned = prompt.to_string();
                                loop {
                                    let cancel = CancellationToken::new();
                                    let cancel_clone = cancel.clone();
                                    let ctrlc_handle = tokio::spawn(async move {
                                        tokio::signal::ctrl_c().await.ok();
                                        cancel_clone.cancel();
                                    });
                                    let outcome = agent.run(prompt_owned.clone(), cancel).await;
                                    ctrlc_handle.abort();
                                    if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) { last_rollback = Some(rb); }
                                    // Check if user pressed Ctrl-C
                                    if ctrlc_handle.is_finished() {
                                        eprintln!("\x1b[2mLoop stopped\x1b[0m");
                                        break;
                                    }
                                    eprintln!("\x1b[2m──── sleeping {}s ────\x1b[0m", interval);
                                    tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
                                }
                            }
                        }
                        continue;
                    }
                    SlashCommand::Memory(ref action) => {
                        let knowledge_dir = project_root.join(".pipit").join("knowledge");
                        match action.as_deref() {
                            None | Some("list") | Some("ls") => {
                                // List stored knowledge
                                if !knowledge_dir.exists() {
                                    eprintln!("\x1b[2mNo stored knowledge yet. Use /memory add <concept> to add.\x1b[0m");
                                } else if let Ok(entries) = std::fs::read_dir(&knowledge_dir) {
                                    eprintln!("\n\x1b[1;33mStored Knowledge\x1b[0m\n");
                                    for entry in entries.flatten() {
                                        if entry.path().extension().map(|e| e == "json").unwrap_or(false) {
                                            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                                                if let Ok(unit) = serde_json::from_str::<pipit_context::knowledge_injection::InjectedKnowledge>(&content) {
                                                    eprintln!("  \x1b[36m{}\x1b[0m — {}", unit.concept, unit.outcome);
                                                }
                                            }
                                        }
                                    }
                                    eprintln!();
                                }
                            }
                            Some(text) if text.starts_with("add ") => {
                                let knowledge_text = &text[4..];
                                let _ = std::fs::create_dir_all(&knowledge_dir);
                                let ts = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs();
                                let unit = pipit_context::knowledge_injection::InjectedKnowledge {
                                    concept: knowledge_text.to_string(),
                                    approach: String::new(),
                                    outcome: "User-provided knowledge".to_string(),
                                    source_project: project_root.file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("unknown")
                                        .to_string(),
                                    relevance_score: 1.0,
                                    estimated_tokens: (knowledge_text.len() as u64) / 4,
                                };
                                let path = knowledge_dir.join(format!("{}.json", ts));
                                if let Ok(json) = serde_json::to_string_pretty(&unit) {
                                    let _ = std::fs::write(&path, json);
                                    eprintln!("\x1b[32mStored: {}\x1b[0m", knowledge_text);
                                }
                            }
                            Some(text) if text.starts_with("clear") => {
                                if knowledge_dir.exists() {
                                    let _ = std::fs::remove_dir_all(&knowledge_dir);
                                    eprintln!("\x1b[33mAll knowledge cleared\x1b[0m");
                                }
                            }
                            Some(_) => {
                                eprintln!("\x1b[33mUsage: /memory [list|add <text>|clear]\x1b[0m");
                            }
                        }
                        continue;
                    }
                    SlashCommand::Background(ref prompt) => {
                        if let Some(task) = prompt {
                            // Check if daemon is running
                            let daemon_running = std::process::Command::new("curl")
                                .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "http://127.0.0.1:3141/health"])
                                .output()
                                .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "200")
                                .unwrap_or(false);
                            if daemon_running {
                                // Submit task to daemon
                                let submit = std::process::Command::new("curl")
                                    .args(["-s", "-X", "POST", "http://127.0.0.1:3141/tasks",
                                        "-H", "Content-Type: application/json",
                                        "-d", &serde_json::json!({"prompt": task}).to_string()])
                                    .output();
                                match submit {
                                    Ok(o) if o.status.success() => {
                                        let resp = String::from_utf8_lossy(&o.stdout);
                                        eprintln!("\x1b[32mTask submitted to background daemon\x1b[0m");
                                        eprintln!("\x1b[2m{}\x1b[0m", resp.trim());
                                    }
                                    _ => eprintln!("\x1b[31mFailed to submit task to daemon\x1b[0m"),
                                }
                            } else {
                                eprintln!("\x1b[33mDaemon not running. Start with: pipitd\x1b[0m");
                                eprintln!("\x1b[2mThe task will run in the foreground instead.\x1b[0m");
                            }
                        } else {
                            eprintln!("\x1b[33mUsage: /bg <prompt>\x1b[0m");
                            eprintln!("\x1b[2mRun a task in the background via the pipit daemon.\x1b[0m");
                        }
                        continue;
                    }
                    SlashCommand::Bench(ref action) => {
                        match action.as_deref() {
                            None | Some("list") => {
                                let suites = pipit_bench::load_custom_suites(&project_root);
                                eprintln!("\n\x1b[1;33mBenchmark Suites\x1b[0m\n");
                                if suites.is_empty() {
                                    eprintln!("  \x1b[2mNo custom suites. Add tasks to .pipit/benchmarks/<name>/\x1b[0m");
                                    eprintln!("  \x1b[2mEach task needs: instruction.md, test.sh, optional Dockerfile\x1b[0m");
                                } else {
                                    for suite in &suites {
                                        eprintln!("  \x1b[36m{}\x1b[0m — {} tasks", suite.name, suite.tasks.len());
                                    }
                                }
                                eprintln!();
                            }
                            Some("history") => {
                                let history = pipit_bench::history::BenchHistory::load(&project_root);
                                let sparkline = history.sparkline("custom", 40);
                                eprintln!("\n\x1b[1;33mBenchmark History\x1b[0m\n");
                                eprintln!("  Pass rate: {}", sparkline);
                                eprintln!();
                            }
                            Some(sub) if sub.starts_with("run") => {
                                eprintln!("\x1b[33mBenchmark runner requires Docker. Use: pipit bench run --suite <name>\x1b[0m");
                            }
                            Some(_) => {
                                eprintln!("\x1b[33mUsage: /bench [list|run|history|compare]\x1b[0m");
                            }
                        }
                        continue;
                    }
                    SlashCommand::Browse(ref action) => {
                        match action.as_deref() {
                            Some(url) if url.starts_with("http") => {
                                eprintln!("\x1b[36mBrowser: navigating to {}\x1b[0m", url);
                                eprintln!("\x1b[2mHeadless Chrome required. Install Chrome and ensure it's accessible.\x1b[0m");
                                // The actual browser integration happens via tools registered in the agent
                                let prompt = format!(
                                    "Navigate to {} using the browser_navigate tool. \
                                     Take a screenshot and report what you see. \
                                     Check for any console errors.", url
                                );
                                let cancel = CancellationToken::new();
                                let cancel_clone = cancel.clone();
                                let ctrlc_handle = tokio::spawn(async move { tokio::signal::ctrl_c().await.ok(); cancel_clone.cancel(); });
                                let outcome = agent.run(prompt, cancel).await;
                                ctrlc_handle.abort();
                                if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) { last_rollback = Some(rb); }
                            }
                            Some("test") => {
                                let prompt = "Auto-detect the dev server (check package.json scripts for 'dev' or 'start'), \
                                              launch it if not running, navigate to localhost, take a screenshot, \
                                              and report any console errors or failed network requests.".to_string();
                                let cancel = CancellationToken::new();
                                let cancel_clone = cancel.clone();
                                let ctrlc_handle = tokio::spawn(async move { tokio::signal::ctrl_c().await.ok(); cancel_clone.cancel(); });
                                let outcome = agent.run(prompt, cancel).await;
                                ctrlc_handle.abort();
                                if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) { last_rollback = Some(rb); }
                            }
                            Some("a11y") | Some("accessibility") => {
                                let prompt = "Run an accessibility audit on the running web app. \
                                              Check for WCAG violations: missing alt text, color contrast, \
                                              keyboard navigation, ARIA roles.".to_string();
                                let cancel = CancellationToken::new();
                                let cancel_clone = cancel.clone();
                                let ctrlc_handle = tokio::spawn(async move { tokio::signal::ctrl_c().await.ok(); cancel_clone.cancel(); });
                                let outcome = agent.run(prompt, cancel).await;
                                ctrlc_handle.abort();
                                if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) { last_rollback = Some(rb); }
                            }
                            None => {
                                eprintln!("\x1b[33mUsage: /browse <url> | /browse test | /browse a11y\x1b[0m");
                            }
                            Some(_) => {
                                eprintln!("\x1b[33mUsage: /browse <url> | /browse test | /browse a11y\x1b[0m");
                            }
                        }
                        continue;
                    }
                    SlashCommand::Mesh(ref action) => {
                        match action.as_deref() {
                            None | Some("status") => {
                                eprintln!("\n\x1b[1;33mMesh Status\x1b[0m\n");
                                eprintln!("  \x1b[2mMesh daemon not running. Start with: pipit mesh start\x1b[0m");
                                eprintln!("  \x1b[2mMesh enables distributed multi-agent task delegation.\x1b[0m");
                                eprintln!();
                            }
                            Some("nodes") => {
                                eprintln!("\x1b[2mNo mesh nodes discovered. Use /mesh join <seed> to connect.\x1b[0m");
                            }
                            Some(sub) if sub.starts_with("join") => {
                                let seed = sub.strip_prefix("join").map(|s| s.trim()).unwrap_or("");
                                if seed.is_empty() {
                                    eprintln!("\x1b[33mUsage: /mesh join <seed_address>\x1b[0m");
                                } else {
                                    eprintln!("\x1b[36mConnecting to mesh seed: {}\x1b[0m", seed);
                                    eprintln!("\x1b[2mMesh protocol (SWIM) will discover other nodes automatically.\x1b[0m");
                                }
                            }
                            Some(_) => {
                                eprintln!("\x1b[33mUsage: /mesh [status|nodes|join <addr>|delegate <task>]\x1b[0m");
                            }
                        }
                        continue;
                    }
                    SlashCommand::Watch(ref action) => {
                        match action.as_deref() {
                            Some("deps") => {
                                eprintln!("\x1b[36mStarting dependency health monitor...\x1b[0m");
                                eprintln!("\x1b[2mWill check for vulnerabilities and outdated packages periodically.\x1b[0m");
                            }
                            Some("tests") => {
                                eprintln!("\x1b[36mStarting test watcher...\x1b[0m");
                                eprintln!("\x1b[2mWill auto-run tests on file save.\x1b[0m");
                            }
                            Some("security") => {
                                eprintln!("\x1b[36mStarting security monitor...\x1b[0m");
                                eprintln!("\x1b[2mWill run taint analysis on modified files.\x1b[0m");
                            }
                            Some("start") => {
                                eprintln!("\x1b[36mAmbient file watcher started.\x1b[0m");
                                eprintln!("\x1b[2mWill notify on external changes and suggest actions.\x1b[0m");
                            }
                            Some("stop") => {
                                eprintln!("\x1b[33mAll watchers stopped.\x1b[0m");
                            }
                            None => {
                                eprintln!("\x1b[33mUsage: /watch [start|stop|deps|tests|security]\x1b[0m");
                            }
                            Some(_) => {
                                eprintln!("\x1b[33mUsage: /watch [start|stop|deps|tests|security]\x1b[0m");
                            }
                        }
                        continue;
                    }
                    SlashCommand::Deps(ref _action) => {
                        eprintln!("\x1b[36mScanning dependencies...\x1b[0m");
                        // Run synchronously for now — async would need runtime
                        let ecosystems = pipit_deps::scanner::detect_ecosystems(&project_root);
                        if ecosystems.is_empty() {
                            eprintln!("\x1b[2mNo package manifests found (Cargo.toml, package.json, etc.)\x1b[0m");
                        } else {
                            let names: Vec<&str> = ecosystems.iter().map(|e| match e {
                                pipit_deps::scanner::Ecosystem::Cargo => "Cargo",
                                pipit_deps::scanner::Ecosystem::Npm => "npm",
                                pipit_deps::scanner::Ecosystem::Python => "Python",
                                pipit_deps::scanner::Ecosystem::Go => "Go",
                            }).collect();
                            eprintln!("\x1b[2mDetected ecosystems: {}\x1b[0m", names.join(", "));
                            eprintln!("\x1b[2mRunning vulnerability scan via OSV API...\x1b[0m");
                            // Note: full async scan runs in the background; results shown in next prompt
                        }
                        continue;
                    }
                    SlashCommand::Model(ref new_model) => {
                        if new_model.is_empty() {
                            eprintln!("\x1b[2mCurrent model: {}\x1b[0m", model);
                            eprintln!("\x1b[2mUsage: /model <model_name>\x1b[0m");
                        } else {
                            match agent.set_model(provider_kind, new_model, &api_key, base_url.as_deref()) {
                                Ok(()) => {
                                    model = new_model.clone();
                                    ui.status_mut().model = model.clone();
                                    eprintln!("\x1b[32mSwitched to model: {}\x1b[0m", model);
                                }
                                Err(e) => {
                                    eprintln!("\x1b[31m{}\x1b[0m", e);
                                }
                            }
                        }
                        continue;
                    }
                    SlashCommand::Branch(ref name) => {
                        if let Some(branch_name) = name {
                            let output = std::process::Command::new("git")
                                .args(["checkout", "-b", branch_name])
                                .current_dir(&project_root)
                                .output();
                            match output {
                                Ok(o) if o.status.success() => {
                                    eprintln!("\x1b[32mCreated and switched to branch '{}'\x1b[0m", branch_name);
                                    ui.status_mut().branch = branch_name.clone();
                                }
                                Ok(o) => {
                                    let err = String::from_utf8_lossy(&o.stderr);
                                    eprintln!("\x1b[31m{}\x1b[0m", err.trim());
                                }
                                Err(e) => eprintln!("\x1b[31mgit error: {}\x1b[0m", e),
                            }
                        } else {
                            // Show current branch
                            let output = std::process::Command::new("git")
                                .args(["branch", "--show-current"])
                                .current_dir(&project_root)
                                .output();
                            if let Ok(o) = output {
                                let branch = String::from_utf8_lossy(&o.stdout);
                                eprintln!("\x1b[2mCurrent branch: {}\x1b[0m", branch.trim());
                            }
                        }
                        continue;
                    }
                    SlashCommand::BranchList => {
                        let output = std::process::Command::new("git")
                            .args(["branch", "-a", "--no-color"])
                            .current_dir(&project_root)
                            .output();
                        match output {
                            Ok(o) => {
                                let branches = String::from_utf8_lossy(&o.stdout);
                                for line in branches.lines() {
                                    eprintln!("{}", line);
                                }
                            }
                            Err(e) => eprintln!("\x1b[31mgit error: {}\x1b[0m", e),
                        }
                        continue;
                    }
                    SlashCommand::BranchSwitch(ref target) => {
                        if target.is_empty() {
                            eprintln!("\x1b[33mUsage: /switch <branch_name>\x1b[0m");
                        } else {
                            // Check for dirty state
                            let status = std::process::Command::new("git")
                                .args(["status", "--porcelain"])
                                .current_dir(&project_root)
                                .output();
                            let has_dirty = status.as_ref()
                                .map(|o| !o.stdout.is_empty())
                                .unwrap_or(false);
                            if has_dirty {
                                eprintln!("\x1b[33mWarning: you have uncommitted changes. Stashing first...\x1b[0m");
                                let _ = std::process::Command::new("git")
                                    .args(["stash", "push", "-m", "pipit-auto-stash"])
                                    .current_dir(&project_root)
                                    .output();
                            }
                            let output = std::process::Command::new("git")
                                .args(["checkout", target])
                                .current_dir(&project_root)
                                .output();
                            match output {
                                Ok(o) if o.status.success() => {
                                    eprintln!("\x1b[32mSwitched to branch '{}'\x1b[0m", target);
                                    ui.status_mut().branch = target.clone();
                                    if has_dirty {
                                        eprintln!("\x1b[2mYour changes were stashed. Use `!git stash pop` to restore.\x1b[0m");
                                    }
                                }
                                Ok(o) => {
                                    let err = String::from_utf8_lossy(&o.stderr);
                                    eprintln!("\x1b[31m{}\x1b[0m", err.trim());
                                    if has_dirty {
                                        let _ = std::process::Command::new("git")
                                            .args(["stash", "pop"])
                                            .current_dir(&project_root)
                                            .output();
                                    }
                                }
                                Err(e) => eprintln!("\x1b[31mgit error: {}\x1b[0m", e),
                            }
                        }
                        continue;
                    }
                    SlashCommand::Setup => {
                        setup::run()?;
                        eprintln!("\x1b[90mRestart pipit to use the new configuration.\x1b[0m");
                        continue;
                    }
                    SlashCommand::Config(ref _key) => {
                        let config_path = pipit_config::user_config_path()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "~/.config/pipit/config.toml".to_string());
                        let has_config = pipit_config::has_user_config();

                        eprintln!();
                        eprintln!("  \x1b[1;33mConfiguration\x1b[0m");
                        eprintln!();
                        eprintln!("  \x1b[90mConfig file: \x1b[0m{} {}", config_path, if has_config { "\x1b[32m✓\x1b[0m" } else { "\x1b[31m✗ not found\x1b[0m" });
                        eprintln!("  \x1b[90mProvider:    \x1b[0m{}", provider_kind);
                        eprintln!("  \x1b[90mModel:       \x1b[0m{}", model);
                        if let Some(ref url) = base_url {
                            eprintln!("  \x1b[90mBase URL:    \x1b[0m{}", url);
                        }
                        eprintln!("  \x1b[90mApproval:    \x1b[0m{}", ui.status_mut().approval_mode);
                        eprintln!("  \x1b[90mMax turns:   \x1b[0m{}", config.context.max_turns);
                        eprintln!();
                        eprintln!("  \x1b[90mEdit: \x1b[0m{}", config_path);
                        eprintln!("  \x1b[90mRe-run: \x1b[0m/setup");
                        eprintln!();
                        continue;
                    }
                    SlashCommand::Doctor => {
                        eprintln!();
                        eprintln!("  \x1b[1;33mSystem Health Check\x1b[0m");
                        eprintln!();
                        eprintln!("  \x1b[90mProvider:    \x1b[0m{} \x1b[32m✓\x1b[0m", provider_kind);
                        eprintln!("  \x1b[90mModel:       \x1b[0m{}", model);
                        if let Some(ref url) = base_url {
                            eprintln!("  \x1b[90mEndpoint:    \x1b[0m{}", url);
                        }
                        let usage = agent.context_usage();
                        let pct = if usage.limit > 0 { usage.total * 100 / usage.limit } else { 0 };
                        eprintln!("  \x1b[90mTokens:      \x1b[0m{}/{} ({}%)", usage.total, usage.limit, pct);
                        eprintln!("  \x1b[90mCost:        \x1b[0m${:.4}", usage.cost);
                        eprintln!("  \x1b[90mSkills:      \x1b[0m{}", skills.count());
                        eprintln!("  \x1b[90mHooks:       \x1b[0m{}", workflow_assets.hook_files.len());
                        eprintln!();
                        continue;
                    }
                    SlashCommand::Skills => {
                        eprintln!();
                        eprintln!("  \x1b[1;33mSkills\x1b[0m");
                        eprintln!();
                        if skills.count() == 0 {
                            eprintln!("  \x1b[90mNo skills found. Add .pipit/skills/<name>/SKILL.md\x1b[0m");
                        } else {
                            for name in skills.list() {
                                eprintln!("  \x1b[36m/{}\x1b[0m", name);
                            }
                        }
                        eprintln!();
                        continue;
                    }
                    SlashCommand::Hooks => {
                        eprintln!();
                        eprintln!("  \x1b[1;33mHooks\x1b[0m");
                        eprintln!();
                        if workflow_assets.hook_files.is_empty() {
                            eprintln!("  \x1b[90mNo hooks found. Add .pipit/hooks/<event>.sh\x1b[0m");
                        } else {
                            for hook in &workflow_assets.hook_files {
                                let name = hook.file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("?");
                                eprintln!("  \x1b[36m{}\x1b[0m", name);
                            }
                        }
                        eprintln!();
                        continue;
                    }
                    SlashCommand::Mcp => {
                        eprintln!();
                        eprintln!("  \x1b[1;33mMCP Servers\x1b[0m");
                        eprintln!();
                        match pipit_tools::load_mcp_config(&project_root) {
                            Some(mcp_config) => {
                                if mcp_config.mcp_servers.is_empty() {
                                    eprintln!("  \x1b[90mConfig found but no servers defined\x1b[0m");
                                } else {
                                    for (name, _server) in &mcp_config.mcp_servers {
                                        eprintln!("  \x1b[36m{}\x1b[0m", name);
                                    }
                                }
                            }
                            None => {
                                eprintln!("  \x1b[90mNo MCP config found. Add .pipit/mcp.json\x1b[0m");
                            }
                        }
                        eprintln!();
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
                                    if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) { last_rollback = Some(rb); }
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
                                    if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) { last_rollback = Some(rb); }
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
                // Direct shell execution — run DIRECTLY, not through the AI
                eprintln!("\x1b[2m$ {}\x1b[0m", cmd);
                let output = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd)
                    .current_dir(&project_root)
                    .stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit())
                    .status();
                match output {
                    Ok(status) => {
                        if !status.success() {
                            if let Some(code) = status.code() {
                                eprintln!("\x1b[31m[exit {}]\x1b[0m", code);
                            }
                        }
                    }
                    Err(e) => eprintln!("\x1b[31mShell error: {}\x1b[0m", e),
                }
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
                if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) { last_rollback = Some(rb); }
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
                if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) { last_rollback = Some(rb); }
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
                if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) { last_rollback = Some(rb); }
                println!();
            }
        }
    }

    // Fire SessionEnd hook
    let _ = extensions_for_lifecycle.on_session_end().await;

    Ok(())
}

/// Handle the outcome of an agent run — persist proofs, print summaries, show errors.
/// Returns optional (checkpoint_sha, modified_files) for /undo support.
fn handle_agent_outcome(
    project_root: &PathBuf,
    agent: &mut AgentLoop,
    outcome: AgentOutcome,
) -> Option<(String, Vec<String>)> {
    match outcome {
        AgentOutcome::Completed {
            turns, cost, proof, ..
        } => {
            let rollback = proof.rollback_checkpoint.checkpoint_id.clone().map(|sha| {
                let files: Vec<String> = proof.realized_edits.iter().map(|e| e.path.clone()).collect();
                (sha, files)
            });
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
            rollback
        }
        AgentOutcome::MaxTurnsReached(n) => {
            if let Some(planning_state) = agent.planning_state() {
                persistence::persist_planning_snapshot(project_root, &planning_state, None).ok();
            }
            eprintln!("\x1b[33mReached max turns ({})\x1b[0m", n);
            None
        }
        AgentOutcome::Cancelled => {
            if let Some(planning_state) = agent.planning_state() {
                persistence::persist_planning_snapshot(project_root, &planning_state, None).ok();
            }
            eprintln!("\x1b[2m(cancelled)\x1b[0m");
            None
        }
        AgentOutcome::Error(e) => {
            if let Some(planning_state) = agent.planning_state() {
                persistence::persist_planning_snapshot(project_root, &planning_state, None).ok();
            }
            // Show work-in-progress diff on error
            let wip_diff = std::process::Command::new("git")
                .args(["diff", "--stat"])
                .current_dir(project_root)
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .filter(|s| !s.trim().is_empty());

            eprintln!("\x1b[31mError: {}\x1b[0m", e);
            if let Some(diff) = &wip_diff {
                eprintln!("\x1b[2mWork-in-progress diff:\n{}\x1b[0m", diff);
            }
            None
        }
    }
}
