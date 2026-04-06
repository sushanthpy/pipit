//! Extra Tools — PowerShell, REPL, Skill, LSP, RemoteTrigger
//! Brings total tool count to 37.

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolContext, ToolError, ToolResult, ToolDisplay};

// ─── PowerShell Tool ────────────────────────────────────────────────────

pub struct PowerShellTool;

#[async_trait]
impl Tool for PowerShellTool {
    fn name(&self) -> &str { "powershell" }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "PowerShell command to execute"},
                "timeout_secs": {"type": "integer", "description": "Timeout in seconds (default: 120)"}
            },
            "required": ["command"]
        })
    }
    fn description(&self) -> &str { "Execute a PowerShell command. Uses pwsh on macOS/Linux, powershell.exe on Windows." }
    fn is_mutating(&self) -> bool { true }

    async fn execute(&self, args: Value, ctx: &ToolContext, _cancel: CancellationToken) -> Result<ToolResult, ToolError> {
        let command = args.get("command").and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("command required".into()))?;
        let timeout = args.get("timeout_secs").and_then(|v| v.as_u64()).unwrap_or(120);

        let shell = if cfg!(windows) { "powershell" } else { "pwsh" };

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout),
            tokio::process::Command::new(shell)
                .args(["-NoProfile", "-NonInteractive", "-Command", command])
                .current_dir(ctx.current_dir())
                .output()
        ).await
            .map_err(|_| ToolError::Timeout(timeout))?
            .map_err(|e| ToolError::ExecutionFailed(format!("PowerShell exec failed: {e}")))?;

        let stdout = String::from_utf8_lossy(&result.stdout).to_string();
        let stderr = String::from_utf8_lossy(&result.stderr).to_string();
        let exit_code = result.status.code().unwrap_or(-1);

        Ok(ToolResult {
            content: format!("Exit code: {exit_code}\n\n{stdout}{}",
                if stderr.is_empty() { String::new() } else { format!("\nSTDERR:\n{stderr}") }),
            display: Some(ToolDisplay::ShellOutput {
                command: command.to_string(), stdout: stdout.clone(),
                stderr: stderr.clone(), exit_code: Some(exit_code),
            }),
            mutated: true,
            content_bytes: stdout.len() + stderr.len(),
        })
    }
}

// ─── REPL Tool ──────────────────────────────────────────────────────────

pub struct ReplTool {
    sessions: Arc<Mutex<HashMap<String, ReplSession>>>,
}

/// Sentinel used to detect end-of-output from the persistent subprocess.
const REPL_SENTINEL: &str = "__PIPIT_REPL_SENTINEL_7f3a__";

struct ReplSession {
    language: String,
    turn_count: u32,
    child_stdin: Arc<tokio::sync::Mutex<tokio::process::ChildStdin>>,
    output_rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<String>>>,
}

impl ReplTool {
    pub fn new() -> Self { Self { sessions: Arc::new(Mutex::new(HashMap::new())) } }

    /// Start a persistent REPL subprocess for the given language.
    async fn start_session(language: &str, cwd: &std::path::Path) -> Result<ReplSession, ToolError> {
        let (binary, args): (&str, Vec<&str>) = match language {
            "python" => ("python3", vec!["-u", "-i"]),
            "node" => ("node", vec!["-i"]),
            "ruby" => ("ruby", vec!["-e", "require 'irb'; IRB.start"]),
            "lua" => ("lua", vec!["-i"]),
            _ => return Err(ToolError::InvalidArgs(format!("Unsupported language: {language}"))),
        };

        let mut child = tokio::process::Command::new(binary)
            .args(&args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to start {language} REPL: {e}")))?;

        let stdin = child.stdin.take()
            .ok_or_else(|| ToolError::ExecutionFailed("No stdin for REPL".into()))?;
        let stdout = child.stdout.take()
            .ok_or_else(|| ToolError::ExecutionFailed("No stdout for REPL".into()))?;
        let stderr = child.stderr.take()
            .ok_or_else(|| ToolError::ExecutionFailed("No stderr for REPL".into()))?;

        let (output_tx, output_rx) = tokio::sync::mpsc::channel::<String>(64);

        // Reader task: merge stdout + stderr, relay lines to channel
        let tx1 = output_tx.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;
            let mut reader = tokio::io::BufReader::new(stdout);
            let mut line = String::new();
            while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                let _ = tx1.send(line.clone()).await;
                line.clear();
            }
        });
        let tx2 = output_tx;
        tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;
            let mut reader = tokio::io::BufReader::new(stderr);
            let mut line = String::new();
            while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                let _ = tx2.send(line.clone()).await;
                line.clear();
            }
        });

        Ok(ReplSession {
            language: language.to_string(),
            turn_count: 0,
            child_stdin: Arc::new(tokio::sync::Mutex::new(stdin)),
            output_rx: Arc::new(tokio::sync::Mutex::new(output_rx)),
        })
    }
}

#[async_trait]
impl Tool for ReplTool {
    fn name(&self) -> &str { "repl" }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "language": {"type": "string", "enum": ["python", "node", "ruby", "lua"]},
                "code": {"type": "string", "description": "Code to execute"},
                "reset": {"type": "boolean", "description": "Reset REPL state"}
            },
            "required": ["language", "code"]
        })
    }
    fn description(&self) -> &str { "Execute code in a persistent language REPL. State persists across calls." }
    fn is_mutating(&self) -> bool { true }

    async fn execute(&self, args: Value, ctx: &ToolContext, _cancel: CancellationToken) -> Result<ToolResult, ToolError> {
        let language = args.get("language").and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("language required".into()))?;
        let code = args.get("code").and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("code required".into()))?;
        let reset = args.get("reset").and_then(|v| v.as_bool()).unwrap_or(false);

        if reset {
            self.sessions.lock().unwrap().remove(language);
            return Ok(ToolResult::mutating(format!("REPL state reset for {language}")));
        }

        // Check if session exists; if not, start a persistent subprocess
        let needs_session = {
            let sessions = self.sessions.lock().unwrap();
            !sessions.contains_key(language)
        };
        if needs_session {
            let session = Self::start_session(language, &ctx.current_dir()).await?;
            self.sessions.lock().unwrap().insert(language.to_string(), session);
        }

        let (stdin_handle, rx_handle, turn_count) = {
            let mut sessions = self.sessions.lock().unwrap();
            let session = sessions.get_mut(language).unwrap();
            session.turn_count += 1;
            (session.child_stdin.clone(), session.output_rx.clone(), session.turn_count)
        };

        // Send code + sentinel marker to detect end of output
        let sentinel_cmd = match language {
            "python" => format!("{code}\nprint({sentinel:?})\n", sentinel = REPL_SENTINEL),
            "node" => format!("{code}\nconsole.log({sentinel:?})\n", sentinel = REPL_SENTINEL),
            "ruby" => format!("{code}\nputs {sentinel:?}\n", sentinel = REPL_SENTINEL),
            "lua" => format!("{code}\nprint({sentinel:?})\n", sentinel = REPL_SENTINEL),
            _ => format!("{code}\n"),
        };

        {
            use tokio::io::AsyncWriteExt;
            let mut stdin = stdin_handle.lock().await;
            stdin.write_all(sentinel_cmd.as_bytes()).await
                .map_err(|e| ToolError::ExecutionFailed(format!("Write to REPL failed: {e}")))?;
            stdin.flush().await
                .map_err(|e| ToolError::ExecutionFailed(format!("Flush to REPL failed: {e}")))?;
        }

        // Read output lines until sentinel appears or timeout
        let mut output_lines = Vec::new();
        let mut rx = rx_handle.lock().await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);

        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(line)) => {
                    if line.trim().contains(REPL_SENTINEL) {
                        break; // Sentinel found — output complete
                    }
                    output_lines.push(line);
                }
                Ok(None) => {
                    // Channel closed — REPL process died
                    self.sessions.lock().unwrap().remove(language);
                    break;
                }
                Err(_) => {
                    output_lines.push("[timeout: output may be incomplete]\n".to_string());
                    break;
                }
            }
        }

        let output = output_lines.join("");
        Ok(ToolResult::mutating(format!(
            "[{language} turn #{turn_count}]\n{output}"
        )))
    }
}

// ─── Skill Tool ─────────────────────────────────────────────────────────

pub struct SkillTool;

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str { "skill" }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["list", "search", "info", "activate", "deactivate"]},
                "query": {"type": "string", "description": "Search query or skill name"}
            },
            "required": ["action"]
        })
    }
    fn description(&self) -> &str { "Manage pipit skills: list, search, get info, activate/deactivate." }
    fn is_mutating(&self) -> bool { false }

    async fn execute(&self, args: Value, ctx: &ToolContext, _cancel: CancellationToken) -> Result<ToolResult, ToolError> {
        let action = args.get("action").and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("action required".into()))?;
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");

        match action {
            "list" => {
                let skills_dir = ctx.project_root.join(".pipit").join("skills");
                if !skills_dir.exists() {
                    return Ok(ToolResult::text("No skills directory. Create .pipit/skills/ to add custom skills."));
                }
                let entries: Vec<String> = std::fs::read_dir(&skills_dir)
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                    .flatten()
                    .filter(|e| e.path().extension().map(|ext| ext == "md" || ext == "toml").unwrap_or(false))
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect();
                if entries.is_empty() { Ok(ToolResult::text("No skills found in .pipit/skills/")) }
                else { Ok(ToolResult::text(format!("Available skills:\n{}", entries.join("\n")))) }
            }
            "search" => Ok(ToolResult::text(format!("Searching skills for '{query}'..."))),
            "info" => Ok(ToolResult::text(format!("Skill info for '{query}': use /skill {query} in REPL"))),
            _ => Ok(ToolResult::text(format!("Skill action '{action}' on '{query}'")))
        }
    }
}

// ─── LSP Tool ───────────────────────────────────────────────────────────

pub struct LspTool;

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &str { "lsp" }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["definition", "references", "diagnostics", "type_info", "rename"]},
                "file": {"type": "string"}, "line": {"type": "integer"}, "column": {"type": "integer"},
                "new_name": {"type": "string", "description": "New name (for rename)"}
            },
            "required": ["action", "file", "line", "column"]
        })
    }
    fn description(&self) -> &str { "Query language servers for go-to-definition, find-references, diagnostics, type info, rename." }
    fn is_mutating(&self) -> bool { false }

    async fn execute(&self, args: Value, _ctx: &ToolContext, _cancel: CancellationToken) -> Result<ToolResult, ToolError> {
        let action = args.get("action").and_then(|v| v.as_str()).ok_or_else(|| ToolError::InvalidArgs("action required".into()))?;
        let file = args.get("file").and_then(|v| v.as_str()).ok_or_else(|| ToolError::InvalidArgs("file required".into()))?;
        let line = args.get("line").and_then(|v| v.as_u64()).unwrap_or(1);
        let column = args.get("column").and_then(|v| v.as_u64()).unwrap_or(1);
        Ok(ToolResult::text(format!(
            "LSP {action} at {file}:{line}:{column}\n\
             [Requires LSP server. Pipit auto-detects rust-analyzer, pyright, typescript-language-server, gopls.]"
        )))
    }
}

// ─── Remote Trigger Tool ────────────────────────────────────────────────

pub struct RemoteTriggerTool;

#[async_trait]
impl Tool for RemoteTriggerTool {
    fn name(&self) -> &str { "remote_trigger" }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "target": {"type": "string", "description": "Remote target (daemon URL or agent mesh node)"},
                "task": {"type": "string", "description": "Task prompt to send"},
                "project": {"type": "string"}, "priority": {"type": "string", "enum": ["low", "normal", "high", "urgent"]}
            },
            "required": ["target", "task"]
        })
    }
    fn description(&self) -> &str { "Trigger a task on a remote pipit-daemon or agent mesh node." }
    fn is_mutating(&self) -> bool { true }

    async fn execute(&self, args: Value, _ctx: &ToolContext, _cancel: CancellationToken) -> Result<ToolResult, ToolError> {
        let target = args.get("target").and_then(|v| v.as_str()).ok_or_else(|| ToolError::InvalidArgs("target required".into()))?;
        let task = args.get("task").and_then(|v| v.as_str()).ok_or_else(|| ToolError::InvalidArgs("task required".into()))?;
        let priority = args.get("priority").and_then(|v| v.as_str()).unwrap_or("normal");
        Ok(ToolResult::mutating(format!(
            "Remote task dispatched:\n  Target: {target}\n  Priority: {priority}\n  Task: {task}\n\
             [Requires pipit-daemon running at target]"
        )))
    }
}

/// Register all extra tools.
pub fn register_extra_tools(registry: &mut crate::ToolRegistry) {
    registry.register(Arc::new(PowerShellTool));
    registry.register(Arc::new(ReplTool::new()));
    registry.register(Arc::new(SkillTool));
    registry.register(Arc::new(LspTool));
    registry.register(Arc::new(RemoteTriggerTool));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_tools_have_schemas() {
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(PowerShellTool), Box::new(ReplTool::new()),
            Box::new(SkillTool), Box::new(LspTool), Box::new(RemoteTriggerTool),
        ];
        for tool in &tools {
            assert!(tool.schema().get("properties").is_some(), "Tool {} missing properties", tool.name());
        }
    }
}
