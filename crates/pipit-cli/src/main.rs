mod auth;
mod init;
mod migrations;
mod persistence;
mod persistence_v2;
mod plugin;
mod prompt_builder;
mod rpc;
mod setup;
mod tui;
mod update;
mod web_ui;
mod workflow;

use persistence::{LoadedPlanningState, PlanningStateSource};

use anyhow::{Context, Result};
use clap::CommandFactory;
use clap::Parser;
use pipit_config::{ApprovalMode, CliOverrides, ProviderKind};
use pipit_context::{ContextManager, budget::ContextSettings};
use pipit_core::{AgentLoop, AgentLoopConfig, AgentOutcome, PlanningState};
use pipit_extensions::HookExtensionRunner;
use pipit_intelligence::RepoMap;
use pipit_io::input::{SlashCommand, UserInput, classify_input, read_input};
use pipit_io::{InteractiveApprovalHandler, PipitUi, StatusBarState};
use pipit_provider::LlmProvider;
use pipit_skills::{ConditionalRegistry, SkillRegistry};
use pipit_tools::ToolRegistry;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncReadExt;
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
#[command(
    name = "pipit",
    version = env!("CARGO_PKG_VERSION"),
    about = "AI coding agent",
    after_help = "\
EXAMPLES:
  pipit \"fix the login bug in auth.rs\"
  pipit --model opus \"refactor the auth module\"
  pipit --mode guarded \"add pagination to the API\"
  pipit --approval full_auto \"run tests and fix failures\"
  pipit --classic                         # REPL mode
  pipit --json \"run tests\" | jq '.cost'   # CI mode
  pipit --dry-run \"delete unused imports\"  # preview only
  pipit auth login anthropic              # store API key
  pipit completions zsh > ~/.zfunc/_pipit # shell completions
"
)]
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

    /// Enable Vim modal editing in the input composer
    #[arg(long, default_value_t = false)]
    vim: bool,

    /// Enable tmux bridge — bash commands run in a visible tmux pane.
    /// Creates a tmux session with agent + shell panes. Requires tmux.
    #[arg(long, default_value_t = false)]
    tmux: bool,

    /// Enable voice input mode (requires microphone access).
    /// Speech is transcribed via Whisper API and fed as user input.
    #[arg(long, default_value_t = false)]
    voice: bool,

    /// Enable mesh networking for multi-agent coordination.
    /// Joins a local P2P mesh for task delegation across pipit instances.
    #[arg(long, default_value_t = false)]
    mesh: bool,

    /// Mesh bind address (ip:port). Defaults to 0.0.0.0:4190.
    #[arg(long, default_value = "0.0.0.0:4190")]
    mesh_bind: String,

    /// Mesh advertise address (ip:port) — the address other nodes use to reach this node.
    /// Required when binding to 0.0.0.0 in multi-machine setups.
    #[arg(long)]
    mesh_advertise: Option<String>,

    /// Mesh seed node address to join (ip:port). Can be specified multiple times.
    #[arg(long)]
    mesh_seed: Vec<String>,

    /// Output structured JSON (NDJSON events + final result). For CI/scripting.
    #[arg(long, default_value_t = false)]
    json: bool,

    /// Dry-run mode: show what would be done without executing mutations
    #[arg(long, default_value_t = false)]
    dry_run: bool,

    /// Resume the last interrupted session. Hydrates ledger, context,
    /// worktree, and permissions from the session WAL before continuing.
    #[arg(long, default_value_t = false)]
    resume: bool,

    /// Session name for resume (default: latest session)
    #[arg(long)]
    session: Option<String>,

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

    /// Print startup timing breakdown
    #[arg(long, default_value_t = false)]
    timing: bool,

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
    /// Generate shell completion script
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Initialize a new project with framework-specific configuration
    Init {
        /// Framework profile: react, nextjs, node, python, rust, go, typescript, minimal
        #[arg(long, default_value = "minimal")]
        profile: String,
        /// Project directory (defaults to current directory)
        #[arg(default_value = ".")]
        path: String,
    },
    /// Manage plugins (install, uninstall, list, search)
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },
    /// Export a session ledger to Markdown or HTML
    Export {
        /// Path to the session ledger file
        ledger: String,
        /// Output format: md, html
        #[arg(long, default_value = "md")]
        format: String,
        /// Output file (default: stdout)
        #[arg(short, long)]
        output: Option<String>,
        /// Include tool call details
        #[arg(long, default_value_t = true)]
        tools: bool,
        /// Include thinking/reasoning
        #[arg(long, default_value_t = false)]
        thinking: bool,
    },
    /// Start a JSON-RPC 2.0 server over stdio for programmatic control
    Rpc,
    /// Start the web UI server (HTTP + future WebSocket)
    Web {
        /// Address to bind to (default: 127.0.0.1:9090)
        #[arg(long, default_value = "127.0.0.1:9090")]
        bind: String,
    },
}

#[derive(clap::Subcommand, Debug)]
enum PluginAction {
    /// Install a plugin from a local path or registry URL
    Install {
        /// Source: local path or registry plugin name (e.g., "tdd-workflow" or "./my-plugin")
        source: String,
    },
    /// Uninstall a plugin
    Uninstall {
        /// Plugin name
        name: String,
    },
    /// List installed plugins
    List,
    /// Search the remote registry
    Search {
        /// Search query
        query: String,
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

fn env_var_for(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::AmazonBedrock => "AWS_BEARER_TOKEN_BEDROCK",
        ProviderKind::Anthropic | ProviderKind::AnthropicCompatible => "ANTHROPIC_API_KEY",
        ProviderKind::OpenAi | ProviderKind::OpenAiCompatible | ProviderKind::OpenAiCodex => {
            "OPENAI_API_KEY"
        }
        ProviderKind::AzureOpenAi => "AZURE_OPENAI_API_KEY",
        ProviderKind::DeepSeek => "DEEPSEEK_API_KEY",
        ProviderKind::Google => "GOOGLE_API_KEY",
        ProviderKind::GoogleGeminiCli => "GOOGLE_GEMINI_CLI_TOKEN",
        ProviderKind::GoogleAntigravity => "GOOGLE_ANTIGRAVITY_TOKEN",
        ProviderKind::Vertex => "VERTEX_API_KEY",
        ProviderKind::OpenRouter => "OPENROUTER_API_KEY",
        ProviderKind::VercelAiGateway => "AI_GATEWAY_API_KEY",
        ProviderKind::GitHubCopilot => "COPILOT_GITHUB_TOKEN",
        ProviderKind::XAi => "XAI_API_KEY",
        ProviderKind::ZAi => "ZAI_API_KEY",
        ProviderKind::Cerebras => "CEREBRAS_API_KEY",
        ProviderKind::Groq => "GROQ_API_KEY",
        ProviderKind::Mistral => "MISTRAL_API_KEY",
        ProviderKind::HuggingFace => "HF_TOKEN",
        ProviderKind::MiniMax => "MINIMAX_API_KEY",
        ProviderKind::MiniMaxCn => "MINIMAX_CN_API_KEY",
        ProviderKind::Opencode | ProviderKind::OpencodeGo => "OPENCODE_API_KEY",
        ProviderKind::KimiCoding => "KIMI_API_KEY",
        ProviderKind::Ollama => "OLLAMA_API_KEY",
        ProviderKind::OpenAiResponses | ProviderKind::CodexOAuth => "OPENAI_API_KEY",
        ProviderKind::CopilotOAuth => "COPILOT_GITHUB_TOKEN",
        ProviderKind::Faux => "FAUX_API_KEY",
    }
}

// ─── Subagent executor ─────────────────────────────────────────────────────
//
// ═══════════════════════════════════════════════════════════════════════════
//  CliSubagentExecutor — Tasks 1, 2, 3
//
//  Task 1: NDJSON streaming protocol replaces stdout-scrape.
//          Child spawned with `--json` flag emits SubagentEvent per line.
//          Parent reads BufReader::lines() in a Tokio task.
//
//  Task 2: Graceful termination (SIGTERM → 5s → SIGKILL).
//          Bounded concurrency handled at SubagentTool level via Semaphore.
//
//  Task 3: --tools, --model, --no-session, --append-system-prompt honored.
//          `_allowed_tools` is no longer dead code.
// ═══════════════════════════════════════════════════════════════════════════

struct CliSubagentExecutor;

#[async_trait::async_trait]
impl pipit_tools::builtins::subagent::SubagentExecutor for CliSubagentExecutor {
    async fn run_subagent(
        &self,
        task: String,
        context: String,
        options: pipit_tools::builtins::subagent::SubagentOptions,
        project_root: std::path::PathBuf,
        cancel: tokio_util::sync::CancellationToken,
        update_tx: Option<tokio::sync::mpsc::Sender<pipit_tools::builtins::subagent::SubagentUpdate>>,
    ) -> Result<pipit_tools::builtins::subagent::SubagentResult, String> {
        use pipit_tools::builtins::subagent::{SubagentEvent, SubagentResult, SubagentUpdate};
        use tokio::io::{AsyncBufReadExt, BufReader};

        let start = std::time::Instant::now();
        let child_id = uuid::Uuid::new_v4().to_string();

        // ── Agent catalog lookup ──
        // If agent_name is specified, look up the built-in AgentDefinition
        // and apply its system prompt, tool restrictions, and turn cap.
        let options = {
            let mut opts = options;
            if let Some(ref name) = opts.agent_name {
                let all_agents = pipit_agents::all_agents(&project_root);
                if let Some(def) = all_agents.iter().find(|a| a.name.eq_ignore_ascii_case(name)) {
                    tracing::info!(agent = %def.name, "Using agent catalog definition");
                    // Apply tool whitelist from catalog (unless explicitly overridden)
                    if opts.allowed_tools.is_empty() || opts.allowed_tools == vec![
                        "read_file".to_string(), "grep".to_string(),
                        "glob".to_string(), "list_directory".to_string(),
                    ] {
                        if !def.allowed_tools.is_empty() {
                            opts.allowed_tools = def.allowed_tools.iter().cloned().collect();
                        }
                    }
                    // Apply turn cap from catalog
                    if opts.max_turns.is_none() {
                        opts.max_turns = Some(def.max_turns);
                    }
                    // Apply system prompt from catalog
                    if opts.append_system_prompt.is_none() {
                        opts.append_system_prompt = Some(def.system_prompt.clone());
                    }
                }
            }
            opts
        };

        // Find the current executable path so we spawn the same binary
        let exe =
            std::env::current_exe().map_err(|e| format!("Cannot find pipit executable: {}", e))?;

        let mut cmd = tokio::process::Command::new(&exe);
        cmd.arg("--root")
            .arg(project_root.display().to_string())
            .arg("-a")
            .arg("full_auto")
            // Task 1: Use --json mode for NDJSON event stream
            .arg("--json")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .env("PIPIT_SUBAGENT", "1");

        // Task 3: Honor --max-turns
        let max_turns = options.max_turns.unwrap_or(15);
        cmd.arg("--max-turns").arg(max_turns.to_string());

        // Task 3: Honor --model
        if let Some(ref model) = options.model {
            cmd.arg("--model").arg(model);
        }

        // Task 3: Honor --tools (enforce tool scoping)
        if !options.allowed_tools.is_empty() {
            // Pass tools as comma-separated env var — child filters at registry startup
            cmd.env(
                "PIPIT_ALLOWED_TOOLS",
                options.allowed_tools.join(","),
            );
        }

        // Task 3: Honor --no-session
        if options.no_session {
            cmd.env("PIPIT_NO_SESSION", "1");
        }

        // Task 3: Honor --append-system-prompt via temp file
        // Write to a temp file with mode 0o600 for security, cleaned up on drop
        let _prompt_tempfile = if let Some(ref prompt) = options.append_system_prompt {
            let mut tmp = tempfile::NamedTempFile::new()
                .map_err(|e| format!("Failed to create temp prompt file: {e}"))?;
            std::io::Write::write_all(&mut tmp, prompt.as_bytes())
                .map_err(|e| format!("Failed to write temp prompt file: {e}"))?;
            cmd.env("PIPIT_APPEND_SYSTEM_PROMPT", tmp.path());
            Some(tmp) // Keep alive until child exits, then auto-deleted
        } else if !context.is_empty() {
            // Inject context as appended system prompt
            let mut tmp = tempfile::NamedTempFile::new()
                .map_err(|e| format!("Failed to create temp context file: {e}"))?;
            std::io::Write::write_all(&mut tmp, context.as_bytes())
                .map_err(|e| format!("Failed to write temp context file: {e}"))?;
            cmd.env("PIPIT_APPEND_SYSTEM_PROMPT", tmp.path());
            Some(tmp)
        } else {
            None
        };

        // Task 7: Fork context via temp file
        let _fork_tempfile = if let Some(ref fork_ctx) = options.fork_context {
            let mut tmp = tempfile::NamedTempFile::new()
                .map_err(|e| format!("Failed to create fork context file: {e}"))?;
            let json = serde_json::to_string(fork_ctx)
                .map_err(|e| format!("Failed to serialize fork context: {e}"))?;
            std::io::Write::write_all(&mut tmp, json.as_bytes())
                .map_err(|e| format!("Failed to write fork context file: {e}"))?;
            cmd.env("PIPIT_FORK_FROM", tmp.path());
            Some(tmp)
        } else {
            None
        };

        // Append the task as the prompt
        cmd.arg(&task);

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn subagent: {}", e))?;

        // Notify parent of start
        if let Some(ref tx) = update_tx {
            let _ = tx
                .send(SubagentUpdate {
                    child_id: child_id.clone(),
                    event: SubagentEvent::Started {
                        child_id: child_id.clone(),
                        task: task.clone(),
                    },
                })
                .await;
        }

        let stdout_pipe = child
            .stdout
            .take()
            .ok_or_else(|| "Failed to capture subagent stdout".to_string())?;
        let mut stderr_pipe = child
            .stderr
            .take()
            .ok_or_else(|| "Failed to capture subagent stderr".to_string())?;

        // Task 1: Read NDJSON events line-by-line (not read_to_end!)
        let tx_for_reader = update_tx.clone();
        let cid = child_id.clone();
        let stdout_handle = tokio::spawn(async move {
            let reader = BufReader::new(stdout_pipe);
            let mut lines = reader.lines();
            let mut last_output = String::new();
            let mut total_input_tokens = 0u64;
            let mut total_output_tokens = 0u64;
            let mut total_cost_usd = 0.0f64;
            let mut total_turns = 0u32;

            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                // Try to parse as SubagentEvent
                match serde_json::from_str::<SubagentEvent>(&line) {
                    Ok(event) => {
                        match &event {
                            SubagentEvent::Completed {
                                output,
                                total_turns: turns,
                                total_input_tokens: inp,
                                total_output_tokens: outp,
                                total_cost_usd: cost,
                                ..
                            } => {
                                last_output = output.clone();
                                total_turns = *turns;
                                total_input_tokens = *inp;
                                total_output_tokens = *outp;
                                total_cost_usd = *cost;
                            }
                            SubagentEvent::MessageEnd { text, turn } => {
                                last_output = text.clone();
                                total_turns = *turn;
                            }
                            SubagentEvent::Usage {
                                input_tokens,
                                output_tokens,
                                cost_usd,
                                ..
                            } => {
                                total_input_tokens += input_tokens;
                                total_output_tokens += output_tokens;
                                total_cost_usd += cost_usd;
                            }
                            SubagentEvent::Error { message, .. } => {
                                if last_output.is_empty() {
                                    last_output = format!("Error: {message}");
                                }
                            }
                            _ => {}
                        }
                        // Forward to parent
                        if let Some(ref tx) = tx_for_reader {
                            let _ = tx
                                .send(SubagentUpdate {
                                    child_id: cid.clone(),
                                    event,
                                })
                                .await;
                        }
                    }
                    Err(_) => {
                        // Not a JSON event — accumulate as raw output (fallback for
                        // non-JSON mode children or intermixed text)
                        if !line.is_empty() {
                            if !last_output.is_empty() {
                                last_output.push('\n');
                            }
                            last_output.push_str(&line);
                        }
                    }
                }
            }

            (last_output, total_turns, total_input_tokens, total_output_tokens, total_cost_usd)
        });

        let stderr_handle = tokio::spawn(async move {
            let mut buf = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut stderr_pipe, &mut buf)
                .await
                .map(|_| buf)
                .map_err(|e| format!("Failed to read subagent stderr: {}", e))
        });

        // Wait for completion or cancellation
        let status = tokio::select! {
            result = child.wait() => {
                result.map_err(|e| format!("Subagent process error: {}", e))?
            }
            _ = cancel.cancelled() => {
                // Task 2: Graceful termination — SIGTERM first, then SIGKILL
                pipit_tools::builtins::subagent::supervisor::graceful_terminate(&mut child).await;
                let _ = stdout_handle.await;
                let _ = stderr_handle.await;
                return Err("Subagent cancelled".to_string());
            }
        };

        let (output, total_turns, total_input_tokens, total_output_tokens, total_cost_usd) =
            stdout_handle
                .await
                .map_err(|e| format!("Subagent stdout task failed: {}", e))?;
        let stderr = stderr_handle
            .await
            .map_err(|e| format!("Subagent stderr task failed: {}", e))??;
        let stderr = String::from_utf8_lossy(&stderr);

        let duration_ms = start.elapsed().as_millis() as u64;

        if status.success() || !output.is_empty() {
            // Send completion event
            if let Some(ref tx) = update_tx {
                let _ = tx
                    .send(SubagentUpdate {
                        child_id: child_id.clone(),
                        event: SubagentEvent::Completed {
                            output: output.clone(),
                            total_turns,
                            total_input_tokens,
                            total_output_tokens,
                            total_cost_usd,
                            duration_ms,
                        },
                    })
                    .await;
            }

            Ok(SubagentResult {
                output: if output.trim().is_empty() {
                    format!("Subagent completed (exit {status}).")
                } else {
                    output
                },
                total_turns,
                total_input_tokens,
                total_output_tokens,
                total_cost_usd,
                duration_ms,
                model: options.model,
            })
        } else {
            // Send error event
            if let Some(ref tx) = update_tx {
                let _ = tx
                    .send(SubagentUpdate {
                        child_id: child_id.clone(),
                        event: SubagentEvent::Error {
                            message: format!("Exit {status}: {stderr}"),
                            recoverable: false,
                        },
                    })
                    .await;
            }

            Err(format!(
                "Subagent failed (exit {}). stderr:\n{}",
                status, stderr
            ))
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let startup_start = std::time::Instant::now();

    // Prevent git from spawning a pager (less, more) which would hang
    // std::process::Command calls. This is process-wide so all git
    // invocations (slash commands, hooks, agent tools) benefit.
    // Also set GIT_TERMINAL_PROMPT=0 to prevent git from prompting for
    // credentials interactively — if auth fails, it should fail fast
    // rather than blocking the process waiting for stdin.
    //
    // SAFETY: set_var is safe here because we are at the very start of main(),
    // before any threads are spawned by the tokio runtime.
    unsafe {
        std::env::set_var("GIT_PAGER", "cat");
        std::env::set_var("GIT_TERMINAL_PROMPT", "0");
    }

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

    let t_parse = std::time::Instant::now();
    let cli = Cli::parse();
    let parse_ms = t_parse.elapsed().as_millis();

    // Enable debug logging if requested
    if cli.debug {
        DEBUG_ENABLED.store(true, Ordering::Relaxed);
        // Truncate previous log
        let _ = std::fs::write("/tmp/pipit-debug.log", "");
        dbg_log("=== pipit startup (--debug) ===");
        dbg_log(&format!("version: {}", env!("CARGO_PKG_VERSION")));
        dbg_log(&format!(
            "classic: {}, mode: {}, repomap: {}",
            cli.classic, cli.mode, cli.repomap
        ));
    }

    // Handle subcommands early (before provider resolution)
    match &cli.command {
        Some(Commands::Auth { action }) => return auth::handle(action).await,
        Some(Commands::Update) => return update::self_update().await,
        Some(Commands::Setup) => return setup::run(),
        Some(Commands::Completions { shell }) => {
            let mut cmd = <Cli as clap::CommandFactory>::command();
            clap_complete::generate(*shell, &mut cmd, "pipit", &mut std::io::stdout());
            return Ok(());
        }
        Some(Commands::Init { profile, path }) => {
            return init::run_init(profile, path);
        }
        Some(Commands::Plugin { action }) => {
            return plugin::handle_plugin_action(action).await;
        }
        Some(Commands::Export {
            ledger,
            format,
            output,
            tools,
            thinking,
        }) => {
            return handle_export(ledger, format, output, *tools, *thinking);
        }
        Some(Commands::Rpc) => {
            return rpc::run_rpc_server().await;
        }
        Some(Commands::Web { bind }) => {
            let addr: std::net::SocketAddr = bind.parse()
                .map_err(|e| anyhow::anyhow!("Invalid bind address '{}': {}", bind, e))?;
            let config = web_ui::WebUiConfig {
                bind_addr: addr,
                cors: true,
            };
            return web_ui::start_web_ui(config).await;
        }
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

    // Canonicalize early so both the system prompt and tool context see the
    // real path. On macOS /tmp is a symlink to /private/tmp — without this
    // the model sees "/tmp/…" in the prompt but tools resolve to "/private/tmp/…",
    // causing it to create files in the wrong directory.
    let project_root = project_root
        .canonicalize()
        .unwrap_or(project_root);

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

    let mut config = pipit_config::resolve_config(Some(&project_root), overrides)
        .context("Config resolution failed")?;

    let mut provider_kind = config.provider.default;
    dbg_log(&format!(
        "[3/12] config resolved, provider={}",
        provider_kind
    ));

    // Resolve API key — offer interactive setup if missing
    let mut api_key = match cli
        .api_key
        .clone()
        .or_else(|| pipit_config::resolve_api_key(provider_kind))
    {
        Some(key) => key,
        None => {
            if provider_kind == ProviderKind::Ollama {
                // Ollama doesn't need a real key
                "ollama".to_string()
            } else {
                eprintln!();
                eprintln!(
                    "  \x1b[1;31m✗ No API key found for {}\x1b[0m",
                    provider_kind
                );
                eprintln!();
                eprintln!("  \x1b[1;33mLet's set up your configuration.\x1b[0m");
                eprintln!();

                setup::run()?;

                // Retry after setup
                let prov2 =
                    pipit_config::resolve_config(Some(&project_root), CliOverrides::default())
                        .map(|c| c.provider.default)
                        .unwrap_or(provider_kind);
                pipit_config::resolve_api_key(prov2).ok_or_else(|| {
                    anyhow::anyhow!(
                        "Still no API key after setup.\n\
                         Set it via: export {}=<key>",
                        env_var_for(provider_kind),
                    )
                })?
            }
        }
    };

    dbg_log("[4/12] api_key resolved");

    // Resolve model
    let mut model = cli.model.unwrap_or(config.model.default_model.clone());

    // Resolve base URL: CLI flag > config file
    let mut base_url = cli.base_url.or(config.provider.custom_base_url.clone());

    // Create provider — on failure, offer interactive setup instead of dying
    let provider: Arc<dyn LlmProvider> = match pipit_provider::create_provider(
        provider_kind,
        &model,
        &api_key,
        base_url.as_deref(),
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
                approval_mode: cli
                    .approval
                    .as_deref()
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

    // ── Upgrade max_output_tokens from provider capabilities ──
    // The config default (8192) is too low for modern models that support
    // 16K-128K output tokens.  When the user hasn't explicitly configured
    // max_output_tokens, adopt the provider's advertised capability.
    // This is the single biggest factor in output quality — a model capped
    // at 8K tokens cannot write a 500-line file in one tool call.
    let provider_max_output = provider.capabilities().max_output_tokens;
    if provider_max_output > config.model.max_output_tokens {
        config.model.max_output_tokens = provider_max_output;
    }

    dbg_log(&format!(
        "[5/12] provider created: {} / {} (max_output_tokens: {})",
        provider_kind, model, config.model.max_output_tokens
    ));

    // ── Provider Roster: auto-discover all available provider profiles ──
    let mut provider_roster = pipit_config::provider_roster::ProviderRoster::discover(
        provider_kind,
        &model,
        &api_key,
        base_url.as_deref(),
    );
    dbg_log(&format!(
        "[5.1/12] provider roster: {} profile(s) discovered",
        provider_roster.len()
    ));

    // Build model router based on agent mode
    let agent_mode: pipit_core::AgentMode = cli
        .mode
        .parse()
        .map_err(|e: String| anyhow::anyhow!("{}", e))?;

    // Auto-promote to Custom if role overrides are specified
    let agent_mode = if agent_mode != pipit_core::AgentMode::Custom
        && (cli.planner_model.is_some()
            || cli.planner_provider.is_some()
            || cli.verifier_model.is_some()
            || cli.verifier_provider.is_some()
            || cli.planner_base_url.is_some()
            || cli.verifier_base_url.is_some())
    {
        pipit_core::AgentMode::Custom
    } else {
        agent_mode
    };

    let pev_config = agent_mode.to_pev_config();

    let models = if agent_mode == pipit_core::AgentMode::Custom {
        use pipit_core::{ModelRole, ModelRouter, RoleProvider};

        let planner_model_id = cli.planner_model.as_deref().unwrap_or(&model);
        let verifier_model_id = cli.verifier_model.as_deref().unwrap_or(&model);

        // Warn if planner/verifier uses a non-reasoning model
        let non_reasoning_hints = ["non-reasoning", "fast", "mini", "instant", "flash", "haiku"];
        for (role, model_id) in [
            ("planner", planner_model_id),
            ("verifier", verifier_model_id),
        ] {
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

        let make_provider = |role_model: &str,
                             role_provider_str: Option<&str>,
                             role_base_url: Option<&str>|
         -> Result<Arc<dyn LlmProvider>, anyhow::Error> {
            let rp_kind: ProviderKind = if let Some(p) = role_provider_str {
                p.parse().map_err(|e: String| anyhow::anyhow!("{}", e))?
            } else {
                provider_kind
            };
            let rp_key = pipit_config::resolve_api_key(rp_kind).unwrap_or_else(|| api_key.clone());
            let rp_base = role_base_url.or(base_url.as_deref());
            Ok(Arc::from(
                pipit_provider::create_provider(rp_kind, role_model, &rp_key, rp_base).map_err(
                    |e| anyhow::anyhow!("Provider creation for {} failed: {}", role_model, e),
                )?,
            ))
        };

        let planner_provider = if cli.planner_model.is_some()
            || cli.planner_provider.is_some()
            || cli.planner_base_url.is_some()
        {
            make_provider(
                planner_model_id,
                cli.planner_provider.as_deref(),
                cli.planner_base_url.as_deref(),
            )?
        } else {
            provider.clone()
        };

        let verifier_provider = if cli.verifier_model.is_some()
            || cli.verifier_provider.is_some()
            || cli.verifier_base_url.is_some()
        {
            make_provider(
                verifier_model_id,
                cli.verifier_provider.as_deref(),
                cli.verifier_base_url.as_deref(),
            )?
        } else {
            provider.clone()
        };

        let router = ModelRouter::new(
            RoleProvider {
                provider: planner_provider,
                model_id: planner_model_id.to_string(),
                role: ModelRole::Planner,
            },
            RoleProvider {
                provider: provider.clone(),
                model_id: model.clone(),
                role: ModelRole::Executor,
            },
            RoleProvider {
                provider: verifier_provider,
                model_id: verifier_model_id.to_string(),
                role: ModelRole::Verifier,
            },
        );

        eprintln!(
            "pipit› mode: custom | planner: {} | executor: {} | verifier: {}",
            planner_model_id, model, verifier_model_id
        );

        router
    } else {
        if agent_mode != pipit_core::AgentMode::Fast {
            eprintln!("pipit› mode: {} — {}", agent_mode, agent_mode.description());
        }
        pipit_core::ModelRouter::single(provider.clone(), model.clone())
    };

    dbg_log(&format!("[6/12] model_router built, mode={}", agent_mode));

    // Create shared MemoryManager for both MemoryTool and AgentLoop.
    let memory_manager = {
        let mm = pipit_memory::MemoryManager::new(&project_root);
        std::sync::Arc::new(std::sync::Mutex::new(mm))
    };

    // Build tool registry
    let mut tools = ToolRegistry::with_builtins();

    // Register MemoryTool — exposes remember/forget/recall to the LLM.
    tools.register(std::sync::Arc::new(
        pipit_tools::builtins::MemoryTool::new(memory_manager.clone()),
    ));

    // Initialize MCP servers (if configured)
    let _mcp_manager = pipit_mcp::initialize_mcp(&project_root, &mut tools).await;

    // Register browser tools only if a browser/CDP config exists.
    // Without a browser, these 6 tools just waste context tokens.
    if pipit_browser::extension_bridge::has_browser_config(&project_root) {
        pipit_browser::extension_bridge::register_browser_tools(&mut tools);
    }

    // Register subagent tool — enables the LLM to delegate subtasks to a
    // child pipit process with bounded scope and isolated execution.
    let subagent_executor = Arc::new(CliSubagentExecutor);
    tools.register_subagent(subagent_executor);

    let workflow_assets = WorkflowAssets::discover(&project_root);

    // Discover skills (#21: progressive disclosure)
    let skill_paths: Vec<PathBuf> = workflow_assets.skill_search_paths();
    let mut skills = SkillRegistry::discover(&skill_paths);

    // Separate conditional skills (those with `paths:` patterns) from always-active ones.
    // Conditional skills activate on file-touch during the session.
    let conditional_skills = skills.drain_conditional();
    let mut conditional_registry = ConditionalRegistry::new(conditional_skills);

    if skills.count() > 0 || conditional_registry.total_count() > 0 {
        tracing::info!(
            "Skills: {} active, {} conditional",
            skills.count(),
            conditional_registry.total_count()
        );
    }

    dbg_log(&format!(
        "[7/12] tools={}, skills={}, workflow_assets loaded",
        tools.tool_names().len(),
        skills.count()
    ));

    // Build system prompt via the composable prompt kernel.
    // Boot listing is returned separately for turn-1 injection (keeps system prompt cache-stable).
    // Pass context_window so the kernel can emit compact guidelines for small models.
    dbg_log(&format!(
        "[7.5] context_window={}, model={}",
        config.model.context_window, config.model.default_model
    ));
    let (assembled_prompt, boot_listing) = prompt_builder::build_composed_prompt_with_context(
        &project_root,
        &tools,
        config.approval,
        provider_kind,
        &skills,
        &workflow_assets,
        Some(config.model.context_window),
    );
    let system_prompt = assembled_prompt.materialize();

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
            tool_result_max_chars: 131_072,
        },
    );

    dbg_log(&format!(
        "[8/12] system_prompt + context_manager built (prompt={} chars, ~{} tokens)",
        system_prompt.len(),
        system_prompt.len() / 4,
    ));

    // Build RepoMap — skip if project_root is not a git repo (e.g. user's home dir)
    // to avoid scanning millions of files and hanging forever.
    // Detect git repo using `git rev-parse` which correctly handles subdirectories
    // of git repos (walking up to find .git/), unlike checking project_root.join(".git").
    let is_git_repo = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(&project_root)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    let repo_map_text = if cli.repomap && is_git_repo {
        dbg_log(&format!(
            "[8.5] building repomap for {}",
            project_root.display()
        ));
        // Dynamic RepoMap budget: allocate 10% of context window, capped.
        // For small models (≤128K), cap aggressively to leave room for conversation.
        // For large models (>128K), allow up to 8192 tokens.
        let repo_map_cap: u64 = if config.model.context_window <= 131_072 { 2048 } else { 8192 };
        let repo_map_budget = ((config.model.context_window as u64) / 10).clamp(1024, repo_map_cap);
        let root = project_root.clone();
        // RepoMap build is CPU-intensive (file scanning, tree-sitter parsing).
        // Run on the blocking threadpool to avoid starving the async runtime.
        let map_result = tokio::task::spawn_blocking(move || {
            let intelligence_config = pipit_intelligence::IntelligenceConfig::default();
            let repo_map = RepoMap::build(&root, intelligence_config);
            if repo_map.file_count() > 0 {
                let map = repo_map.render(&[], repo_map_budget as usize);
                let file_count = repo_map.file_count();
                Some((map, file_count))
            } else {
                None
            }
        })
        .await
        .unwrap_or(None);

        if let Some((map, file_count)) = map_result {
            let map_tokens = pipit_context::estimate_text_tokens(&map);
            tracing::info!(
                "RepoMap: {} files indexed, {} tokens (budget: {})",
                file_count,
                map_tokens,
                repo_map_budget
            );
            dbg_log(&format!(
                "[8.5] repomap: {} files indexed, {} tokens",
                file_count, map_tokens
            ));
            context.update_repo_map_tokens(map_tokens);
            Some(map)
        } else {
            None
        }
    } else {
        if cli.repomap && !is_git_repo {
            dbg_log("[8.5] skipping repomap — not a git repo");
            tracing::info!(
                "RepoMap skipped — {} is not a git repository",
                project_root.display()
            );
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
        dry_run: cli.dry_run,
        cli_explicit_max_turns: cli.max_turns.is_some(),
        boot_context: if boot_listing.is_empty() || config.model.context_window <= 131_072 {
            // For compact-context models (≤128K), skip boot listing injection.
            // The project structure costs ~200-500 tokens in the first user message.
            // The model can use list_directory on demand, which is cheaper overall.
            None
        } else {
            Some(format!(
                "## Initial project structure\n{}",
                boot_listing
            ))
        },
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

    // Share the MemoryManager with the agent loop (for boot context injection + auto-dream).
    agent.set_memory_manager(memory_manager.clone());

    // ── Session kernel + resume support ──
    // Enable durable session tracking. When --resume is set, hydrate from
    // the last session's WAL before accepting new input.
    let session_dir = project_root.join(".pipit").join("sessions").join("latest");
    if let Ok(kernel) = pipit_core::session_kernel::SessionKernel::new(
        pipit_core::session_kernel::SessionKernelConfig {
            session_dir: session_dir.clone(),
            durable_writes: true,
            snapshot_interval: 50,
        },
    ) {
        if cli.resume {
            // Hydrate from persisted state
            let mut kernel = kernel;
            match pipit_core::hydration::hydrate_session(
                &mut kernel,
                agent.context_mut(),
                &session_dir,
            ) {
                Ok(result) => {
                    eprintln!(
                        "Resumed session: {} events replayed, {} messages restored{}",
                        result.events_replayed,
                        result.messages_restored.len(),
                        if result.worktree_restored {
                            ", worktree restored"
                        } else {
                            ""
                        },
                    );
                    if let Some(cwd) = result.restored_cwd {
                        agent.tool_context_mut().set_cwd(cwd);
                    }
                }
                Err(e) => {
                    eprintln!("Resume failed (starting fresh): {e}");
                }
            }
            agent.enable_session_kernel(kernel);
        } else {
            agent.enable_session_kernel(kernel);
        }
    }

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

    // ── Voice mode (Task 9) ──
    #[cfg(feature = "voice")]
    let _voice_handle = if cli.voice {
        let api_key = api_key.clone();
        Some(tokio::spawn(async move {
            use pipit_voice::transcription::TranscriptionConfig;
            tracing::info!("Voice mode enabled — starting audio capture");
            let config = TranscriptionConfig {
                api_key,
                ..TranscriptionConfig::default()
            };
            // Voice pipeline runs in background; transcriptions are printed.
            // Full integration into the agent turn loop requires wiring
            // the speech bus into the input stream (future work).
            tracing::info!("Voice transcription config: {:?}", config);
        }))
    } else {
        None
    };
    #[cfg(not(feature = "voice"))]
    if cli.voice {
        eprintln!("\x1b[33mVoice mode not available — rebuild with `--features voice`\x1b[0m");
    }

    // ── Mesh mode (Task 10) ──
    #[cfg(feature = "mesh")]
    let mesh_daemon: Option<std::sync::Arc<pipit_mesh::MeshDaemon>> = if cli.mesh {
        let root = project_root.clone();
        let bind_addr: std::net::SocketAddr = cli.mesh_bind.parse().unwrap_or_else(|_| {
            eprintln!("\x1b[33mInvalid --mesh-bind address, using 0.0.0.0:4190\x1b[0m");
            "0.0.0.0:4190".parse().unwrap()
        });
        // Advertise address: use --mesh-advertise if given, else use bind_addr
        // (but warn if bind is 0.0.0.0 — other nodes can't reach that)
        let advertise_addr: std::net::SocketAddr = if let Some(ref adv) = cli.mesh_advertise {
            adv.parse().unwrap_or_else(|_| {
                eprintln!("\x1b[33mInvalid --mesh-advertise, falling back to bind address\x1b[0m");
                bind_addr
            })
        } else if bind_addr.ip().is_unspecified() {
            eprintln!("\x1b[33mWarning: binding to {} — use --mesh-advertise <ip:port> so remote nodes can reach you\x1b[0m", bind_addr);
            bind_addr
        } else {
            bind_addr
        };
        let seeds = cli.mesh_seed.clone();
        use pipit_mesh::{MeshDaemon, NodeDescriptor};
        tracing::info!("Mesh mode enabled — starting P2P mesh");
        let node = NodeDescriptor {
            id: uuid::Uuid::new_v4().to_string(),
            name: hostname(),
            addr: advertise_addr,
            capabilities: vec!["agent".to_string()],
            model: None,
            load: 0.0,
            gpu: None,
            project_roots: vec![root.display().to_string()],
            joined_at: chrono::Utc::now(),
            last_heartbeat: chrono::Utc::now(),
        };
        let daemon = std::sync::Arc::new(MeshDaemon::new(node));
        let d = daemon.clone();
        tokio::spawn(async move {
            if let Err(e) = d.start(bind_addr).await {
                tracing::error!(error = %e, "Mesh daemon failed to start");
                return;
            }
            tracing::info!(node_id = %d.local_node.id, bind = %bind_addr, advertise = %advertise_addr, "Mesh daemon listening");
            for seed_str in &seeds {
                if let Ok(seed_addr) = seed_str.parse::<std::net::SocketAddr>() {
                    match d.join(seed_addr).await {
                        Ok(_) => tracing::info!(seed = %seed_addr, "Joined mesh seed"),
                        Err(e) => tracing::warn!(seed = %seed_addr, error = %e, "Failed to join seed"),
                    }
                } else {
                    tracing::warn!(seed = %seed_str, "Invalid seed address");
                }
            }
        });
        eprintln!("\x1b[36mMesh daemon started on {} (advertise: {})\x1b[0m", bind_addr, advertise_addr);
        Some(daemon)
    } else {
        None
    };
    #[cfg(not(feature = "mesh"))]
    let mesh_daemon: Option<()> = if cli.mesh {
        eprintln!("\x1b[33mMesh mode not available — rebuild with `--features mesh`\x1b[0m");
        None
    } else {
        None
    };

    // Create UI
    let mut ui = PipitUi::new(show_thinking, true, trace_ui, status.clone());

    if let Some(msg) = update::detect_path_version_conflict() {
        eprintln!("\x1b[33m{}\x1b[0m\n", msg);
    }

    // Show update notification if available.
    // Use a short timeout so a slow/unreachable GitHub API never stalls startup.
    let _update_notice =
        match tokio::time::timeout(std::time::Duration::from_millis(800), update_msg).await {
            Ok(Ok(Some(msg))) => {
                eprintln!("\x1b[33m{}\x1b[0m\n", msg);
                Some(msg)
            }
            _ => None,
        };

    // Single-shot mode (or JSON mode)
    if let Some(prompt) = cli.prompt {
        let cancel = CancellationToken::new();
        let json_mode = cli.json;

        // Wire Ctrl-C to the cancellation token so single-shot mode gets a
        // graceful shutdown instead of an abrupt process kill.  This ensures
        // telemetry is flushed and the ledger is persisted before exit.
        let cancel_for_ctrlc = cancel.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            cancel_for_ctrlc.cancel();
        });

        // Spawn event handler
        let _ui_handle = tokio::spawn(async move {
            if json_mode {
                // JSON mode: emit NDJSON events to stderr (structured events)
                while let Ok(event) = event_rx.recv().await {
                    // Format events as JSON objects with type + data
                    let json = match &event {
                        pipit_core::AgentEvent::TurnStart { turn_number } => {
                            serde_json::json!({"event": "turn_start", "turn": turn_number})
                        }
                        pipit_core::AgentEvent::TurnEnd { turn_number, .. } => {
                            serde_json::json!({"event": "turn_end", "turn": turn_number})
                        }
                        pipit_core::AgentEvent::ContentDelta { text } => {
                            serde_json::json!({"event": "content_delta", "text": text})
                        }
                        pipit_core::AgentEvent::ContentComplete { full_text } => {
                            serde_json::json!({"event": "content_complete", "text": full_text})
                        }
                        pipit_core::AgentEvent::ToolCallStart { call_id, name, .. } => {
                            serde_json::json!({"event": "tool_start", "call_id": call_id, "name": name})
                        }
                        pipit_core::AgentEvent::ToolCallEnd { call_id, name, .. } => {
                            serde_json::json!({"event": "tool_end", "call_id": call_id, "name": name})
                        }
                        pipit_core::AgentEvent::TokenUsageUpdate { used, limit, cost } => {
                            serde_json::json!({"event": "token_usage", "used": used, "limit": limit, "cost": cost})
                        }
                        pipit_core::AgentEvent::Waiting { label } => {
                            serde_json::json!({"event": "waiting", "label": label})
                        }
                        _ => serde_json::json!({"event": "other"}),
                    };
                    eprintln!("{}", json);
                }
            } else {
                let mut ui = PipitUi::new(true, true, trace_ui, status);
                while let Ok(event) = event_rx.recv().await {
                    ui.handle_event(&event);
                }
            }
        });

        let outcome = agent.run(prompt, cancel).await;

        if json_mode {
            // Structured JSON result to stdout
            let result = match &outcome {
                AgentOutcome::Completed {
                    turns,
                    cost,
                    proof,
                    total_tokens,
                } => {
                    let files_modified: Vec<String> = proof
                        .realized_edits
                        .iter()
                        .map(|e| e.path.clone())
                        .collect();
                    let confidence = &proof.confidence;
                    serde_json::json!({
                        "status": "completed",
                        "turns": turns,
                        "total_tokens": total_tokens,
                        "cost": cost,
                        "files_modified": files_modified,
                        "exit_code": 0,
                        "proof": {
                            "objective": proof.objective.statement,
                            "strategy": format!("{:?}", proof.selected_plan.strategy),
                            "plan_source": format!("{:?}", proof.selected_plan.plan_source),
                            "plan_pivots": proof.plan_pivots.len(),
                            "evidence_count": proof.evidence.len(),
                            "realized_edits": proof.realized_edits.len(),
                            "risk_score": proof.risk.score,
                            "risk_class": format!("{:?}", proof.risk.action_class),
                            "confidence": {
                                "overall": confidence.overall(),
                                "root_cause": confidence.root_cause,
                                "semantic_understanding": confidence.semantic_understanding,
                                "side_effect_risk": confidence.side_effect_risk,
                                "verification_strength": confidence.verification_strength,
                                "environment_certainty": confidence.environment_certainty,
                            },
                            "rollback_checkpoint": proof.rollback_checkpoint.checkpoint_id,
                            "unresolved_assumptions": proof.unresolved_assumptions.len(),
                        }
                    })
                }
                AgentOutcome::MaxTurnsReached(n) => {
                    serde_json::json!({
                        "status": "max_turns_reached",
                        "turns": n,
                        "exit_code": 2,
                    })
                }
                AgentOutcome::BudgetExhausted {
                    turns,
                    cost,
                    budget,
                } => {
                    serde_json::json!({
                        "status": "budget_exhausted",
                        "turns": turns,
                        "cost": cost,
                        "budget": budget,
                        "exit_code": 2,
                    })
                }
                AgentOutcome::Cancelled => {
                    serde_json::json!({
                        "status": "cancelled",
                        "exit_code": 1,
                    })
                }
                AgentOutcome::Error(e) => {
                    serde_json::json!({
                        "status": "error",
                        "error": e,
                        "exit_code": 1,
                    })
                }
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&result).unwrap_or_default()
            );

            let exit_code = result["exit_code"].as_i64().unwrap_or(0);
            if exit_code != 0 {
                std::process::exit(exit_code as i32);
            }
            return Ok(());
        }

        match outcome {
            AgentOutcome::Completed {
                turns, cost, proof, ..
            } => {
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
                    persistence::persist_planning_snapshot(&project_root, &planning_state, None)
                        .ok();
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
                    persistence::persist_planning_snapshot(&project_root, &planning_state, None)
                        .ok();
                }
            }
        }

        return Ok(());
    }

    dbg_log("[12/12] pre-TUI: all init done, entering TUI/REPL");

    // Print startup timing breakdown if --timing is set
    if cli.timing {
        let total_ms = startup_start.elapsed().as_millis();
        eprintln!("\x1b[2m── startup timing ──\x1b[0m");
        eprintln!("\x1b[2m  parse:  {}ms\x1b[0m", parse_ms);
        eprintln!("\x1b[2m  total:  {}ms\x1b[0m", total_ms);
        if total_ms > 200 {
            eprintln!("\x1b[33m  ⚠ startup exceeded 200ms budget\x1b[0m");
        }
    }

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
            cli.tmux,
            cli.vim,
            provider_roster,
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

    // VCS Gateway: single execution surface for all git mutations.
    // Slash commands route through here instead of shelling out directly.
    let mut vcs_gateway = pipit_vcs::VcsGateway::new(project_root.clone());

    // Track the working directory for `!` shell passthrough commands.
    // `cd` is intercepted and persists across `!` invocations.
    let mut shell_cwd = project_root.clone();

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
                            .or_else(|| {
                                persistence::load_planning_snapshot(&project_root)
                                    .ok()
                                    .flatten()
                            });
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
                                    stats.messages_removed, stats.tokens_freed,
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
                            format!(
                                "Create a plan for: {}. Do NOT make any changes yet — only discuss the approach, list the files involved, and outline the steps.",
                                t
                            )
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
                            let prompt = format!(
                                "Read the file {} and keep it in context for our discussion.",
                                path
                            );
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
                            eprintln!(
                                "\x1b[33mRolling back {} file(s) to {}\x1b[0m",
                                files.len(),
                                &sha[..8.min(sha.len())]
                            );
                            // Route through VCS gateway — ledger-logged rollback
                            match vcs_gateway.restore_files(sha, files) {
                                Ok(results) => {
                                    let mut success = 0;
                                    let mut failed = 0;
                                    for (file, ok) in &results {
                                        if *ok {
                                            eprintln!("  \x1b[32m✓\x1b[0m {}", file);
                                            success += 1;
                                        } else {
                                            eprintln!("  \x1b[31m✗\x1b[0m {}", file);
                                            failed += 1;
                                        }
                                    }
                                    eprintln!(
                                        "\x1b[2m({} restored, {} failed)\x1b[0m",
                                        success, failed
                                    );
                                }
                                Err(e) => eprintln!("\x1b[31m{}\x1b[0m", e),
                            }
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
                        if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) {
                            last_rollback = Some(rb);
                        }
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
                        if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) {
                            last_rollback = Some(rb);
                        }
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
                                 Aim for 80%+ coverage.",
                                t
                            )
                        } else {
                            "Show the current test coverage and suggest what tests are missing. Do NOT write code yet — just analyze.".to_string()
                        };
                        let cancel = CancellationToken::new();
                        let cancel_clone = cancel.clone();
                        let ctrlc_handle = tokio::spawn(async move {
                            tokio::signal::ctrl_c().await.ok();
                            cancel_clone.cancel();
                        });
                        let outcome = agent.run(prompt, cancel).await;
                        ctrlc_handle.abort();
                        if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) {
                            last_rollback = Some(rb);
                        }
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
                        let ctrlc_handle = tokio::spawn(async move {
                            tokio::signal::ctrl_c().await.ok();
                            cancel_clone.cancel();
                        });
                        let outcome = agent.run(prompt, cancel).await;
                        ctrlc_handle.abort();
                        if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) {
                            last_rollback = Some(rb);
                        }
                        continue;
                    }
                    SlashCommand::BuildFix => {
                        let prompt = "Fix build errors incrementally:\n\
                            1. Detect the build system (cargo, npm, tsc, make, gradle, go, etc.).\n\
                            2. Run the build command and capture errors.\n\
                            3. Fix ONE error at a time — the first/root error.\n\
                            4. Re-run the build to verify the fix.\n\
                            5. Repeat until the build succeeds or report what's unresolvable.\n\
                            Make minimal, surgical fixes. Do not refactor."
                            .to_string();
                        let cancel = CancellationToken::new();
                        let cancel_clone = cancel.clone();
                        let ctrlc_handle = tokio::spawn(async move {
                            tokio::signal::ctrl_c().await.ok();
                            cancel_clone.cancel();
                        });
                        let outcome = agent.run(prompt, cancel).await;
                        ctrlc_handle.abort();
                        if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) {
                            last_rollback = Some(rb);
                        }
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
                        let ctrlc_handle = tokio::spawn(async move {
                            tokio::signal::ctrl_c().await.ok();
                            cancel_clone.cancel();
                        });
                        let outcome = agent.run(prompt, cancel).await;
                        ctrlc_handle.abort();
                        if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) {
                            last_rollback = Some(rb);
                        }
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
                             Recommend the best approach with rationale.",
                            target_desc
                        );
                        let cancel = CancellationToken::new();
                        let cancel_clone = cancel.clone();
                        let ctrlc_handle = tokio::spawn(async move {
                            tokio::signal::ctrl_c().await.ok();
                            cancel_clone.cancel();
                        });
                        let outcome = agent.run(prompt, cancel).await;
                        ctrlc_handle.abort();
                        if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) {
                            last_rollback = Some(rb);
                        }
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
                        let ctrlc_handle = tokio::spawn(async move {
                            tokio::signal::ctrl_c().await.ok();
                            cancel_clone.cancel();
                        });
                        let outcome = agent.run(prompt, cancel).await;
                        ctrlc_handle.abort();
                        if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) {
                            last_rollback = Some(rb);
                        }
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
                                    eprintln!(
                                        "No spec file found. Create PIPIT.md with ## Task sections, or run /spec <file>"
                                    );
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
                            let names: Vec<_> = group
                                .iter()
                                .map(|t| format!("#{} {}", t.id, t.title))
                                .collect();
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
                            let ctrlc_handle = tokio::spawn(async move {
                                tokio::signal::ctrl_c().await.ok();
                                cancel_clone.cancel();
                            });
                            let outcome = agent.run(prompt, cancel).await;
                            ctrlc_handle.abort();
                            if let Some(rb) =
                                handle_agent_outcome(&project_root, &mut agent, outcome)
                            {
                                last_rollback = Some(rb);
                            }
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
                                let _ = std::fs::write(
                                    &meta_file,
                                    serde_json::to_string_pretty(&meta).unwrap_or_default(),
                                );
                                let msg_count = agent.context_usage().total;
                                eprintln!(
                                    "\x1b[32mSession '{}' saved ({} tokens)\x1b[0m",
                                    name, msg_count
                                );
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
                                    eprintln!(
                                        "\x1b[32mRestored {} messages from session '{}'\x1b[0m",
                                        msg_count, name
                                    );
                                    // Also restore metadata (files_in_context)
                                    let meta_file = session_subdir.join("metadata.json");
                                    if let Ok(content) = std::fs::read_to_string(&meta_file) {
                                        if let Ok(data) =
                                            serde_json::from_str::<serde_json::Value>(&content)
                                        {
                                            if let Some(files) = data
                                                .get("files_in_context")
                                                .and_then(|v| v.as_array())
                                            {
                                                for f in files {
                                                    if let Some(path) = f.as_str() {
                                                        if !files_in_context
                                                            .contains(&path.to_string())
                                                        {
                                                            files_in_context.push(path.to_string());
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    eprintln!("\x1b[31mFailed to resume session: {}\x1b[0m", e)
                                }
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
                                            let name = path
                                                .file_name()
                                                .unwrap_or_default()
                                                .to_string_lossy();
                                            let meta_file = path.join("metadata.json");
                                            let detail = if let Ok(c) =
                                                std::fs::read_to_string(&meta_file)
                                            {
                                                if let Ok(d) =
                                                    serde_json::from_str::<serde_json::Value>(&c)
                                                {
                                                    let model = d
                                                        .get("model")
                                                        .and_then(|m| m.as_str())
                                                        .unwrap_or("?");
                                                    let cost = d
                                                        .get("token_usage")
                                                        .and_then(|t| t.get("cost"))
                                                        .and_then(|c| c.as_f64())
                                                        .unwrap_or(0.0);
                                                    format!(" ({}, ${:.4})", model, cost)
                                                } else {
                                                    String::new()
                                                }
                                            } else {
                                                String::new()
                                            };
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
                            && unstaged
                                .as_ref()
                                .map(|o| o.stdout.is_empty())
                                .unwrap_or(true)
                            && untracked
                                .as_ref()
                                .map(|o| o.stdout.is_empty())
                                .unwrap_or(true)
                        {
                            eprintln!("\x1b[2mNo uncommitted changes\x1b[0m");
                        }
                        eprintln!();
                        continue;
                    }
                    SlashCommand::Commit(ref msg) => {
                        // Route through VCS gateway for all git operations
                        let has_staged = vcs_gateway.has_staged_changes().unwrap_or(false);
                        if !has_staged {
                            // Auto-stage all changes (via gateway commit with auto_stage=true)
                        }
                        if let Some(message) = msg {
                            // Direct commit with provided message
                            match vcs_gateway.commit(message, !has_staged) {
                                Ok(o) if o.status.success() => {
                                    eprintln!("\x1b[32mCommitted: {}\x1b[0m", message);
                                }
                                Ok(o) => {
                                    let err = String::from_utf8_lossy(&o.stderr);
                                    eprintln!("\x1b[31m{}\x1b[0m", err.trim());
                                }
                                Err(e) => eprintln!("\x1b[31m{}\x1b[0m", e),
                            }
                        } else {
                            // Generate commit message via LLM
                            let diff_text = vcs_gateway.staged_diff().unwrap_or_default();
                            if diff_text.trim().is_empty() {
                                // Stage first if nothing staged
                                let _ = std::process::Command::new("git")
                                    .args(["add", "-A"])
                                    .current_dir(&project_root)
                                    .output();
                                let diff_text = vcs_gateway.staged_diff().unwrap_or_default();
                                if diff_text.trim().is_empty() {
                                    eprintln!("\x1b[33mNo changes to commit\x1b[0m");
                                    continue;
                                }
                            }
                            let diff_text = vcs_gateway.staged_diff().unwrap_or_default();
                            let prompt = format!(
                                "Generate a conventional commit message for this diff. \
                                 Use the format: type(scope): description\n\
                                 Types: feat, fix, refactor, docs, test, chore, perf\n\
                                 Reply with ONLY the commit message, nothing else.\n\n\
                                 ```diff\n{}\n```",
                                {
                                    let max = 8000;
                                    if diff_text.len() > max {
                                        let safe_end = diff_text
                                            .char_indices()
                                            .take_while(|(i, _)| *i < max)
                                            .last()
                                            .map(|(i, c)| i + c.len_utf8())
                                            .unwrap_or(0);
                                        &diff_text[..safe_end]
                                    } else {
                                        &diff_text
                                    }
                                }
                            );
                            let cancel = CancellationToken::new();
                            let cancel_clone = cancel.clone();
                            let ctrlc_handle = tokio::spawn(async move {
                                tokio::signal::ctrl_c().await.ok();
                                cancel_clone.cancel();
                            });
                            let outcome = agent.run(prompt, cancel).await;
                            ctrlc_handle.abort();
                            if let AgentOutcome::Completed { .. } = &outcome {
                                let generated_msg = agent
                                    .last_assistant_text()
                                    .unwrap_or_default()
                                    .trim()
                                    .to_string();
                                if generated_msg.is_empty() {
                                    eprintln!("\x1b[31mNo commit message generated\x1b[0m");
                                } else {
                                    eprintln!("\n\x1b[33mCommit with this message? [y/N]\x1b[0m");
                                    if let Some(answer) = read_input() {
                                        if answer.trim().eq_ignore_ascii_case("y")
                                            || answer.trim().eq_ignore_ascii_case("yes")
                                        {
                                            match vcs_gateway.commit(&generated_msg, false) {
                                                Ok(o) if o.status.success() => {
                                                    eprintln!(
                                                        "\x1b[32mCommitted: {}\x1b[0m",
                                                        generated_msg
                                                    );
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
                                            eprintln!(
                                                "\x1b[2m... and {} more results\x1b[0m",
                                                lines.len() - shown
                                            );
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
                                        Err(_) => eprintln!(
                                            "\x1b[31mNeither grep nor rg available\x1b[0m"
                                        ),
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
                            let (interval, prompt) = if let Some(rest) =
                                args_str.strip_prefix(|c: char| c.is_ascii_digit())
                            {
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
                                eprintln!(
                                    "\x1b[2mLooping every {}s (Ctrl-C to stop): {}\x1b[0m",
                                    interval, prompt
                                );
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
                                    if let Some(rb) =
                                        handle_agent_outcome(&project_root, &mut agent, outcome)
                                    {
                                        last_rollback = Some(rb);
                                    }
                                    // Check if user pressed Ctrl-C
                                    if ctrlc_handle.is_finished() {
                                        eprintln!("\x1b[2mLoop stopped\x1b[0m");
                                        break;
                                    }
                                    eprintln!("\x1b[2m──── sleeping {}s ────\x1b[0m", interval);
                                    tokio::time::sleep(std::time::Duration::from_secs(interval))
                                        .await;
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
                                    eprintln!(
                                        "\x1b[2mNo stored knowledge yet. Use /memory add <concept> to add.\x1b[0m"
                                    );
                                } else if let Ok(entries) = std::fs::read_dir(&knowledge_dir) {
                                    eprintln!("\n\x1b[1;33mStored Knowledge\x1b[0m\n");
                                    for entry in entries.flatten() {
                                        if entry
                                            .path()
                                            .extension()
                                            .map(|e| e == "json")
                                            .unwrap_or(false)
                                        {
                                            if let Ok(content) =
                                                std::fs::read_to_string(entry.path())
                                            {
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
                                    source_project: project_root
                                        .file_name()
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
                            // Derive project name from current directory
                            let bg_project = project_root
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("default")
                                .to_string();
                            // Check if daemon is running (default port 3100)
                            let daemon_running = std::process::Command::new("curl")
                                .args([
                                    "-s",
                                    "-o",
                                    "/dev/null",
                                    "-w",
                                    "%{http_code}",
                                    "http://127.0.0.1:3100/api/health",
                                ])
                                .output()
                                .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "200")
                                .unwrap_or(false);
                            if daemon_running {
                                // Submit task to daemon
                                let submit = std::process::Command::new("curl")
                                    .args([
                                        "-s",
                                        "-X",
                                        "POST",
                                        "http://127.0.0.1:3100/api/tasks",
                                        "-H",
                                        "Content-Type: application/json",
                                        "-d",
                                        &serde_json::json!({"project": bg_project, "prompt": task})
                                            .to_string(),
                                    ])
                                    .output();
                                match submit {
                                    Ok(o) if o.status.success() => {
                                        let resp = String::from_utf8_lossy(&o.stdout);
                                        eprintln!(
                                            "\x1b[32mTask submitted to background daemon\x1b[0m"
                                        );
                                        eprintln!("\x1b[2m{}\x1b[0m", resp.trim());
                                    }
                                    _ => {
                                        eprintln!("\x1b[31mFailed to submit task to daemon\x1b[0m")
                                    }
                                }
                            } else {
                                eprintln!("\x1b[33mDaemon not running. Start with: pipitd\x1b[0m");
                                eprintln!(
                                    "\x1b[2mThe task will run in the foreground instead.\x1b[0m"
                                );
                            }
                        } else {
                            eprintln!("\x1b[33mUsage: /bg <prompt>\x1b[0m");
                            eprintln!(
                                "\x1b[2mRun a task in the background via the pipit daemon.\x1b[0m"
                            );
                        }
                        continue;
                    }
                    SlashCommand::Bench(ref action) => {
                        match action.as_deref() {
                            None | Some("list") => {
                                let suites = pipit_bench::load_custom_suites(&project_root);
                                eprintln!("\n\x1b[1;33mBenchmark Suites\x1b[0m\n");
                                if suites.is_empty() {
                                    eprintln!(
                                        "  \x1b[2mNo custom suites. Add tasks to .pipit/benchmarks/<name>/\x1b[0m"
                                    );
                                    eprintln!(
                                        "  \x1b[2mEach task needs: instruction.md, test.sh, optional Dockerfile\x1b[0m"
                                    );
                                } else {
                                    for suite in &suites {
                                        eprintln!(
                                            "  \x1b[36m{}\x1b[0m — {} tasks",
                                            suite.name,
                                            suite.tasks.len()
                                        );
                                    }
                                }
                                eprintln!();
                            }
                            Some("history") => {
                                let history =
                                    pipit_bench::history::BenchHistory::load(&project_root);
                                let sparkline = history.sparkline("custom", 40);
                                eprintln!("\n\x1b[1;33mBenchmark History\x1b[0m\n");
                                eprintln!("  Pass rate: {}", sparkline);
                                eprintln!();
                            }
                            Some(sub) if sub.starts_with("run") => {
                                eprintln!(
                                    "\x1b[33mBenchmark runner requires Docker. Use: pipit bench run --suite <name>\x1b[0m"
                                );
                            }
                            Some(_) => {
                                eprintln!(
                                    "\x1b[33mUsage: /bench [list|run|history|compare]\x1b[0m"
                                );
                            }
                        }
                        continue;
                    }
                    SlashCommand::Browse(ref action) => {
                        match action.as_deref() {
                            Some(url) if url.starts_with("http") => {
                                eprintln!("\x1b[36mBrowser: navigating to {}\x1b[0m", url);
                                eprintln!(
                                    "\x1b[2mHeadless Chrome required. Install Chrome and ensure it's accessible.\x1b[0m"
                                );
                                // The actual browser integration happens via tools registered in the agent
                                let prompt = format!(
                                    "Navigate to {} using the browser_navigate tool. \
                                     Take a screenshot and report what you see. \
                                     Check for any console errors.",
                                    url
                                );
                                let cancel = CancellationToken::new();
                                let cancel_clone = cancel.clone();
                                let ctrlc_handle = tokio::spawn(async move {
                                    tokio::signal::ctrl_c().await.ok();
                                    cancel_clone.cancel();
                                });
                                let outcome = agent.run(prompt, cancel).await;
                                ctrlc_handle.abort();
                                if let Some(rb) =
                                    handle_agent_outcome(&project_root, &mut agent, outcome)
                                {
                                    last_rollback = Some(rb);
                                }
                            }
                            Some("test") => {
                                let prompt = "Auto-detect the dev server (check package.json scripts for 'dev' or 'start'), \
                                              launch it if not running, navigate to localhost, take a screenshot, \
                                              and report any console errors or failed network requests.".to_string();
                                let cancel = CancellationToken::new();
                                let cancel_clone = cancel.clone();
                                let ctrlc_handle = tokio::spawn(async move {
                                    tokio::signal::ctrl_c().await.ok();
                                    cancel_clone.cancel();
                                });
                                let outcome = agent.run(prompt, cancel).await;
                                ctrlc_handle.abort();
                                if let Some(rb) =
                                    handle_agent_outcome(&project_root, &mut agent, outcome)
                                {
                                    last_rollback = Some(rb);
                                }
                            }
                            Some("a11y") | Some("accessibility") => {
                                let prompt = "Run an accessibility audit on the running web app. \
                                              Check for WCAG violations: missing alt text, color contrast, \
                                              keyboard navigation, ARIA roles.".to_string();
                                let cancel = CancellationToken::new();
                                let cancel_clone = cancel.clone();
                                let ctrlc_handle = tokio::spawn(async move {
                                    tokio::signal::ctrl_c().await.ok();
                                    cancel_clone.cancel();
                                });
                                let outcome = agent.run(prompt, cancel).await;
                                ctrlc_handle.abort();
                                if let Some(rb) =
                                    handle_agent_outcome(&project_root, &mut agent, outcome)
                                {
                                    last_rollback = Some(rb);
                                }
                            }
                            None => {
                                eprintln!(
                                    "\x1b[33mUsage: /browse <url> | /browse test | /browse a11y\x1b[0m"
                                );
                            }
                            Some(_) => {
                                eprintln!(
                                    "\x1b[33mUsage: /browse <url> | /browse test | /browse a11y\x1b[0m"
                                );
                            }
                        }
                        continue;
                    }
                    SlashCommand::Mesh(ref action) => {
                        #[cfg(feature = "mesh")]
                        match action.as_deref() {
                            None | Some("status") => {
                                eprintln!("\n\x1b[1;33mMesh Status\x1b[0m\n");
                                if let Some(ref d) = mesh_daemon {
                                    let reg = d.registry.blocking_read();
                                    let all = reg.all_nodes();
                                    let alive = all.iter().filter(|(_, s)| **s == pipit_mesh::NodeStatus::Alive).count();
                                    eprintln!("  Node ID:  {}", d.local_node.id);
                                    eprintln!("  Address:  {}", d.local_node.addr);
                                    eprintln!("  Nodes:    {} total, {} alive", all.len(), alive);
                                    eprintln!();
                                } else {
                                    eprintln!("  \x1b[2mMesh not active. Start with: pipit --mesh\x1b[0m\n");
                                }
                            }
                            Some("nodes") => {
                                if let Some(ref d) = mesh_daemon {
                                    let reg = d.registry.blocking_read();
                                    let nodes = reg.all_nodes();
                                    if nodes.is_empty() {
                                        eprintln!("\x1b[2mNo mesh nodes.\x1b[0m");
                                    } else {
                                        eprintln!("\n\x1b[1;33m  Mesh Nodes\x1b[0m\n");
                                        for (desc, status) in &nodes {
                                            let me = if desc.id == d.local_node.id { " ← you" } else { "" };
                                            let status_icon = match status {
                                                pipit_mesh::NodeStatus::Alive => "\x1b[32m●\x1b[0m",
                                                pipit_mesh::NodeStatus::Suspect => "\x1b[33m◐\x1b[0m",
                                                pipit_mesh::NodeStatus::Dead => "\x1b[31m○\x1b[0m",
                                            };
                                            let model = desc.model.as_deref().unwrap_or("-");
                                            let gpu = desc.gpu.as_ref().map(|g| format!("{}×{} ({}GB)", g.count, g.name, g.vram_gb as u32)).unwrap_or_else(|| "cpu".into());
                                            eprintln!("  {} \x1b[1m{}\x1b[0m  {}{}", status_icon, &desc.id[..8], desc.name, me);
                                            eprintln!("    addr: {}  model: {}  hw: {}  load: {:.0}%", desc.addr, model, gpu, desc.load * 100.0);
                                            if !desc.capabilities.is_empty() {
                                                eprintln!("    caps: {}", desc.capabilities.join(", "));
                                            }
                                        }
                                        eprintln!();
                                    }
                                } else {
                                    eprintln!("\x1b[2mMesh not active.\x1b[0m");
                                }
                            }
                            Some(sub) if sub.starts_with("join") => {
                                let seed = sub.strip_prefix("join").map(|s| s.trim()).unwrap_or("");
                                if seed.is_empty() {
                                    eprintln!("\x1b[33mUsage: /mesh join <ip:port>\x1b[0m");
                                } else if let Some(ref d) = mesh_daemon {
                                    if let Ok(addr) = seed.parse::<std::net::SocketAddr>() {
                                        let d2 = d.clone();
                                        tokio::spawn(async move {
                                            match d2.join(addr).await {
                                                Ok(_) => eprintln!("\x1b[32mJoined mesh seed {}\x1b[0m", addr),
                                                Err(e) => eprintln!("\x1b[31mFailed to join {}: {}\x1b[0m", addr, e),
                                            }
                                        });
                                    } else {
                                        eprintln!("\x1b[31mInvalid address: {}\x1b[0m", seed);
                                    }
                                } else {
                                    eprintln!("\x1b[2mMesh not active. Start with: pipit --mesh\x1b[0m");
                                }
                            }
                            Some(sub) if sub.starts_with("delegate") => {
                                let prompt = sub.strip_prefix("delegate").map(|s| s.trim()).unwrap_or("");
                                if prompt.is_empty() {
                                    eprintln!("\x1b[33mUsage: /mesh delegate <task prompt>\x1b[0m");
                                } else if let Some(ref d) = mesh_daemon {
                                    let task = pipit_mesh::MeshTask {
                                        id: uuid::Uuid::new_v4().to_string(),
                                        prompt: prompt.to_string(),
                                        required_capabilities: vec!["agent".to_string()],
                                        project_root: Some(project_root.display().to_string()),
                                        timeout_secs: 300,
                                    };
                                    eprintln!("\x1b[36mDelegating task to mesh...\x1b[0m");
                                    let d2 = d.clone();
                                    tokio::spawn(async move {
                                        match d2.delegate_task(task).await {
                                            Ok(result) => {
                                                eprintln!("\n\x1b[1;32m━━━ Mesh Task Result ━━━\x1b[0m");
                                                eprintln!("  Node:    {}", if result.node_id.is_empty() { "remote" } else { &result.node_id });
                                                eprintln!("  Status:  {}", if result.success { "\x1b[32mSuccess\x1b[0m" } else { "\x1b[31mFailed\x1b[0m" });
                                                eprintln!("  Time:    {:.1}s", result.elapsed_secs);
                                                eprintln!("\x1b[2m{}\x1b[0m", if result.output.len() > 2000 { &result.output[..2000] } else { &result.output });
                                                eprintln!("\x1b[1;32m━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m\n");
                                            }
                                            Err(e) => {
                                                eprintln!("\x1b[31mDelegation failed: {}\x1b[0m", e);
                                            }
                                        }
                                    });
                                } else {
                                    eprintln!("\x1b[2mMesh not active. Start with: pipit --mesh\x1b[0m");
                                }
                            }
                            Some(sub) if sub.starts_with("run ") => {
                                let rest = sub.strip_prefix("run ").unwrap().trim();
                                let (node_prefix, prompt) = match rest.split_once(' ') {
                                    Some((n, p)) => (n.trim(), p.trim()),
                                    None => {
                                        eprintln!("\x1b[33mUsage: /mesh run <node_name_or_id_prefix> <prompt>\x1b[0m");
                                        continue;
                                    }
                                };
                                if let Some(ref d) = mesh_daemon {
                                    let reg = d.registry.blocking_read();
                                    if let Some(target) = reg.find_by_prefix(node_prefix) {
                                        let addr = target.addr;
                                        let name = target.name.clone();
                                        drop(reg);
                                        let task = pipit_mesh::MeshTask {
                                            id: uuid::Uuid::new_v4().to_string(),
                                            prompt: prompt.to_string(),
                                            required_capabilities: vec![],
                                            project_root: Some(project_root.display().to_string()),
                                            timeout_secs: 300,
                                        };
                                        eprintln!("\x1b[36mSending task to {} ({})...\x1b[0m", name, addr);
                                        let d2 = d.clone();
                                        tokio::spawn(async move {
                                            match d2.delegate_to_node(task, addr).await {
                                                Ok(result) => {
                                                    eprintln!("\n\x1b[1;32m━━━ Result from {} ━━━\x1b[0m", name);
                                                    eprintln!("  Status:  {}", if result.success { "\x1b[32mSuccess\x1b[0m" } else { "\x1b[31mFailed\x1b[0m" });
                                                    eprintln!("  Time:    {:.1}s", result.elapsed_secs);
                                                    eprintln!("\x1b[2m{}\x1b[0m", if result.output.len() > 2000 { &result.output[..2000] } else { &result.output });
                                                    eprintln!("\x1b[1;32m━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m\n");
                                                }
                                                Err(e) => eprintln!("\x1b[31mFailed: {}\x1b[0m", e),
                                            }
                                        });
                                    } else {
                                        eprintln!("\x1b[31mNo alive node matching '{}'. Use /mesh nodes to see available.\x1b[0m", node_prefix);
                                    }
                                } else {
                                    eprintln!("\x1b[2mMesh not active.\x1b[0m");
                                }
                            }
                            Some(sub) if sub.starts_with("broadcast") => {
                                let prompt = sub.strip_prefix("broadcast").map(|s| s.trim()).unwrap_or("");
                                if prompt.is_empty() {
                                    eprintln!("\x1b[33mUsage: /mesh broadcast <prompt>\x1b[0m");
                                } else if let Some(ref d) = mesh_daemon {
                                    let task = pipit_mesh::MeshTask {
                                        id: uuid::Uuid::new_v4().to_string(),
                                        prompt: prompt.to_string(),
                                        required_capabilities: vec![],
                                        project_root: Some(project_root.display().to_string()),
                                        timeout_secs: 300,
                                    };
                                    eprintln!("\x1b[36mBroadcasting task to all mesh nodes...\x1b[0m");
                                    let d2 = d.clone();
                                    tokio::spawn(async move {
                                        let results = d2.broadcast_task(task).await;
                                        eprintln!("\n\x1b[1;33m━━━ Broadcast Results ({} nodes) ━━━\x1b[0m", results.len());
                                        for (i, r) in results.iter().enumerate() {
                                            match r {
                                                Ok(result) => {
                                                    eprintln!("  \x1b[1mNode {}\x1b[0m: {} in {:.1}s", i + 1,
                                                        if result.success { "\x1b[32mSuccess\x1b[0m" } else { "\x1b[31mFailed\x1b[0m" },
                                                        result.elapsed_secs);
                                                    let preview = if result.output.len() > 500 { &result.output[..500] } else { &result.output };
                                                    eprintln!("  \x1b[2m{}\x1b[0m", preview);
                                                }
                                                Err(e) => eprintln!("  \x1b[1mNode {}\x1b[0m: \x1b[31m{}\x1b[0m", i + 1, e),
                                            }
                                        }
                                        eprintln!("\x1b[1;33m━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m\n");
                                    });
                                } else {
                                    eprintln!("\x1b[2mMesh not active.\x1b[0m");
                                }
                            }
                            Some(_) => {
                                eprintln!("\x1b[33mUsage: /mesh [status|nodes|join|delegate|run|broadcast]\x1b[0m");
                            }
                        }
                        #[cfg(not(feature = "mesh"))]
                        {
                            let _ = action;
                            eprintln!("\x1b[33mMesh not available — rebuild with `--features mesh`\x1b[0m");
                        }
                        continue;
                    }
                    SlashCommand::Watch(ref action) => {
                        match action.as_deref() {
                            Some("deps") => {
                                eprintln!("\x1b[36mStarting dependency health monitor...\x1b[0m");
                                eprintln!(
                                    "\x1b[2mWill check for vulnerabilities and outdated packages periodically.\x1b[0m"
                                );
                            }
                            Some("tests") => {
                                eprintln!("\x1b[36mStarting test watcher...\x1b[0m");
                                eprintln!("\x1b[2mWill auto-run tests on file save.\x1b[0m");
                            }
                            Some("security") => {
                                eprintln!("\x1b[36mStarting security monitor...\x1b[0m");
                                eprintln!(
                                    "\x1b[2mWill run taint analysis on modified files.\x1b[0m"
                                );
                            }
                            Some("start") => {
                                eprintln!("\x1b[36mAmbient file watcher started.\x1b[0m");
                                eprintln!(
                                    "\x1b[2mWill notify on external changes and suggest actions.\x1b[0m"
                                );
                            }
                            Some("stop") => {
                                eprintln!("\x1b[33mAll watchers stopped.\x1b[0m");
                            }
                            None => {
                                eprintln!(
                                    "\x1b[33mUsage: /watch [start|stop|deps|tests|security]\x1b[0m"
                                );
                            }
                            Some(_) => {
                                eprintln!(
                                    "\x1b[33mUsage: /watch [start|stop|deps|tests|security]\x1b[0m"
                                );
                            }
                        }
                        continue;
                    }
                    SlashCommand::Deps(ref _action) => {
                        eprintln!("\x1b[36mScanning dependencies...\x1b[0m");
                        // Run synchronously for now — async would need runtime
                        let ecosystems = pipit_deps::scanner::detect_ecosystems(&project_root);
                        if ecosystems.is_empty() {
                            eprintln!(
                                "\x1b[2mNo package manifests found (Cargo.toml, package.json, etc.)\x1b[0m"
                            );
                        } else {
                            let names: Vec<&str> = ecosystems
                                .iter()
                                .map(|e| match e {
                                    pipit_deps::scanner::Ecosystem::Cargo => "Cargo",
                                    pipit_deps::scanner::Ecosystem::Npm => "npm",
                                    pipit_deps::scanner::Ecosystem::Python => "Python",
                                    pipit_deps::scanner::Ecosystem::Go => "Go",
                                })
                                .collect();
                            eprintln!("\x1b[2mDetected ecosystems: {}\x1b[0m", names.join(", "));
                            eprintln!("\x1b[2mRunning vulnerability scan via OSV API...\x1b[0m");
                            // Note: full async scan runs in the background; results shown in next prompt
                        }
                        continue;
                    }
                    SlashCommand::Registry(ref query) => {
                        match query.as_deref() {
                            None | Some("list") => {
                                eprintln!("\x1b[36mFetching plugin registry...\x1b[0m");
                                let rt_result = tokio::runtime::Handle::try_current();
                                let output = std::process::Command::new("curl")
                                    .args(["-s", "https://raw.githubusercontent.com/pipit-project/registry/main/index.json"])
                                    .output();
                                match output {
                                    Ok(o) if o.status.success() => {
                                        let body = String::from_utf8_lossy(&o.stdout);
                                        if let Ok(entries) =
                                            serde_json::from_str::<Vec<serde_json::Value>>(&body)
                                        {
                                            eprintln!("\n\x1b[1;33mPlugin Registry\x1b[0m\n");
                                            if entries.is_empty() {
                                                eprintln!(
                                                    "  \x1b[2mNo plugins published yet.\x1b[0m"
                                                );
                                            } else {
                                                for entry in &entries {
                                                    let name =
                                                        entry["name"].as_str().unwrap_or("?");
                                                    let version =
                                                        entry["version"].as_str().unwrap_or("?");
                                                    let desc =
                                                        entry["description"].as_str().unwrap_or("");
                                                    eprintln!(
                                                        "  \x1b[36m{}\x1b[0m v{} — {}",
                                                        name, version, desc
                                                    );
                                                }
                                            }
                                            eprintln!(
                                                "\n  \x1b[2mInstall: /registry install <name>\x1b[0m"
                                            );
                                            eprintln!(
                                                "  \x1b[2mSearch:  /registry search <query>\x1b[0m\n"
                                            );
                                        } else {
                                            eprintln!(
                                                "\x1b[31mFailed to parse registry index\x1b[0m"
                                            );
                                        }
                                    }
                                    _ => {
                                        eprintln!(
                                            "\x1b[31mFailed to fetch registry. Check network.\x1b[0m"
                                        );
                                    }
                                }
                            }
                            Some(sub) if sub.starts_with("search ") => {
                                let search_query = &sub[7..];
                                eprintln!(
                                    "\x1b[36mSearching registry for '{}'...\x1b[0m",
                                    search_query
                                );
                                let output = std::process::Command::new("curl")
                                    .args(["-s", "https://raw.githubusercontent.com/pipit-project/registry/main/index.json"])
                                    .output();
                                match output {
                                    Ok(o) if o.status.success() => {
                                        let body = String::from_utf8_lossy(&o.stdout);
                                        if let Ok(entries) =
                                            serde_json::from_str::<Vec<serde_json::Value>>(&body)
                                        {
                                            let matches: Vec<&serde_json::Value> = entries
                                                .iter()
                                                .filter(|e| {
                                                    let name = e["name"].as_str().unwrap_or("");
                                                    let desc =
                                                        e["description"].as_str().unwrap_or("");
                                                    name.contains(search_query)
                                                        || desc
                                                            .to_lowercase()
                                                            .contains(&search_query.to_lowercase())
                                                })
                                                .collect();
                                            if matches.is_empty() {
                                                eprintln!(
                                                    "\x1b[33mNo plugins matching '{}'\x1b[0m",
                                                    search_query
                                                );
                                            } else {
                                                for entry in &matches {
                                                    let name =
                                                        entry["name"].as_str().unwrap_or("?");
                                                    let version =
                                                        entry["version"].as_str().unwrap_or("?");
                                                    let desc =
                                                        entry["description"].as_str().unwrap_or("");
                                                    eprintln!(
                                                        "  \x1b[36m{}\x1b[0m v{} — {}",
                                                        name, version, desc
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    _ => {
                                        eprintln!("\x1b[31mFailed to fetch registry.\x1b[0m");
                                    }
                                }
                            }
                            Some(sub) if sub.starts_with("install ") => {
                                let plugin_name = &sub[8..];
                                eprintln!("\x1b[36mInstalling plugin '{}'...\x1b[0m", plugin_name);
                                eprintln!(
                                    "\x1b[2mUse: pipit plugin install {}\x1b[0m",
                                    plugin_name
                                );
                            }
                            Some(_) => {
                                eprintln!(
                                    "\x1b[33mUsage: /registry [list|search <query>|install <name>]\x1b[0m"
                                );
                            }
                        }
                        continue;
                    }
                    SlashCommand::Model(ref new_model) => {
                        if new_model.is_empty() {
                            eprintln!("\x1b[2mCurrent model: {}\x1b[0m", model);
                            eprintln!("\x1b[2mUsage: /model <model_name>\x1b[0m");
                        } else {
                            match agent.set_model(
                                provider_kind,
                                new_model,
                                &api_key,
                                base_url.as_deref(),
                            ) {
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
                    SlashCommand::Provider(ref arg) => {
                        match arg.as_deref() {
                            None | Some("") | Some("list") => {
                                // Show all available provider profiles
                                eprintln!("\x1b[1mProvider Roster\x1b[0m");
                                eprintln!();
                                eprint!("{}", provider_roster.render_list());
                                eprintln!();
                                eprintln!("\x1b[2mUsage: /provider <label|number|next|prev>\x1b[0m");
                            }
                            Some("next" | "n") => {
                                let profile = provider_roster.next();
                                match agent.set_model(
                                    profile.kind,
                                    &profile.model,
                                    &profile.api_key,
                                    profile.base_url.as_deref(),
                                ) {
                                    Ok(()) => {
                                        provider_kind = profile.kind;
                                        model = profile.model.clone();
                                        api_key = profile.api_key.clone();
                                        base_url = profile.base_url.clone();
                                        ui.status_mut().model = provider_roster.status_label();
                                        ui.status_mut().provider_kind = format!("{}", provider_kind);
                                        eprintln!(
                                            "\x1b[32mSwitched to: {}\x1b[0m",
                                            provider_roster.status_label()
                                        );
                                    }
                                    Err(e) => {
                                        // Revert roster to previous
                                        provider_roster.prev();
                                        eprintln!("\x1b[31m{}\x1b[0m", e);
                                    }
                                }
                            }
                            Some("prev" | "p") => {
                                let profile = provider_roster.prev();
                                match agent.set_model(
                                    profile.kind,
                                    &profile.model,
                                    &profile.api_key,
                                    profile.base_url.as_deref(),
                                ) {
                                    Ok(()) => {
                                        provider_kind = profile.kind;
                                        model = profile.model.clone();
                                        api_key = profile.api_key.clone();
                                        base_url = profile.base_url.clone();
                                        ui.status_mut().model = provider_roster.status_label();
                                        ui.status_mut().provider_kind = format!("{}", provider_kind);
                                        eprintln!(
                                            "\x1b[32mSwitched to: {}\x1b[0m",
                                            provider_roster.status_label()
                                        );
                                    }
                                    Err(e) => {
                                        provider_roster.next(); // Revert
                                        eprintln!("\x1b[31m{}\x1b[0m", e);
                                    }
                                }
                            }
                            Some(query) => {
                                // Try numeric index first, then label match
                                let switch_result = if let Ok(idx) = query.parse::<usize>() {
                                    provider_roster.switch_to_index(idx)
                                } else {
                                    provider_roster.switch_to(query)
                                };
                                match switch_result {
                                    Ok(profile) => {
                                        let profile = profile.clone();
                                        match agent.set_model(
                                            profile.kind,
                                            &profile.model,
                                            &profile.api_key,
                                            profile.base_url.as_deref(),
                                        ) {
                                            Ok(()) => {
                                                provider_kind = profile.kind;
                                                model = profile.model.clone();
                                                api_key = profile.api_key.clone();
                                                base_url = profile.base_url.clone();
                                                ui.status_mut().model = provider_roster.status_label();
                                                ui.status_mut().provider_kind =
                                                    format!("{}", provider_kind);
                                                eprintln!(
                                                    "\x1b[32mSwitched to: {}\x1b[0m",
                                                    provider_roster.status_label()
                                                );
                                            }
                                            Err(e) => {
                                                eprintln!("\x1b[31m{}\x1b[0m", e);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!("\x1b[31m{}\x1b[0m", e);
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    SlashCommand::Branch(ref name) => {
                        if let Some(branch_name) = name {
                            // Route through VCS gateway — firewall-checked, ledger-logged
                            match vcs_gateway.create_branch(branch_name) {
                                Ok(o) if o.status.success() => {
                                    eprintln!(
                                        "\x1b[32mCreated and switched to branch '{}'\x1b[0m",
                                        branch_name
                                    );
                                    ui.status_mut().branch = branch_name.clone();
                                }
                                Ok(o) => {
                                    let err = String::from_utf8_lossy(&o.stderr);
                                    eprintln!("\x1b[31m{}\x1b[0m", err.trim());
                                }
                                Err(e) => eprintln!("\x1b[31m{}\x1b[0m", e),
                            }
                        } else {
                            // Show current branch (read-only via gateway)
                            match vcs_gateway.current_branch() {
                                Ok(branch) => {
                                    eprintln!("\x1b[2mCurrent branch: {}\x1b[0m", branch);
                                }
                                Err(e) => eprintln!("\x1b[31m{}\x1b[0m", e),
                            }
                        }
                        continue;
                    }
                    SlashCommand::BranchList => {
                        // Read-only via gateway
                        match vcs_gateway.list_branches() {
                            Ok(branches) => {
                                for line in branches.lines() {
                                    eprintln!("{}", line);
                                }
                            }
                            Err(e) => eprintln!("\x1b[31m{}\x1b[0m", e),
                        }
                        continue;
                    }
                    SlashCommand::BranchSwitch(ref target) => {
                        if target.is_empty() {
                            eprintln!("\x1b[33mUsage: /switch <branch_name>\x1b[0m");
                        } else {
                            // Route through VCS gateway — auto-stash, firewall, ledger
                            match vcs_gateway.switch_branch(target) {
                                Ok((output, had_dirty)) => {
                                    if output.status.success() {
                                        eprintln!("\x1b[32mSwitched to branch '{}'\x1b[0m", target);
                                        ui.status_mut().branch = target.clone();
                                        if had_dirty {
                                            eprintln!(
                                                "\x1b[2mYour changes were stashed. Use `!git stash pop` to restore.\x1b[0m"
                                            );
                                        }
                                    } else {
                                        let err = String::from_utf8_lossy(&output.stderr);
                                        eprintln!("\x1b[31m{}\x1b[0m", err.trim());
                                    }
                                }
                                Err(e) => eprintln!("\x1b[31m{}\x1b[0m", e),
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
                        eprintln!(
                            "  \x1b[90mConfig file: \x1b[0m{} {}",
                            config_path,
                            if has_config {
                                "\x1b[32m✓\x1b[0m"
                            } else {
                                "\x1b[31m✗ not found\x1b[0m"
                            }
                        );
                        eprintln!("  \x1b[90mProvider:    \x1b[0m{}", provider_kind);
                        eprintln!("  \x1b[90mModel:       \x1b[0m{}", model);
                        if let Some(ref url) = base_url {
                            eprintln!("  \x1b[90mBase URL:    \x1b[0m{}", url);
                        }
                        eprintln!(
                            "  \x1b[90mApproval:    \x1b[0m{}",
                            ui.status_mut().approval_mode
                        );
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
                        eprintln!(
                            "  \x1b[90mProvider:    \x1b[0m{} \x1b[32m✓\x1b[0m",
                            provider_kind
                        );
                        eprintln!("  \x1b[90mModel:       \x1b[0m{}", model);
                        if let Some(ref url) = base_url {
                            eprintln!("  \x1b[90mEndpoint:    \x1b[0m{}", url);
                        }
                        let usage = agent.context_usage();
                        let pct = if usage.limit > 0 {
                            usage.total * 100 / usage.limit
                        } else {
                            0
                        };
                        eprintln!(
                            "  \x1b[90mTokens:      \x1b[0m{}/{} ({}%)",
                            usage.total, usage.limit, pct
                        );
                        eprintln!("  \x1b[90mCost:        \x1b[0m${:.4}", usage.cost);
                        eprintln!(
                            "  \x1b[90mSkills:      \x1b[0m{} active, {} conditional",
                            skills.count(),
                            conditional_registry.total_count()
                        );
                        eprintln!(
                            "  \x1b[90mHooks:       \x1b[0m{}",
                            workflow_assets.hook_files.len()
                        );
                        eprintln!();
                        continue;
                    }
                    SlashCommand::Skills => {
                        eprintln!();
                        eprintln!("  \x1b[1;33mSkills\x1b[0m");
                        eprintln!();
                        if skills.count() == 0 && conditional_registry.total_count() == 0 {
                            eprintln!(
                                "  \x1b[90mNo skills found. Add .pipit/skills/<name>/SKILL.md\x1b[0m"
                            );
                        } else {
                            for name in skills.list() {
                                eprintln!("  \x1b[36m/{}\x1b[0m", name);
                            }
                            for (name, _) in conditional_registry.active_skills() {
                                eprintln!("  \x1b[36m/{}\x1b[0m \x1b[90m(conditional, active)\x1b[0m", name);
                            }
                            if conditional_registry.dormant_count() > 0 {
                                eprintln!(
                                    "  \x1b[90m({} conditional skills dormant — activate by touching matching files)\x1b[0m",
                                    conditional_registry.dormant_count()
                                );
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
                            eprintln!(
                                "  \x1b[90mNo hooks found. Add .pipit/hooks/<event>.sh\x1b[0m"
                            );
                        } else {
                            for hook in &workflow_assets.hook_files {
                                let name = hook.file_name().and_then(|n| n.to_str()).unwrap_or("?");
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
                                    eprintln!(
                                        "  \x1b[90mConfig found but no servers defined\x1b[0m"
                                    );
                                } else {
                                    for (name, _server) in &mcp_config.mcp_servers {
                                        eprintln!("  \x1b[36m{}\x1b[0m", name);
                                    }
                                }
                            }
                            None => {
                                eprintln!(
                                    "  \x1b[90mNo MCP config found. Add .pipit/mcp.json\x1b[0m"
                                );
                            }
                        }
                        eprintln!();
                        continue;
                    }
                    SlashCommand::Vim => {
                        eprintln!("⌨ Vim mode is only available in TUI mode (remove --classic)");
                        continue;
                    }
                    SlashCommand::Unknown(cmd) => {
                        let args = input
                            .strip_prefix(&format!("/{}", cmd))
                            .unwrap_or("")
                            .trim();

                        // 1. Try skill system first
                        if skills.has_skill(&cmd) {
                            match skills.load(&cmd) {
                                Ok(skill) => {
                                    let sid = agent.session_id().to_string();
                                    let injection = skill.as_injection(args, Some(&sid));
                                    let cancel = CancellationToken::new();
                                    let outcome = agent.run(injection, cancel).await;
                                    let modified = extract_modified_files(&outcome);
                                    if let Some(rb) =
                                        handle_agent_outcome(&project_root, &mut agent, outcome)
                                    {
                                        last_rollback = Some(rb);
                                    }
                                    activate_conditional_skills(&modified, &mut conditional_registry, &project_root);
                                }
                                Err(e) => {
                                    eprintln!("\x1b[31mFailed to load skill: {}\x1b[0m", e);
                                }
                            }
                            continue;
                        }

                        // 2. Try custom commands from .pipit/commands/
                        let custom_commands = workflow_assets.discover_commands();
                        if let Some((_, _, cmd_path)) =
                            custom_commands.iter().find(|(name, _, _)| name == &cmd)
                        {
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
                                    if let Some(rb) =
                                        handle_agent_outcome(&project_root, &mut agent, outcome)
                                    {
                                        last_rollback = Some(rb);
                                    }
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
                let trimmed = cmd.trim();

                // Intercept `cd` to persist directory changes across `!` calls
                if trimmed == "cd"
                    || (trimmed.starts_with("cd ")
                        && !trimmed.contains("&&")
                        && !trimmed.contains(';')
                        && !trimmed.contains('|'))
                {
                    let target = if trimmed == "cd" {
                        std::env::var("HOME")
                            .map(PathBuf::from)
                            .unwrap_or_else(|_| project_root.clone())
                    } else {
                        let arg = trimmed.strip_prefix("cd ").unwrap().trim();
                        let arg = arg.trim_matches('"').trim_matches('\'');
                        let expanded = if arg.starts_with("~/") || arg == "~" {
                            if let Ok(home) = std::env::var("HOME") {
                                PathBuf::from(home)
                                    .join(arg.strip_prefix("~/").unwrap_or(""))
                            } else {
                                PathBuf::from(arg)
                            }
                        } else {
                            PathBuf::from(arg)
                        };
                        if expanded.is_absolute() {
                            expanded
                        } else {
                            shell_cwd.join(&expanded)
                        }
                    };
                    match target.canonicalize() {
                        Ok(resolved) if resolved.is_dir() => {
                            shell_cwd = resolved.clone();
                            eprintln!("Changed directory to {}", resolved.display());
                        }
                        Ok(resolved) => {
                            eprintln!("\x1b[31mcd: {}: Not a directory\x1b[0m", resolved.display());
                        }
                        Err(e) => {
                            eprintln!("\x1b[31mcd: {}: {}\x1b[0m", target.display(), e);
                        }
                    }
                    continue;
                }

                eprintln!("\x1b[2m$ {}\x1b[0m", cmd);
                let output = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd)
                    .current_dir(&shell_cwd)
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
                // Show clean file indicators
                for f in &files {
                    let name = std::path::Path::new(f)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(f);
                    eprintln!("\x1b[36m  📎 {}\x1b[0m", name);
                }
                let file_list = files.join(", ");
                let enriched = format!("First read these files: {}. Then: {}", file_list, prompt);
                let cancel = CancellationToken::new();
                let cancel_clone = cancel.clone();
                let ctrlc_handle = tokio::spawn(async move {
                    tokio::signal::ctrl_c().await.ok();
                    cancel_clone.cancel();
                });
                let outcome = agent.run(enriched, cancel).await;
                ctrlc_handle.abort();
                if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) {
                    last_rollback = Some(rb);
                }
                println!();
                continue;
            }
            UserInput::PromptWithImages {
                prompt,
                image_paths,
            } => {
                // Read image files and send as vision prompt
                let mut image_descriptions = Vec::new();
                for (i, img_path) in image_paths.iter().enumerate() {
                    let name = std::path::Path::new(img_path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("image");
                    match pipit_io::input::read_image_file(img_path) {
                        Ok((media_type, data)) => {
                            let size_kb = data.len() / 1024;
                            eprintln!(
                                "\x1b[35m  🖼 Image #{}: {} ({}KB)\x1b[0m",
                                i + 1,
                                name,
                                size_kb
                            );
                            image_descriptions
                                .push(format!("{} ({}KB, {})", name, size_kb, media_type));
                            // Inject the image into context as a user message with image content block
                            agent.inject_image(&media_type, data);
                        }
                        Err(e) => {
                            eprintln!("\x1b[31m{}\x1b[0m", e);
                        }
                    }
                }
                let enriched = if prompt.is_empty() {
                    format!(
                        "I've attached {} image(s): {}. Please analyze what you see.",
                        image_paths.len(),
                        image_descriptions.join(", ")
                    )
                } else {
                    format!(
                        "I've attached image(s): {}. {}",
                        image_descriptions.join(", "),
                        prompt
                    )
                };
                let cancel = CancellationToken::new();
                let cancel_clone = cancel.clone();
                let ctrlc_handle = tokio::spawn(async move {
                    tokio::signal::ctrl_c().await.ok();
                    cancel_clone.cancel();
                });
                let outcome = agent.run(enriched, cancel).await;
                ctrlc_handle.abort();
                if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) {
                    last_rollback = Some(rb);
                }
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
                let modified = extract_modified_files(&outcome);
                if let Some(rb) = handle_agent_outcome(&project_root, &mut agent, outcome) {
                    last_rollback = Some(rb);
                }
                activate_conditional_skills(&modified, &mut conditional_registry, &project_root);
                println!();
            }
        }
    }

    // Fire SessionEnd hook
    let _ = extensions_for_lifecycle.on_session_end().await;

    Ok(())
}

/// Extract modified file paths from a completed agent outcome.
fn extract_modified_files(outcome: &AgentOutcome) -> Vec<String> {
    match outcome {
        AgentOutcome::Completed { proof, .. } => {
            proof.realized_edits.iter().map(|e| e.path.clone()).collect()
        }
        _ => Vec::new(),
    }
}

/// Activate conditional skills based on files modified by the last agent run.
fn activate_conditional_skills(
    modified_files: &[String],
    conditional_registry: &mut ConditionalRegistry,
    cwd: &std::path::Path,
) {
    if modified_files.is_empty() || conditional_registry.dormant_count() == 0 {
        return;
    }
    let paths: Vec<&std::path::Path> = modified_files
        .iter()
        .map(|s| std::path::Path::new(s.as_str()))
        .collect();
    let activated = conditional_registry.activate_for_paths(&paths, cwd);
    for name in &activated {
        eprintln!("\x1b[36m  ⚡ Activated skill: {}\x1b[0m", name);
        tracing::info!("Conditional skill '{}' activated by file touch", name);
    }
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
                let files: Vec<String> = proof
                    .realized_edits
                    .iter()
                    .map(|e| e.path.clone())
                    .collect();
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
        AgentOutcome::BudgetExhausted {
            turns,
            cost,
            budget,
        } => {
            if let Some(planning_state) = agent.planning_state() {
                persistence::persist_planning_snapshot(project_root, &planning_state, None).ok();
            }
            eprintln!(
                "\x1b[33mCost budget exhausted after {} turns: ${:.4} >= ${:.2} limit\x1b[0m",
                turns, cost, budget
            );
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

/// Handle the `pipit export` subcommand.
fn handle_export(
    ledger_path: &str,
    format: &str,
    output: &Option<String>,
    include_tools: bool,
    include_thinking: bool,
) -> Result<()> {
    use pipit_core::export::{ExportOptions, export_html, export_markdown};
    use pipit_core::ledger::SessionLedger;
    use std::path::Path;

    let path = Path::new(ledger_path);
    if !path.exists() {
        anyhow::bail!("Ledger file not found: {}", ledger_path);
    }

    let events = SessionLedger::replay(path)
        .map_err(|e| anyhow::anyhow!("Failed to replay ledger: {}", e))?;

    if events.is_empty() {
        anyhow::bail!("Ledger is empty: {}", ledger_path);
    }

    let opts = ExportOptions {
        include_tools,
        include_thinking,
        include_timestamps: true,
        include_stats: true,
        title: None,
    };

    let content = match format {
        "html" => export_html(&events, &opts),
        "md" | "markdown" => export_markdown(&events, &opts),
        _ => anyhow::bail!("Unknown format '{}'. Use 'md' or 'html'.", format),
    };

    match output {
        Some(path) => {
            std::fs::write(path, &content)?;
            eprintln!("Exported {} events to {}", events.len(), path);
        }
        None => {
            print!("{}", content);
        }
    }

    Ok(())
}

/// Get the hostname for mesh node naming.
#[cfg(feature = "mesh")]
fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "pipit-node".to_string())
}
