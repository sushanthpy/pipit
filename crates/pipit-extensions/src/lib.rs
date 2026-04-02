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
    async fn on_before_request(&self, system_prompt: &str) -> Result<Option<String>, ExtensionError>;

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
    async fn on_tool_failure(
        &self,
        name: &str,
        error: &str,
    ) -> Result<(), ExtensionError>;

    /// Called when agent turn completes.
    async fn on_turn_end(&self, modified_files: &[String]) -> Result<(), ExtensionError>;

    /// Called after each agent response completes (Stop event).
    async fn on_stop(&self) -> Result<(), ExtensionError>;

    /// Called before context compaction. Save any state here.
    async fn on_pre_compact(&self) -> Result<(), ExtensionError>;

    /// Called when a new session starts.
    async fn on_session_start(&self) -> Result<(), ExtensionError>;

    /// Called when a session ends.
    async fn on_session_end(&self) -> Result<(), ExtensionError>;
}

/// No-op extension runner (when extensions are disabled).
pub struct NoopExtensionRunner;

#[async_trait]
impl ExtensionRunner for NoopExtensionRunner {
    async fn on_input(&self, _text: &str) -> Result<Option<String>, ExtensionError> { Ok(None) }
    async fn on_before_request(&self, _system_prompt: &str) -> Result<Option<String>, ExtensionError> { Ok(None) }
    async fn on_content_delta(&self, _text: &str) -> Result<(), ExtensionError> { Ok(()) }
    async fn on_before_tool(&self, _name: &str, _args: &Value) -> Result<Option<Value>, ExtensionError> { Ok(None) }
    async fn on_after_tool(&self, _name: &str, _result: &str) -> Result<Option<String>, ExtensionError> { Ok(None) }
    async fn on_tool_failure(&self, _name: &str, _error: &str) -> Result<(), ExtensionError> { Ok(()) }
    async fn on_turn_end(&self, _modified_files: &[String]) -> Result<(), ExtensionError> { Ok(()) }
    async fn on_stop(&self) -> Result<(), ExtensionError> { Ok(()) }
    async fn on_pre_compact(&self) -> Result<(), ExtensionError> { Ok(()) }
    async fn on_session_start(&self) -> Result<(), ExtensionError> { Ok(()) }
    async fn on_session_end(&self) -> Result<(), ExtensionError> { Ok(()) }
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

#[async_trait]
impl ExtensionRunner for HookExtensionRunner {
    async fn on_input(&self, _text: &str) -> Result<Option<String>, ExtensionError> {
        Ok(None)
    }

    async fn on_before_request(&self, _system_prompt: &str) -> Result<Option<String>, ExtensionError> {
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
        for hook in self.pre_tool_hooks.iter().filter(|h| matcher_matches(&h.matcher, name)) {
            run_pre_tool_hook(hook, &self.project_root, name, args).await?;
        }
        Ok(None)
    }

    async fn on_after_tool(
        &self,
        name: &str,
        result: &str,
    ) -> Result<Option<String>, ExtensionError> {
        for hook in self.post_tool_hooks.iter().filter(|h| matcher_matches(&h.matcher, name)) {
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

    async fn on_tool_failure(
        &self,
        name: &str,
        error: &str,
    ) -> Result<(), ExtensionError> {
        for hook in self.post_tool_failure_hooks.iter().filter(|h| matcher_matches(&h.matcher, name)) {
            run_lifecycle_hook(hook, &self.project_root, &[
                ("PIPIT_HOOK_EVENT", "PostToolUseFailure"),
                ("PIPIT_TOOL_NAME", name),
                ("PIPIT_TOOL_ERROR", error),
            ]).await?;
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
                    let _ = run_lifecycle_hook(&hook, &root, &[
                        ("PIPIT_HOOK_EVENT", "Stop"),
                    ]).await;
                });
            } else {
                run_lifecycle_hook(hook, &self.project_root, &[
                    ("PIPIT_HOOK_EVENT", "Stop"),
                ]).await?;
            }
        }
        Ok(())
    }

    async fn on_pre_compact(&self) -> Result<(), ExtensionError> {
        for hook in &self.pre_compact_hooks {
            run_lifecycle_hook(hook, &self.project_root, &[
                ("PIPIT_HOOK_EVENT", "PreCompact"),
            ]).await?;
        }
        Ok(())
    }

    async fn on_session_start(&self) -> Result<(), ExtensionError> {
        for hook in &self.session_start_hooks {
            run_lifecycle_hook(hook, &self.project_root, &[
                ("PIPIT_HOOK_EVENT", "SessionStart"),
            ]).await?;
        }
        Ok(())
    }

    async fn on_session_end(&self) -> Result<(), ExtensionError> {
        for hook in &self.session_end_hooks {
            if hook.script.async_hook {
                let hook = hook.clone();
                let root = self.project_root.clone();
                tokio::spawn(async move {
                    let _ = run_lifecycle_hook(&hook, &root, &[
                        ("PIPIT_HOOK_EVENT", "SessionEnd"),
                    ]).await;
                });
            } else {
                run_lifecycle_hook(hook, &self.project_root, &[
                    ("PIPIT_HOOK_EVENT", "SessionEnd"),
                ]).await?;
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
    if hook.script.kind != "command" {
        return Err(ExtensionError::Other(format!(
            "Unsupported hook script type '{}' for {}",
            hook.script.kind, hook.matcher
        )));
    }

    let mut command = tokio::process::Command::new("sh");
    command.arg("-c").arg(&hook.script.command);
    command.current_dir(project_root);
    command.env("PIPIT_HOOK_EVENT", &hook.event);
    command.env("PIPIT_TOOL_NAME", tool_name);
    command.env("PIPIT_TOOL_ARGS", serde_json::to_string(args).unwrap_or_default());
    command.env("PIPIT_PROJECT_ROOT", project_root.display().to_string());

    let output_future = command.output();
    let output = if let Some(timeout_secs) = hook.script.timeout {
        tokio::time::timeout(Duration::from_secs(timeout_secs), output_future)
            .await
            .map_err(|_| {
                ExtensionError::HookBlocked(format!(
                    "Hook '{}' timed out after {}s",
                    hook.matcher, timeout_secs
                ))
            })?
            .map_err(|err| ExtensionError::Other(format!("Failed to run hook: {}", err)))?
    } else {
        output_future
            .await
            .map_err(|err| ExtensionError::Other(format!("Failed to run hook: {}", err)))?
    };

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
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
        return Err(ExtensionError::Other(format!(
            "Unsupported hook script type '{}' for {}",
            hook.script.kind, hook.matcher
        )));
    }

    let mut command = tokio::process::Command::new("sh");
    command.arg("-c").arg(&hook.script.command);
    command.current_dir(project_root);
    command.env("PIPIT_HOOK_EVENT", &hook.event);
    command.env("PIPIT_TOOL_NAME", tool_name);
    command.env("PIPIT_TOOL_RESULT", result);
    command.env("PIPIT_PROJECT_ROOT", project_root.display().to_string());

    let output_future = command.output();
    let output = if let Some(timeout_secs) = hook.script.timeout {
        tokio::time::timeout(Duration::from_secs(timeout_secs), output_future)
            .await
            .map_err(|_| {
                ExtensionError::HookBlocked(format!(
                    "Hook '{}' timed out after {}s",
                    hook.matcher, timeout_secs
                ))
            })?
            .map_err(|err| ExtensionError::Other(format!("Failed to run hook: {}", err)))?
    } else {
        output_future
            .await
            .map_err(|err| ExtensionError::Other(format!("Failed to run hook: {}", err)))?
    };

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
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

/// Run a generic lifecycle hook (Stop, PreCompact, SessionStart, SessionEnd, PostToolUseFailure).
async fn run_lifecycle_hook(
    hook: &HookManifest,
    project_root: &Path,
    env_vars: &[(&str, &str)],
) -> Result<(), ExtensionError> {
    if hook.script.kind != "command" {
        return Err(ExtensionError::Other(format!(
            "Unsupported hook script type '{}' for {}",
            hook.script.kind, hook.event
        )));
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
            Ok(result) => result.map_err(|e| ExtensionError::Other(format!("Hook failed: {}", e)))?,
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
/// 2. Claude Code grouped format: `{"hooks": {"PreToolUse": [...], "PostToolUse": [...]}}`
fn load_hook_manifest(path: &Path) -> Result<HookManifest, ExtensionError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|err| ExtensionError::Other(format!("Failed to read hook {}: {}", path.display(), err)))?;

    // Try single-hook format first
    if let Ok(manifest) = serde_json::from_str::<HookManifest>(&raw) {
        return Ok(manifest);
    }

    Err(ExtensionError::Other(format!("Failed to parse hook {}", path.display())))
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
                        let matcher = entry.get("matcher")
                            .and_then(|m| m.as_str())
                            .unwrap_or("*")
                            .to_string();
                        let description = entry.get("description")
                            .and_then(|d| d.as_str())
                            .map(|s| s.to_string());

                        // Extract hook commands from the "hooks" array inside each entry
                        if let Some(hook_cmds) = entry.get("hooks").and_then(|h| h.as_array()) {
                            for hook_cmd in hook_cmds {
                                let command = hook_cmd.get("command")
                                    .and_then(|c| c.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let timeout = hook_cmd.get("timeout")
                                    .and_then(|t| t.as_u64());
                                let async_hook = hook_cmd.get("async")
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
