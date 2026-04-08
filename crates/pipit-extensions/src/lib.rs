pub mod event;
pub mod hook_kind;
pub mod prompt_runtime;
pub mod replay;
pub mod wasm_runtime;

pub use event::{HookEventMask, event_name_to_mask, events_to_mask};
pub use hook_kind::{HookContext, HookDecision, HookKind, ReplayMode, TypedHookManifest};
pub use prompt_runtime::{
    AgentHookConfig, AgentHookResult, HttpHookConfig, HttpHookResult, PromptHookConfig,
    PromptHookResult, execute_agent_hook, execute_http_hook, execute_prompt_hook,
};
pub use replay::{HookRecord, HookReplayCache, execute_with_replay};
pub use wasm_runtime::WasmHookRuntime;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExtensionError {
    #[error("Extension error: {0}")]
    Other(String),
    #[error("Hook blocked tool execution: {0}")]
    HookBlocked(String),
}

/// Hook points in the agent lifecycle — mirrors Code's hook events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hook {
    /// Before a tool executes. Can block (exit code 2) or warn.
    PreToolUse,
    /// After a tool executes successfully.
    PostToolUse,
    /// After a tool execution fails.
    PostToolUseFailure,
    /// Before context compaction. Save state here.
    PreCompact,
    /// After context compaction completes.
    PostCompact,
    /// After each agent response completes (the "Stop" event).
    Stop,
    /// When a new session starts.
    SessionStart,
    /// When a session ends.
    SessionEnd,
}

/// The extension runner trait — abstracts over hook-based extensions.
#[async_trait]
pub trait ExtensionRunner: Send + Sync {
    /// Called when user input is received.
    async fn on_input(&self, text: &str) -> Result<Option<String>, ExtensionError>;

    /// Called before building the LLM request.
    async fn on_before_request(
        &self,
        system_prompt: &str,
    ) -> Result<Option<String>, ExtensionError>;

    /// Called when content arrives from LLM.
    async fn on_content_delta(&self, text: &str) -> Result<(), ExtensionError>;

    /// Called before a tool executes. Return modified args or None.
    async fn on_before_tool(
        &self,
        name: &str,
        args: &Value,
    ) -> Result<Option<Value>, ExtensionError>;

    /// Called after a tool executes successfully.
    async fn on_after_tool(
        &self,
        name: &str,
        result: &str,
    ) -> Result<Option<String>, ExtensionError>;

    /// Called after a tool execution fails.
    async fn on_tool_failure(&self, name: &str, error: &str) -> Result<(), ExtensionError>;

    /// Called when agent turn completes.
    async fn on_turn_end(&self, modified_files: &[String]) -> Result<(), ExtensionError>;

    /// Called after each agent response completes (Stop event).
    async fn on_stop(&self) -> Result<(), ExtensionError>;

    /// Called before context compaction. Save any state here.
    async fn on_pre_compact(&self) -> Result<(), ExtensionError>;

    /// Called after context compaction completes.
    async fn on_post_compact(
        &self,
        messages_removed: usize,
        tokens_freed: u64,
    ) -> Result<(), ExtensionError>;

    /// Called when a new session starts.
    async fn on_session_start(&self) -> Result<(), ExtensionError>;

    /// Called when a session ends.
    async fn on_session_end(&self) -> Result<(), ExtensionError>;
}

/// No-op extension runner (when extensions are disabled).
pub struct NoopExtensionRunner;

#[async_trait]
impl ExtensionRunner for NoopExtensionRunner {
    async fn on_input(&self, _text: &str) -> Result<Option<String>, ExtensionError> {
        Ok(None)
    }
    async fn on_before_request(
        &self,
        _system_prompt: &str,
    ) -> Result<Option<String>, ExtensionError> {
        Ok(None)
    }
    async fn on_content_delta(&self, _text: &str) -> Result<(), ExtensionError> {
        Ok(())
    }
    async fn on_before_tool(
        &self,
        _name: &str,
        _args: &Value,
    ) -> Result<Option<Value>, ExtensionError> {
        Ok(None)
    }
    async fn on_after_tool(
        &self,
        _name: &str,
        _result: &str,
    ) -> Result<Option<String>, ExtensionError> {
        Ok(None)
    }
    async fn on_tool_failure(&self, _name: &str, _error: &str) -> Result<(), ExtensionError> {
        Ok(())
    }
    async fn on_turn_end(&self, _modified_files: &[String]) -> Result<(), ExtensionError> {
        Ok(())
    }
    async fn on_stop(&self) -> Result<(), ExtensionError> {
        Ok(())
    }
    async fn on_pre_compact(&self) -> Result<(), ExtensionError> {
        Ok(())
    }
    async fn on_post_compact(
        &self,
        _messages_removed: usize,
        _tokens_freed: u64,
    ) -> Result<(), ExtensionError> {
        Ok(())
    }
    async fn on_session_start(&self) -> Result<(), ExtensionError> {
        Ok(())
    }
    async fn on_session_end(&self) -> Result<(), ExtensionError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct HookManifest {
    event: String,
    matcher: String,
    description: Option<String>,
    script: HookScript,
}

#[derive(Debug, Clone, Deserialize)]
struct HookScript {
    #[serde(rename = "type")]
    kind: String,
    command: String,
    timeout: Option<u64>,
    /// If true, run in background without blocking.
    #[serde(default)]
    async_hook: bool,
}

#[derive(Debug, Clone)]
pub struct HookExtensionRunner {
    project_root: PathBuf,
    pre_tool_hooks: Vec<Arc<HookManifest>>,
    post_tool_hooks: Vec<Arc<HookManifest>>,
    post_tool_failure_hooks: Vec<Arc<HookManifest>>,
    pre_compact_hooks: Vec<Arc<HookManifest>>,
    post_compact_hooks: Vec<Arc<HookManifest>>,
    stop_hooks: Vec<Arc<HookManifest>>,
    session_start_hooks: Vec<Arc<HookManifest>>,
    session_end_hooks: Vec<Arc<HookManifest>>,
}

impl HookExtensionRunner {
    pub fn from_hook_files(project_root: PathBuf, hook_files: &[PathBuf]) -> Self {
        let mut all_manifests: Vec<Arc<HookManifest>> = Vec::new();
        for path in hook_files {
            // Try single-manifest format first, then grouped format
            if let Ok(manifest) = load_hook_manifest(path) {
                all_manifests.push(Arc::new(manifest));
            } else {
                for m in load_all_hook_manifests(path) {
                    all_manifests.push(Arc::new(m));
                }
            }
        }

        let by_event = |event: &str| -> Vec<Arc<HookManifest>> {
            all_manifests
                .iter()
                .filter(|m| m.event == event)
                .cloned()
                .collect()
        };

        Self {
            project_root,
            pre_tool_hooks: by_event("PreToolUse"),
            post_tool_hooks: by_event("PostToolUse"),
            post_tool_failure_hooks: by_event("PostToolUseFailure"),
            pre_compact_hooks: by_event("PreCompact"),
            post_compact_hooks: by_event("PostCompact"),
            stop_hooks: by_event("Stop"),
            session_start_hooks: by_event("SessionStart"),
            session_end_hooks: by_event("SessionEnd"),
        }
    }
}

/// Check if a hook's matcher matches a given tool name.
/// Supports: exact match, wildcard `*`, and `|`-separated alternatives (e.g. `Edit|Write`).
fn matcher_matches(matcher: &str, tool_name: &str) -> bool {
    if matcher == "*" {
        return true;
    }
    matcher.split('|').any(|m| m.trim() == tool_name)
}

/// Set environment variables used by hooks from other AI coding tools
/// so their scripts can find resources relative to the project root.
fn set_cross_tool_env(cmd: &mut tokio::process::Command, project_root: &Path) {
    let root = project_root.display().to_string();
    // Claude Code: hooks reference scripts via ${CLAUDE_PLUGIN_ROOT}
    cmd.env("CLAUDE_PLUGIN_ROOT", &root);
    // Cursor: uses CURSOR_PLUGIN_ROOT
    cmd.env("CURSOR_PLUGIN_ROOT", &root);
    // Codex: uses CODEX_PLUGIN_ROOT
    cmd.env("CODEX_PLUGIN_ROOT", &root);
    // General: some hooks use PROJECT_ROOT
    cmd.env("PROJECT_ROOT", &root);
}

#[async_trait]
impl ExtensionRunner for HookExtensionRunner {
    async fn on_input(&self, _text: &str) -> Result<Option<String>, ExtensionError> {
        Ok(None)
    }

    async fn on_before_request(
        &self,
        _system_prompt: &str,
    ) -> Result<Option<String>, ExtensionError> {
        Ok(None)
    }

    async fn on_content_delta(&self, _text: &str) -> Result<(), ExtensionError> {
        Ok(())
    }

    async fn on_before_tool(
        &self,
        name: &str,
        args: &Value,
    ) -> Result<Option<Value>, ExtensionError> {
        for hook in self
            .pre_tool_hooks
            .iter()
            .filter(|h| matcher_matches(&h.matcher, name))
        {
            run_pre_tool_hook(hook, &self.project_root, name, args).await?;
        }
        Ok(None)
    }

    async fn on_after_tool(
        &self,
        name: &str,
        result: &str,
    ) -> Result<Option<String>, ExtensionError> {
        for hook in self
            .post_tool_hooks
            .iter()
            .filter(|h| matcher_matches(&h.matcher, name))
        {
            if hook.script.async_hook {
                let hook = hook.clone();
                let root = self.project_root.clone();
                let name = name.to_string();
                let result = result.to_string();
                tokio::spawn(async move {
                    let _ = run_post_tool_hook(&hook, &root, &name, &result).await;
                });
            } else {
                run_post_tool_hook(hook, &self.project_root, name, result).await?;
            }
        }
        Ok(None)
    }

    async fn on_tool_failure(&self, name: &str, error: &str) -> Result<(), ExtensionError> {
        for hook in self
            .post_tool_failure_hooks
            .iter()
            .filter(|h| matcher_matches(&h.matcher, name))
        {
            run_lifecycle_hook(
                hook,
                &self.project_root,
                &[
                    ("PIPIT_HOOK_EVENT", "PostToolUseFailure"),
                    ("PIPIT_TOOL_NAME", name),
                    ("PIPIT_TOOL_ERROR", error),
                ],
            )
            .await?;
        }
        Ok(())
    }

    async fn on_turn_end(&self, _modified_files: &[String]) -> Result<(), ExtensionError> {
        Ok(())
    }

    async fn on_stop(&self) -> Result<(), ExtensionError> {
        for hook in &self.stop_hooks {
            if hook.script.async_hook {
                let hook = hook.clone();
                let root = self.project_root.clone();
                tokio::spawn(async move {
                    let _ = run_lifecycle_hook(&hook, &root, &[("PIPIT_HOOK_EVENT", "Stop")]).await;
                });
            } else {
                run_lifecycle_hook(hook, &self.project_root, &[("PIPIT_HOOK_EVENT", "Stop")])
                    .await?;
            }
        }
        Ok(())
    }

    async fn on_pre_compact(&self) -> Result<(), ExtensionError> {
        for hook in &self.pre_compact_hooks {
            run_lifecycle_hook(
                hook,
                &self.project_root,
                &[("PIPIT_HOOK_EVENT", "PreCompact")],
            )
            .await?;
        }
        Ok(())
    }

    async fn on_post_compact(
        &self,
        messages_removed: usize,
        tokens_freed: u64,
    ) -> Result<(), ExtensionError> {
        for hook in &self.post_compact_hooks {
            run_lifecycle_hook(
                hook,
                &self.project_root,
                &[
                    ("PIPIT_HOOK_EVENT", "PostCompact"),
                    ("PIPIT_MESSAGES_REMOVED", &messages_removed.to_string()),
                    ("PIPIT_TOKENS_FREED", &tokens_freed.to_string()),
                ],
            )
            .await?;
        }
        Ok(())
    }

    async fn on_session_start(&self) -> Result<(), ExtensionError> {
        for hook in &self.session_start_hooks {
            run_lifecycle_hook(
                hook,
                &self.project_root,
                &[("PIPIT_HOOK_EVENT", "SessionStart")],
            )
            .await?;
        }
        Ok(())
    }

    async fn on_session_end(&self) -> Result<(), ExtensionError> {
        for hook in &self.session_end_hooks {
            if hook.script.async_hook {
                let hook = hook.clone();
                let root = self.project_root.clone();
                tokio::spawn(async move {
                    let _ = run_lifecycle_hook(&hook, &root, &[("PIPIT_HOOK_EVENT", "SessionEnd")])
                        .await;
                });
            } else {
                run_lifecycle_hook(
                    hook,
                    &self.project_root,
                    &[("PIPIT_HOOK_EVENT", "SessionEnd")],
                )
                .await?;
            }
        }
        Ok(())
    }
}

async fn run_pre_tool_hook(
    hook: &HookManifest,
    project_root: &Path,
    tool_name: &str,
    args: &Value,
) -> Result<(), ExtensionError> {
    // Dispatch based on hook script kind — supports all HookKind variants
    match hook.script.kind.as_str() {
        "command" => run_command_pre_tool_hook(hook, project_root, tool_name, args).await,
        "prompt" => {
            let config = PromptHookConfig {
                system: hook.script.command.clone(),
                model: None,
                provider: None,
                max_tokens: 256,
                timeout_ms: hook.script.timeout.unwrap_or(30) * 1000,
            };
            let ctx = hook_kind::HookContext {
                event: hook.event.clone(),
                tool_name: Some(tool_name.to_string()),
                tool_args: Some(args.clone()),
                tool_result: None,
                project_root: project_root.to_path_buf(),
                session_id: String::new(),
                replay_mode: hook_kind::ReplayMode::Live,
            };
            let cancel = tokio_util::sync::CancellationToken::new();
            match execute_prompt_hook(&config, &ctx, cancel).await {
                Ok(result) if !result.decision.allow => Err(ExtensionError::HookBlocked(
                    result
                        .decision
                        .message
                        .unwrap_or_else(|| "Prompt hook denied".into()),
                )),
                Ok(_) => Ok(()),
                Err(e) => {
                    tracing::warn!("Prompt hook error (allowing): {}", e);
                    Ok(())
                }
            }
        }
        "http" => {
            let config = prompt_runtime::HttpHookConfig {
                url: hook.script.command.clone(),
                headers: std::collections::HashMap::new(),
                method: "POST".into(),
                timeout_ms: hook.script.timeout.unwrap_or(30) * 1000,
            };
            let ctx = hook_kind::HookContext {
                event: hook.event.clone(),
                tool_name: Some(tool_name.to_string()),
                tool_args: Some(args.clone()),
                tool_result: None,
                project_root: project_root.to_path_buf(),
                session_id: String::new(),
                replay_mode: hook_kind::ReplayMode::Live,
            };
            let cancel = tokio_util::sync::CancellationToken::new();
            match execute_http_hook(&config, &ctx, cancel).await {
                Ok(result) if !result.decision.allow => Err(ExtensionError::HookBlocked(
                    result
                        .decision
                        .message
                        .unwrap_or_else(|| "HTTP hook denied".into()),
                )),
                Ok(_) => Ok(()),
                Err(e) => {
                    tracing::warn!("HTTP hook error (allowing): {}", e);
                    Ok(())
                }
            }
        }
        "agent" => {
            let config = AgentHookConfig {
                task: hook.script.command.clone(),
                allowed_tools: vec![],
                max_turns: 1,
                cost_fraction: 0.1,
                timeout_ms: hook.script.timeout.unwrap_or(60) * 1000,
            };
            let ctx = hook_kind::HookContext {
                event: hook.event.clone(),
                tool_name: Some(tool_name.to_string()),
                tool_args: Some(args.clone()),
                tool_result: None,
                project_root: project_root.to_path_buf(),
                session_id: String::new(),
                replay_mode: hook_kind::ReplayMode::Live,
            };
            let cancel = tokio_util::sync::CancellationToken::new();
            match execute_agent_hook(&config, &ctx, cancel).await {
                Ok(result) if !result.decision.allow => Err(ExtensionError::HookBlocked(
                    result
                        .decision
                        .message
                        .unwrap_or_else(|| "Agent hook denied".into()),
                )),
                Ok(_) => Ok(()),
                Err(e) => {
                    tracing::warn!("Agent hook error (allowing): {}", e);
                    Ok(())
                }
            }
        }
        "wasm" => {
            // WASM hooks dispatched via WasmHookRuntime
            let ctx = hook_kind::HookContext {
                event: hook.event.clone(),
                tool_name: Some(tool_name.to_string()),
                tool_args: Some(args.clone()),
                tool_result: None,
                project_root: project_root.to_path_buf(),
                session_id: String::new(),
                replay_mode: hook_kind::ReplayMode::Live,
            };
            match WasmHookRuntime::new() {
                Ok(runtime) => {
                    let module_path = project_root.join(&hook.script.command);
                    match runtime.load_module(&module_path, None) {
                        Ok((module, _hash)) => {
                            let input_json = serde_json::to_string(&ctx).unwrap_or_default();
                            match runtime.execute(
                                &module,
                                &input_json,
                                10_000_000,
                                16 * 1024 * 1024,
                            ) {
                                Ok(decision) if !decision.allow => {
                                    Err(ExtensionError::HookBlocked(
                                        decision
                                            .message
                                            .unwrap_or_else(|| "WASM hook denied".into()),
                                    ))
                                }
                                Ok(_) => Ok(()),
                                Err(e) => {
                                    tracing::warn!("WASM hook error (allowing): {}", e);
                                    Ok(())
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("WASM module load error (allowing): {}", e);
                            Ok(())
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("WASM runtime init error (allowing): {}", e);
                    Ok(())
                }
            }
        }
        other => Err(ExtensionError::Other(format!(
            "Unsupported hook script type '{}' for {}",
            other, hook.matcher
        ))),
    }
}

/// Command-specific pre-tool hook execution (original implementation).
async fn run_command_pre_tool_hook(
    hook: &HookManifest,
    project_root: &Path,
    tool_name: &str,
    args: &Value,
) -> Result<(), ExtensionError> {
    let mut command = tokio::process::Command::new("sh");
    command.arg("-c").arg(&hook.script.command);
    command.current_dir(project_root);
    command.env("PIPIT_HOOK_EVENT", &hook.event);
    command.env("PIPIT_TOOL_NAME", tool_name);
    command.env(
        "PIPIT_TOOL_ARGS",
        serde_json::to_string(args).unwrap_or_default(),
    );
    command.env("PIPIT_PROJECT_ROOT", project_root.display().to_string());
    // Cross-tool compatibility: set env vars used by hooks from other AI tools
    // so their script paths resolve correctly.
    set_cross_tool_env(&mut command, project_root);

    let output_future = command.output();
    let output = if let Some(timeout_secs) = hook.script.timeout {
        match tokio::time::timeout(Duration::from_secs(timeout_secs), output_future).await {
            Ok(Ok(output)) => output,
            Ok(Err(err)) => {
                return Err(ExtensionError::Other(format!(
                    "Failed to run pre-tool hook: {}",
                    err
                )));
            }
            Err(_) => {
                tracing::warn!(hook = %hook.matcher, timeout_secs, "Pre-tool hook timed out — skipping");
                return Ok(());
            }
        }
    } else {
        output_future
            .await
            .map_err(|err| ExtensionError::Other(format!("Failed to run hook: {}", err)))?
    };

    if output.status.success() {
        return Ok(());
    }

    let exit_code = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Exit codes 127 (not found) and 126 (not executable) mean the hook script
    // is missing. This commonly happens with cross-tool hooks (e.g., Claude Code's
    // ECC hooks reference scripts via ${CLAUDE_PLUGIN_ROOT} that don't exist for
    // pipit). Warn and skip rather than blocking all tool execution.
    //
    // Also detect Node.js MODULE_NOT_FOUND errors (exit code 1 with specific
    // error text) which happen when hook scripts reference missing npm packages.
    if exit_code == 127 || exit_code == 126 {
        tracing::warn!(
            hook = %hook.matcher,
            exit_code,
            stderr = %stderr,
            "Pre-tool hook script not found — skipping (cross-tool compatibility)"
        );
        return Ok(());
    }
    if exit_code == 1
        && (stderr.contains("Cannot find module")
            || stderr.contains("MODULE_NOT_FOUND")
            || stderr.contains("ENOENT"))
    {
        tracing::warn!(
            hook = %hook.matcher,
            stderr = %stderr,
            "Pre-tool hook has missing dependency — skipping (cross-tool compatibility)"
        );
        return Ok(());
    }

    let details = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else if let Some(description) = &hook.description {
        description.clone()
    } else {
        format!("Hook blocked {}", tool_name)
    };

    Err(ExtensionError::HookBlocked(details))
}

async fn run_post_tool_hook(
    hook: &HookManifest,
    project_root: &Path,
    tool_name: &str,
    result: &str,
) -> Result<(), ExtensionError> {
    if hook.script.kind != "command" {
        // For non-command hooks, dispatch through prompt/http/agent/wasm
        let ctx = hook_kind::HookContext {
            event: hook.event.clone(),
            tool_name: Some(tool_name.to_string()),
            tool_args: None,
            tool_result: Some(result.to_string()),
            project_root: project_root.to_path_buf(),
            session_id: String::new(),
            replay_mode: hook_kind::ReplayMode::Live,
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        return match hook.script.kind.as_str() {
            "prompt" => {
                let config = PromptHookConfig {
                    system: hook.script.command.clone(),
                    model: None,
                    provider: None,
                    max_tokens: 256,
                    timeout_ms: hook.script.timeout.unwrap_or(30) * 1000,
                };
                execute_prompt_hook(&config, &ctx, cancel)
                    .await
                    .map(|_| ())
                    .map_err(|e| ExtensionError::Other(e))
            }
            "http" => {
                let config = prompt_runtime::HttpHookConfig {
                    url: hook.script.command.clone(),
                    headers: std::collections::HashMap::new(),
                    method: "POST".into(),
                    timeout_ms: hook.script.timeout.unwrap_or(30) * 1000,
                };
                execute_http_hook(&config, &ctx, cancel)
                    .await
                    .map(|_| ())
                    .map_err(|e| ExtensionError::Other(e))
            }
            other => Err(ExtensionError::Other(format!(
                "Unsupported hook script type '{}' for {}",
                other, hook.matcher
            ))),
        };
    }

    let mut command = tokio::process::Command::new("sh");
    command.arg("-c").arg(&hook.script.command);
    command.current_dir(project_root);
    command.env("PIPIT_HOOK_EVENT", &hook.event);
    command.env("PIPIT_TOOL_NAME", tool_name);
    command.env("PIPIT_TOOL_RESULT", result);
    command.env("PIPIT_PROJECT_ROOT", project_root.display().to_string());
    set_cross_tool_env(&mut command, project_root);

    let output_future = command.output();
    let output = if let Some(timeout_secs) = hook.script.timeout {
        match tokio::time::timeout(Duration::from_secs(timeout_secs), output_future).await {
            Ok(Ok(output)) => output,
            Ok(Err(err)) => {
                return Err(ExtensionError::Other(format!(
                    "Failed to run post-tool hook: {}",
                    err
                )));
            }
            Err(_) => {
                tracing::warn!(hook = %hook.matcher, timeout_secs, "Post-tool hook timed out — skipping");
                return Ok(());
            }
        }
    } else {
        output_future
            .await
            .map_err(|err| ExtensionError::Other(format!("Failed to run hook: {}", err)))?
    };

    if output.status.success() {
        return Ok(());
    }

    let exit_code = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Same cross-tool compatibility as pre-tool hooks: missing scripts (127/126)
    // are warned and skipped, not treated as failures.
    if exit_code == 127 || exit_code == 126 {
        tracing::warn!(
            hook = %hook.matcher,
            exit_code,
            stderr = %stderr,
            "Post-tool hook script not found — skipping (cross-tool compatibility)"
        );
        return Ok(());
    }
    if exit_code == 1
        && (stderr.contains("Cannot find module")
            || stderr.contains("MODULE_NOT_FOUND")
            || stderr.contains("ENOENT"))
    {
        tracing::warn!(
            hook = %hook.matcher,
            stderr = %stderr,
            "Post-tool hook has missing dependency — skipping (cross-tool compatibility)"
        );
        return Ok(());
    }

    let details = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else if let Some(description) = &hook.description {
        description.clone()
    } else {
        format!("Hook blocked {}", tool_name)
    };

    Err(ExtensionError::HookBlocked(details))
}

/// Run a generic lifecycle hook (Stop, PreCompact, PostCompact, SessionStart, SessionEnd, PostToolUseFailure).
/// Supports all HookKind variants: command, prompt, http, agent, wasm.
async fn run_lifecycle_hook(
    hook: &HookManifest,
    project_root: &Path,
    env_vars: &[(&str, &str)],
) -> Result<(), ExtensionError> {
    if hook.script.kind != "command" {
        // Non-command lifecycle hooks: dispatch through the appropriate runtime
        let ctx = hook_kind::HookContext {
            event: hook.event.clone(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            project_root: project_root.to_path_buf(),
            session_id: String::new(),
            replay_mode: hook_kind::ReplayMode::Live,
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        return match hook.script.kind.as_str() {
            "prompt" => {
                let config = PromptHookConfig {
                    system: hook.script.command.clone(),
                    model: None,
                    provider: None,
                    max_tokens: 256,
                    timeout_ms: hook.script.timeout.unwrap_or(30) * 1000,
                };
                execute_prompt_hook(&config, &ctx, cancel)
                    .await
                    .map(|_| ())
                    .map_err(|e| ExtensionError::Other(e))
            }
            "http" => {
                let config = prompt_runtime::HttpHookConfig {
                    url: hook.script.command.clone(),
                    headers: std::collections::HashMap::new(),
                    method: "POST".into(),
                    timeout_ms: hook.script.timeout.unwrap_or(30) * 1000,
                };
                execute_http_hook(&config, &ctx, cancel)
                    .await
                    .map(|_| ())
                    .map_err(|e| ExtensionError::Other(e))
            }
            "agent" => {
                let config = AgentHookConfig {
                    task: hook.script.command.clone(),
                    allowed_tools: vec![],
                    max_turns: 1,
                    cost_fraction: 0.1,
                    timeout_ms: hook.script.timeout.unwrap_or(60) * 1000,
                };
                execute_agent_hook(&config, &ctx, cancel)
                    .await
                    .map(|_| ())
                    .map_err(|e| ExtensionError::Other(e))
            }
            other => {
                tracing::warn!(
                    "Unsupported lifecycle hook type '{}' for {}",
                    other,
                    hook.event
                );
                Ok(())
            }
        };
    }

    let mut command = tokio::process::Command::new("sh");
    command.arg("-c").arg(&hook.script.command);
    command.current_dir(project_root);
    command.env("PIPIT_PROJECT_ROOT", project_root.display().to_string());
    for (key, val) in env_vars {
        command.env(key, val);
    }

    let output_future = command.output();
    let output = if let Some(timeout_secs) = hook.script.timeout {
        match tokio::time::timeout(Duration::from_secs(timeout_secs), output_future).await {
            Ok(result) => {
                result.map_err(|e| ExtensionError::Other(format!("Hook failed: {}", e)))?
            }
            Err(_) => return Ok(()), // Lifecycle hooks timeout silently
        }
    } else {
        output_future
            .await
            .map_err(|e| ExtensionError::Other(format!("Hook failed: {}", e)))?
    };

    // Lifecycle hooks don't block on failure — they just log
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if !stderr.is_empty() {
            tracing::warn!("{} hook stderr: {}", hook.event, stderr);
        }
    }

    Ok(())
}

/// Load hook manifests from a JSON file.
/// Supports two formats:
/// 1. Single-hook format: `{"event": "PreToolUse", "matcher": "bash", "script": {...}}`
/// 2. Grouped format: `{"hooks": {"PreToolUse": [...], "PostToolUse": [...]}}`
fn load_hook_manifest(path: &Path) -> Result<HookManifest, ExtensionError> {
    let raw = std::fs::read_to_string(path).map_err(|err| {
        ExtensionError::Other(format!("Failed to read hook {}: {}", path.display(), err))
    })?;

    // Try single-hook format first
    if let Ok(manifest) = serde_json::from_str::<HookManifest>(&raw) {
        return Ok(manifest);
    }

    Err(ExtensionError::Other(format!(
        "Failed to parse hook {}",
        path.display()
    )))
}

/// Load ALL hook manifests from a file, supporting the Code grouped format.
pub(crate) fn load_all_hook_manifests(path: &Path) -> Vec<HookManifest> {
    let raw = match std::fs::read_to_string(path) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    // Try single-hook format
    if let Ok(manifest) = serde_json::from_str::<HookManifest>(&raw) {
        return vec![manifest];
    }

    // Try Code grouped format: {"hooks": {"PreToolUse": [{...}], ...}}
    if let Ok(grouped) = serde_json::from_str::<serde_json::Value>(&raw) {
        if let Some(hooks_obj) = grouped.get("hooks").and_then(|h| h.as_object()) {
            let mut manifests = Vec::new();
            for (event_name, entries) in hooks_obj {
                if let Some(arr) = entries.as_array() {
                    for entry in arr {
                        let matcher = entry
                            .get("matcher")
                            .and_then(|m| m.as_str())
                            .unwrap_or("*")
                            .to_string();
                        let description = entry
                            .get("description")
                            .and_then(|d| d.as_str())
                            .map(|s| s.to_string());

                        // Extract hook commands from the "hooks" array inside each entry
                        if let Some(hook_cmds) = entry.get("hooks").and_then(|h| h.as_array()) {
                            for hook_cmd in hook_cmds {
                                let command = hook_cmd
                                    .get("command")
                                    .and_then(|c| c.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let timeout = hook_cmd.get("timeout").and_then(|t| t.as_u64());
                                let async_hook = hook_cmd
                                    .get("async")
                                    .and_then(|a| a.as_bool())
                                    .unwrap_or(false);

                                if !command.is_empty() {
                                    manifests.push(HookManifest {
                                        event: event_name.clone(),
                                        matcher: matcher.clone(),
                                        description: description.clone(),
                                        script: HookScript {
                                            kind: "command".to_string(),
                                            command,
                                            timeout,
                                            async_hook,
                                        },
                                    });
                                }
                            }
                        }
                    }
                }
            }
            return manifests;
        }
    }

    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn blocks_bash_when_hook_returns_non_zero() {
        let temp = tempdir().unwrap();
        let hook_path = temp.path().join("hook.json");
        std::fs::write(
            &hook_path,
            r#"{
  "event": "PreToolUse",
  "matcher": "bash",
  "script": {
    "type": "command",
    "command": "exit 7",
    "timeout": 1
  }
}"#,
        )
        .unwrap();

        let runner = HookExtensionRunner::from_hook_files(temp.path().to_path_buf(), &[hook_path]);
        let result = runner
            .on_before_tool("bash", &serde_json::json!({"command": "echo hi"}))
            .await;

        assert!(matches!(result, Err(ExtensionError::HookBlocked(_))));
    }

    #[tokio::test]
    async fn blocks_edit_file_when_pre_tool_hook_matches_tool_name() {
        let temp = tempdir().unwrap();
        let hook_path = temp.path().join("hook.json");
        std::fs::write(
            &hook_path,
            r#"{
  "event": "PreToolUse",
  "matcher": "edit_file",
  "script": {
    "type": "command",
    "command": "exit 4",
    "timeout": 1
  }
}"#,
        )
        .unwrap();

        let runner = HookExtensionRunner::from_hook_files(temp.path().to_path_buf(), &[hook_path]);
        let result = runner
            .on_before_tool("edit_file", &serde_json::json!({"path": "x.txt"}))
            .await;

        assert!(matches!(result, Err(ExtensionError::HookBlocked(_))));
    }

    #[tokio::test]
    async fn blocks_bash_when_post_tool_hook_returns_non_zero() {
        let temp = tempdir().unwrap();
        let hook_path = temp.path().join("hook.json");
        std::fs::write(
            &hook_path,
            r#"{
  "event": "PostToolUse",
  "matcher": "bash",
  "script": {
    "type": "command",
    "command": "exit 5",
    "timeout": 1
  }
}"#,
        )
        .unwrap();

        let runner = HookExtensionRunner::from_hook_files(temp.path().to_path_buf(), &[hook_path]);
        let result = runner.on_after_tool("bash", "hello").await;

        assert!(matches!(result, Err(ExtensionError::HookBlocked(_))));
    }

    #[tokio::test]
    async fn blocks_edit_file_when_post_tool_hook_matches_tool_name() {
        let temp = tempdir().unwrap();
        let hook_path = temp.path().join("hook.json");
        std::fs::write(
            &hook_path,
            r#"{
  "event": "PostToolUse",
  "matcher": "edit_file",
  "script": {
    "type": "command",
    "command": "exit 6",
    "timeout": 1
  }
}"#,
        )
        .unwrap();

        let runner = HookExtensionRunner::from_hook_files(temp.path().to_path_buf(), &[hook_path]);
        let result = runner.on_after_tool("edit_file", "patched file").await;

        assert!(matches!(result, Err(ExtensionError::HookBlocked(_))));
    }
}
