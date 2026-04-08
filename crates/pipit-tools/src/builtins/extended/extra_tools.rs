//! Extra Tools — PowerShell, REPL, Skill, LSP, RemoteTrigger
//! Brings total tool count to 37.

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolContext, ToolDisplay, ToolError, ToolResult};

// ─── PowerShell Tool ────────────────────────────────────────────────────

pub struct PowerShellTool;

#[async_trait]
impl Tool for PowerShellTool {
    fn name(&self) -> &str {
        "powershell"
    }
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
    fn description(&self) -> &str {
        "Execute a PowerShell command. Uses pwsh on macOS/Linux, powershell.exe on Windows."
    }
    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("command required".into()))?;
        let timeout = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(120);

        let shell = if cfg!(windows) { "powershell" } else { "pwsh" };

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout),
            tokio::process::Command::new(shell)
                .args(["-NoProfile", "-NonInteractive", "-Command", command])
                .current_dir(ctx.current_dir())
                .output(),
        )
        .await
        .map_err(|_| ToolError::Timeout(timeout))?
        .map_err(|e| ToolError::ExecutionFailed(format!("PowerShell exec failed: {e}")))?;

        let stdout = String::from_utf8_lossy(&result.stdout).to_string();
        let stderr = String::from_utf8_lossy(&result.stderr).to_string();
        let exit_code = result.status.code().unwrap_or(-1);

        Ok(ToolResult {
            content: format!(
                "Exit code: {exit_code}\n\n{stdout}{}",
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!("\nSTDERR:\n{stderr}")
                }
            ),
            display: Some(ToolDisplay::ShellOutput {
                command: command.to_string(),
                stdout: stdout.clone(),
                stderr: stderr.clone(),
                exit_code: Some(exit_code),
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
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Start a persistent REPL subprocess for the given language.
    async fn start_session(
        language: &str,
        cwd: &std::path::Path,
    ) -> Result<ReplSession, ToolError> {
        let (binary, args): (&str, Vec<&str>) = match language {
            "python" => ("python3", vec!["-u", "-i"]),
            "node" => ("node", vec!["-i"]),
            "ruby" => ("ruby", vec!["-e", "require 'irb'; IRB.start"]),
            "lua" => ("lua", vec!["-i"]),
            _ => {
                return Err(ToolError::InvalidArgs(format!(
                    "Unsupported language: {language}"
                )));
            }
        };

        let mut child = tokio::process::Command::new(binary)
            .args(&args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to start {language} REPL: {e}"))
            })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ToolError::ExecutionFailed("No stdin for REPL".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ToolError::ExecutionFailed("No stdout for REPL".into()))?;
        let stderr = child
            .stderr
            .take()
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
    fn name(&self) -> &str {
        "repl"
    }
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
    fn description(&self) -> &str {
        "Execute code in a persistent language REPL. State persists across calls."
    }
    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let language = args
            .get("language")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("language required".into()))?;
        let code = args
            .get("code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("code required".into()))?;
        let reset = args.get("reset").and_then(|v| v.as_bool()).unwrap_or(false);

        if reset {
            self.sessions.lock().unwrap().remove(language);
            return Ok(ToolResult::mutating(format!(
                "REPL state reset for {language}"
            )));
        }

        // Check if session exists; if not, start a persistent subprocess
        let needs_session = {
            let sessions = self.sessions.lock().unwrap();
            !sessions.contains_key(language)
        };
        if needs_session {
            let session = Self::start_session(language, &ctx.current_dir()).await?;
            self.sessions
                .lock()
                .unwrap()
                .insert(language.to_string(), session);
        }

        let (stdin_handle, rx_handle, turn_count) = {
            let mut sessions = self.sessions.lock().unwrap();
            let session = sessions.get_mut(language).unwrap();
            session.turn_count += 1;
            (
                session.child_stdin.clone(),
                session.output_rx.clone(),
                session.turn_count,
            )
        };

        // Send code + sentinel marker to detect end of output
        let sentinel_cmd = match language {
            "python" => format!("{code}\nprint({sentinel:?})\n", sentinel = REPL_SENTINEL),
            "node" => format!(
                "{code}\nconsole.log({sentinel:?})\n",
                sentinel = REPL_SENTINEL
            ),
            "ruby" => format!("{code}\nputs {sentinel:?}\n", sentinel = REPL_SENTINEL),
            "lua" => format!("{code}\nprint({sentinel:?})\n", sentinel = REPL_SENTINEL),
            _ => format!("{code}\n"),
        };

        {
            use tokio::io::AsyncWriteExt;
            let mut stdin = stdin_handle.lock().await;
            stdin
                .write_all(sentinel_cmd.as_bytes())
                .await
                .map_err(|e| ToolError::ExecutionFailed(format!("Write to REPL failed: {e}")))?;
            stdin
                .flush()
                .await
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
    fn name(&self) -> &str {
        "skill"
    }
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
    fn description(&self) -> &str {
        "Manage pipit skills: list, search, get info, activate/deactivate."
    }
    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("action required".into()))?;
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");

        match action {
            "list" => {
                let skills_dir = ctx.project_root.join(".pipit").join("skills");
                if !skills_dir.exists() {
                    return Ok(ToolResult::text(
                        "No skills directory. Create .pipit/skills/ to add custom skills.",
                    ));
                }
                let entries: Vec<String> = std::fs::read_dir(&skills_dir)
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                    .flatten()
                    .filter(|e| {
                        e.path()
                            .extension()
                            .map(|ext| ext == "md" || ext == "toml")
                            .unwrap_or(false)
                    })
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect();
                if entries.is_empty() {
                    Ok(ToolResult::text("No skills found in .pipit/skills/"))
                } else {
                    Ok(ToolResult::text(format!(
                        "Available skills:\n{}",
                        entries.join("\n")
                    )))
                }
            }
            "search" => {
                let skills_dir = ctx.project_root.join(".pipit").join("skills");
                let global_dir = dirs::config_dir()
                    .map(|d| d.join("pipit").join("skills"))
                    .unwrap_or_default();

                let mut matches = Vec::new();
                for dir in [&skills_dir, &global_dir] {
                    if !dir.exists() {
                        continue;
                    }
                    if let Ok(entries) = std::fs::read_dir(dir) {
                        for entry in entries.flatten() {
                            let name = entry.file_name().to_string_lossy().to_string();
                            let name_lower = name.to_lowercase();
                            let query_lower = query.to_lowercase();
                            // Check filename match
                            if name_lower.contains(&query_lower) {
                                matches.push(format!("  {} (name match)", name));
                                continue;
                            }
                            // Check content match for .md and .toml files
                            if name.ends_with(".md") || name.ends_with(".toml") {
                                if let Ok(content) = std::fs::read_to_string(entry.path()) {
                                    if content.to_lowercase().contains(&query_lower) {
                                        let preview = content
                                            .lines()
                                            .find(|l| l.to_lowercase().contains(&query_lower))
                                            .unwrap_or("")
                                            .trim();
                                        let preview = if preview.len() > 80 {
                                            &preview[..80]
                                        } else {
                                            preview
                                        };
                                        matches.push(format!("  {} — {}", name, preview));
                                    }
                                }
                            }
                        }
                    }
                }
                if matches.is_empty() {
                    Ok(ToolResult::text(format!(
                        "No skills matching '{query}' found."
                    )))
                } else {
                    Ok(ToolResult::text(format!(
                        "Skills matching '{query}':\n{}",
                        matches.join("\n")
                    )))
                }
            }
            "info" => {
                if query.is_empty() {
                    return Ok(ToolResult::text("Usage: skill info <name>"));
                }
                let skills_dir = ctx.project_root.join(".pipit").join("skills");
                // Try .md then .toml extensions
                let candidates = vec![
                    skills_dir.join(format!("{query}.md")),
                    skills_dir.join(format!("{query}.toml")),
                    skills_dir.join(query),
                ];
                for path in &candidates {
                    if path.exists() {
                        let content = std::fs::read_to_string(path)
                            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                        let truncated = if content.len() > 4000 {
                            &content[..4000]
                        } else {
                            &content
                        };
                        return Ok(ToolResult::text(format!(
                            "Skill: {query}\nPath: {}\n\n{truncated}",
                            path.display()
                        )));
                    }
                }
                Ok(ToolResult::text(format!(
                    "Skill '{query}' not found in {}",
                    skills_dir.display()
                )))
            }
            _ => Ok(ToolResult::text(format!(
                "Skill action '{action}' on '{query}'"
            ))),
        }
    }
}

// ─── LSP Tool ───────────────────────────────────────────────────────────

pub struct LspTool;

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &str {
        "lsp"
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["definition", "references", "diagnostics", "type_info", "rename"]},
                "file": {"type": "string"}, "line": {"type": "integer"}, "column": {"type": "integer"},
                "new_name": {"type": "string", "description": "New name (for rename)"}
            },
            "required": ["action", "file"]
        })
    }
    fn description(&self) -> &str {
        "Query language servers for go-to-definition, find-references, diagnostics, type info, rename. Uses real LSP when available, falls back to grep."
    }
    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("action required".into()))?;
        let file = args
            .get("file")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("file required".into()))?;
        let line = args.get("line").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
        let column = args.get("column").and_then(|v| v.as_u64()).unwrap_or(1) as u32;

        let file_path = ctx.project_root.join(file);
        if !file_path.exists() {
            return Err(ToolError::ExecutionFailed(format!(
                "File not found: {file}"
            )));
        }

        // Try real LSP first for definition/references/type_info
        match action {
            "definition" | "references" | "type_info" => {
                // Attempt real LSP client — O(1) amortized after server init
                if let Some(result) =
                    try_lsp_action(action, &file_path, line, column, &ctx.project_root).await
                {
                    return Ok(ToolResult::text(result));
                }

                // Fallback: grep-based symbol search
                let file_content = tokio::fs::read_to_string(&file_path).await.map_err(|e| {
                    ToolError::ExecutionFailed(format!("Failed to read {file}: {e}"))
                })?;

                let target_line = file_content
                    .lines()
                    .nth((line - 1) as usize)
                    .ok_or_else(|| ToolError::InvalidArgs(format!("Line {line} out of range")))?;

                let symbol = extract_symbol_at(target_line, column as usize);
                if symbol.is_empty() {
                    return Ok(ToolResult::text(format!(
                        "No symbol found at {file}:{line}:{column}"
                    )));
                }

                let search_pattern = if action == "definition" {
                    format!(
                        r"(fn |struct |enum |trait |type |class |def |const |let |var |pub )\b{}\b",
                        regex::escape(&symbol)
                    )
                } else {
                    format!(r"\b{}\b", regex::escape(&symbol))
                };

                let output = tokio::process::Command::new("grep")
                    .args([
                        "-rn",
                        "--include=*.rs",
                        "--include=*.py",
                        "--include=*.ts",
                        "--include=*.js",
                        "--include=*.go",
                        "--include=*.java",
                        "-E",
                        &search_pattern,
                    ])
                    .arg(".")
                    .current_dir(&ctx.project_root)
                    .output()
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("grep failed: {e}")))?;

                let stdout = String::from_utf8_lossy(&output.stdout);
                let results: Vec<&str> = stdout.lines().take(50).collect();

                if results.is_empty() {
                    Ok(ToolResult::text(format!(
                        "No {action} found for `{symbol}` at {file}:{line}:{column}"
                    )))
                } else {
                    Ok(ToolResult::text(format!(
                        "{} for `{symbol}` ({} results):\n{}",
                        if action == "definition" {
                            "Definitions"
                        } else {
                            "References"
                        },
                        results.len(),
                        results.join("\n")
                    )))
                }
            }
            "diagnostics" => {
                // Run language-specific linter
                let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
                let (cmd, args_list): (&str, Vec<&str>) = match ext {
                    "rs" => ("cargo", vec!["check", "--message-format=short", "2>&1"]),
                    "py" => ("python3", vec!["-m", "py_compile", file]),
                    "ts" | "tsx" => ("npx", vec!["tsc", "--noEmit", file]),
                    "js" | "jsx" => ("node", vec!["--check", file]),
                    _ => {
                        return Ok(ToolResult::text(format!(
                            "No diagnostic support for .{ext} files"
                        )));
                    }
                };

                let output = tokio::process::Command::new(cmd)
                    .args(&args_list)
                    .current_dir(&ctx.project_root)
                    .output()
                    .await
                    .map_err(|e| {
                        ToolError::ExecutionFailed(format!("Diagnostic command failed: {e}"))
                    })?;

                let combined = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                let truncated = if combined.len() > 8000 {
                    &combined[..8000]
                } else {
                    &combined
                };

                Ok(ToolResult::text(format!(
                    "Diagnostics for {file}:\n{}",
                    if truncated.trim().is_empty() {
                        "No issues found."
                    } else {
                        truncated
                    }
                )))
            }
            "rename" => {
                let new_name = args
                    .get("new_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidArgs("new_name required for rename".into()))?;

                let file_content = tokio::fs::read_to_string(&file_path)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read: {e}")))?;
                let target_line = file_content.lines().nth((line - 1) as usize).unwrap_or("");
                let symbol = extract_symbol_at(target_line, column as usize);

                if symbol.is_empty() {
                    return Err(ToolError::InvalidArgs(format!(
                        "No symbol at {file}:{line}:{column}"
                    )));
                }

                // Find all files containing the symbol
                let output = tokio::process::Command::new("grep")
                    .args([
                        "-rln",
                        "--include=*.rs",
                        "--include=*.py",
                        "--include=*.ts",
                        "--include=*.js",
                        "--include=*.go",
                        "-w",
                        &symbol,
                    ])
                    .arg(".")
                    .current_dir(&ctx.project_root)
                    .output()
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("grep failed: {e}")))?;

                let stdout_str = String::from_utf8_lossy(&output.stdout).to_string();
                let files: Vec<&str> = stdout_str.lines().take(100).collect();

                // Perform sed rename across all files
                let file_count = files.len();
                for f in &files {
                    let fpath = ctx.project_root.join(f.trim_start_matches("./"));
                    if let Ok(content) = tokio::fs::read_to_string(&fpath).await {
                        let replaced = content.replace(&symbol, new_name);
                        if replaced != content {
                            let _ = tokio::fs::write(&fpath, replaced).await;
                        }
                    }
                }

                Ok(ToolResult::mutating(format!(
                    "Renamed `{symbol}` → `{new_name}` across {file_count} file(s)"
                )))
            }
            _ => Err(ToolError::InvalidArgs(format!(
                "Unknown LSP action: {action}"
            ))),
        }
    }
}

/// Try real LSP server for definition/references/type_info.
/// Returns None if no suitable LSP is available, allowing fallback to grep.
async fn try_lsp_action(
    action: &str,
    file_path: &std::path::Path,
    line: u32,
    column: u32,
    project_root: &std::path::Path,
) -> Option<String> {
    use pipit_lsp::LspKind;

    // Detect which LSP server to use based on file extension
    let ext = file_path.extension()?.to_str()?;
    let kind = match ext {
        "rs" => LspKind::RustAnalyzer,
        "py" => LspKind::Pyright,
        "ts" | "tsx" | "js" | "jsx" => LspKind::TypescriptLanguageServer,
        "go" => LspKind::Gopls,
        _ => return None,
    };

    // Try to start the LSP client (amortized cost — caller should cache for production)
    let client = match pipit_lsp::client::LspClient::start(kind, project_root).await {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(server = kind.binary(), error = %e, "LSP server not available, falling back to grep");
            return None;
        }
    };

    // LSP uses 0-indexed positions
    let lsp_line = line.saturating_sub(1);
    let lsp_col = column.saturating_sub(1);

    let result = match action {
        "definition" => match client.goto_definition(file_path, lsp_line, lsp_col).await {
            Ok(def) if !def.locations.is_empty() => {
                let locs: Vec<String> = def
                    .locations
                    .iter()
                    .map(|l| format!("{}:{}:{}", l.file.display(), l.line + 1, l.column + 1))
                    .collect();
                Some(format!(
                    "Definition ({} location(s)):\n{}",
                    locs.len(),
                    locs.join("\n")
                ))
            }
            _ => None,
        },
        "references" => match client.find_references(file_path, lsp_line, lsp_col).await {
            Ok(refs) if !refs.locations.is_empty() => {
                let locs: Vec<String> = refs
                    .locations
                    .iter()
                    .take(50)
                    .map(|l| format!("{}:{}:{}", l.file.display(), l.line + 1, l.column + 1))
                    .collect();
                Some(format!(
                    "References ({} location(s)):\n{}",
                    refs.locations.len(),
                    locs.join("\n")
                ))
            }
            _ => None,
        },
        "type_info" => match client.hover(file_path, lsp_line, lsp_col).await {
            Ok(Some(info)) if !info.type_string.is_empty() => Some(format!(
                "Type: {}\n{}",
                info.type_string,
                info.documentation.unwrap_or_default()
            )),
            _ => None,
        },
        _ => None,
    };

    // Shut down the temporary client
    client.shutdown().await;
    result
}

/// Extract the symbol (identifier) at a given column in a line.
fn extract_symbol_at(line: &str, col: usize) -> String {
    let chars: Vec<char> = line.chars().collect();
    if col == 0 || col > chars.len() {
        return String::new();
    }
    let idx = col - 1; // 1-indexed to 0-indexed
    if !chars[idx].is_alphanumeric() && chars[idx] != '_' {
        return String::new();
    }
    let mut start = idx;
    while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_') {
        start -= 1;
    }
    let mut end = idx;
    while end < chars.len() - 1 && (chars[end + 1].is_alphanumeric() || chars[end + 1] == '_') {
        end += 1;
    }
    chars[start..=end].iter().collect()
}

// ─── Remote Trigger Tool ────────────────────────────────────────────────

pub struct RemoteTriggerTool;

#[async_trait]
impl Tool for RemoteTriggerTool {
    fn name(&self) -> &str {
        "remote_trigger"
    }
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
    fn description(&self) -> &str {
        "Trigger a task on a remote pipit-daemon or agent mesh node."
    }
    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        args: Value,
        _ctx: &ToolContext,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let target = args
            .get("target")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("target required".into()))?;
        let task = args
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("task required".into()))?;
        let priority = args
            .get("priority")
            .and_then(|v| v.as_str())
            .unwrap_or("normal");
        let project = args.get("project").and_then(|v| v.as_str()).unwrap_or("");

        // Build the daemon API URL
        let url = if target.starts_with("http") {
            format!("{}/api/tasks", target.trim_end_matches('/'))
        } else {
            format!("http://{}/api/tasks", target)
        };

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| ToolError::ExecutionFailed(format!("HTTP client error: {e}")))?;

        let body = serde_json::json!({
            "prompt": task,
            "priority": priority,
            "project": project,
        });

        let response = tokio::select! {
            r = client.post(&url).json(&body).send() => {
                r.map_err(|e| ToolError::ExecutionFailed(format!(
                    "Failed to reach pipit-daemon at {target}: {e}\n\
                     Ensure pipit-daemon is running: pipit daemon start"
                )))?
            }
            _ = cancel.cancelled() => return Err(ToolError::ExecutionFailed("Cancelled".into())),
        };

        let status = response.status();
        let body_text = response.text().await.unwrap_or_default();

        if status.is_success() {
            Ok(ToolResult::mutating(format!(
                "Task dispatched to {target}:\n  Priority: {priority}\n  Task: {task}\n  Response: {body_text}"
            )))
        } else {
            Err(ToolError::ExecutionFailed(format!(
                "Daemon at {target} returned {status}: {body_text}"
            )))
        }
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
            Box::new(PowerShellTool),
            Box::new(ReplTool::new()),
            Box::new(SkillTool),
            Box::new(LspTool),
            Box::new(RemoteTriggerTool),
        ];
        for tool in &tools {
            assert!(
                tool.schema().get("properties").is_some(),
                "Tool {} missing properties",
                tool.name()
            );
        }
    }
}
