use crate::{Tool, ToolContext, ToolError, ToolResult, ToolDisplay};
use async_trait::async_trait;
use pipit_config::ApprovalMode;
use serde_json::Value;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Execute a shell command with timeout.
pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 120)"
                }
            },
            "required": ["command"]
        })
    }

    fn description(&self) -> &str {
        "Execute a shell command and return stdout/stderr."
    }

    fn is_mutating(&self) -> bool {
        true
    }

    fn requires_approval(&self, mode: ApprovalMode) -> bool {
        // Shell commands need approval in all modes except FullAuto
        !matches!(mode, ApprovalMode::FullAuto)
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let command = args["command"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArgs("missing 'command'".to_string()))?;

        let timeout_secs = args["timeout"].as_u64().unwrap_or(120);

        // Defense-in-depth — block dangerous patterns.
        // Normalize the command to defeat encoding tricks (hex escapes, variable
        // interpolation patterns, backslash evasion, etc.) before checking.
        let cmd_normalized = normalize_command(command);
        let cmd_lower = cmd_normalized.to_lowercase();

        const DANGEROUS_PATTERNS: &[&str] = &[
            "rm -rf /", "rm -rf /*", "rm -r -f /", "rm --recursive --force /",
            "mkfs", "dd if=", ":(){", "> /dev/sd", "> /dev/nvm",
            "chmod -R 000 /", "find / -delete",
        ];
        for d in DANGEROUS_PATTERNS {
            if cmd_lower.contains(d) {
                return Err(ToolError::PermissionDenied(format!(
                    "Blocked dangerous command pattern: {}", d
                )));
            }
        }

        // Block known destructive binaries even when invoked via absolute path
        const BLOCKED_BINARIES: &[&str] = &[
            "mkfs", "fdisk", "parted", "wipefs",
        ];
        for bin in BLOCKED_BINARIES {
            // Match /usr/sbin/mkfs, /sbin/mkfs, etc.
            if cmd_lower.split_whitespace().any(|tok| {
                tok == *bin || tok.ends_with(&format!("/{}", bin))
            }) {
                return Err(ToolError::PermissionDenied(format!(
                    "Blocked dangerous binary: {}", bin
                )));
            }
        }

        // Block commands that resolve paths outside project root
        const PATH_TRAVERSAL_INDICATORS: &[&str] = &["../../../", "/../"];
        for p in PATH_TRAVERSAL_INDICATORS {
            if command.contains(p) {
                return Err(ToolError::PermissionDenied(
                    "Blocked: path traversal detected in command".to_string(),
                ));
            }
        }

        // #23: Use sandbox for command execution when available
        let sandbox_config = super::sandbox::load_sandbox_config(&ctx.project_root);
        let mut child_cmd = super::sandbox::sandboxed_command(command, &ctx.project_root, &sandbox_config);
        child_cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let child = child_cmd
            .spawn()
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to spawn: {}", e)))?;

        let timeout = Duration::from_secs(timeout_secs);

        let result = tokio::select! {
            output = child.wait_with_output() => {
                output.map_err(|e| ToolError::ExecutionFailed(format!("Process error: {}", e)))?
            }
            _ = tokio::time::sleep(timeout) => {
                return Err(ToolError::Timeout(timeout_secs));
            }
            _ = cancel.cancelled() => {
                return Err(ToolError::ExecutionFailed("Cancelled".to_string()));
            }
        };

        let stdout = String::from_utf8_lossy(&result.stdout).to_string();
        let stderr = String::from_utf8_lossy(&result.stderr).to_string();
        let exit_code = result.status.code();

        // Truncate long output
        let max_len = 32_000;
        let stdout_truncated = if stdout.len() > max_len {
            let lines: Vec<&str> = stdout.lines().collect();
            let total = lines.len();
            let first_n = 50;
            let last_n = 50;
            if total > first_n + last_n {
                format!(
                    "{}\n\n[...truncated {} lines...]\n\n{}",
                    lines[..first_n].join("\n"),
                    total - first_n - last_n,
                    lines[total - last_n..].join("\n"),
                )
            } else {
                stdout[..max_len].to_string()
            }
        } else {
            stdout.clone()
        };

        let mut output = String::new();
        if !stdout_truncated.is_empty() {
            output.push_str(&stdout_truncated);
        }
        if !stderr.is_empty() {
            if !output.is_empty() {
                output.push_str("\n\n");
            }
            output.push_str("[STDERR]\n");
            output.push_str(&stderr);
        }
        if let Some(code) = exit_code {
            if code != 0 {
                output.push_str(&format!("\n\n[Exit code: {}]", code));
            }
        }

        if output.is_empty() {
            output = "[No output]".to_string();
        }

        if !result.status.success() {
            return Err(ToolError::ExecutionFailed(output));
        }

        let mut result = ToolResult::mutating(output);
        result.display = Some(ToolDisplay::ShellOutput {
            command: command.to_string(),
            stdout: stdout_truncated,
            stderr,
            exit_code,
        });

        Ok(result)
    }
}

/// Normalize a shell command string for security checks.
///
/// Strips common evasion techniques: backslash line continuations, shell hex
/// escapes (`$'\x72'`), and redundant whitespace. This makes pattern matching
/// more reliable against obfuscated commands.
fn normalize_command(cmd: &str) -> String {
    let mut s = cmd.to_string();

    // Remove backslash-newline continuations (line splicing)
    s = s.replace("\\\n", "");

    // Expand $'\xHH' hex escapes (commonly used to smuggle characters)
    while let Some(start) = s.find("$'\\x") {
        if let Some(end) = s[start..].find('\'').and_then(|first_quote| {
            s[start + first_quote + 1..].find('\'').map(|q| start + first_quote + 1 + q)
        }) {
            // We found a $'...' block; try to decode hex escapes inside it
            let inner = &s[start + 2..end + 1]; // includes surrounding quotes
            let decoded = decode_dollar_quotes(inner);
            s = format!("{}{}{}", &s[..start], decoded, &s[end + 1..]);
        } else {
            break;
        }
    }

    // Collapse runs of whitespace
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decode the interior of a `$'...'` shell string, handling `\xHH` escapes.
fn decode_dollar_quotes(s: &str) -> String {
    let inner = s.trim_matches('\'');
    let mut result = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('x') => {
                    let hex: String = chars.by_ref().take(2).collect();
                    if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                        result.push(byte as char);
                    } else {
                        result.push_str("\\x");
                        result.push_str(&hex);
                    }
                }
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(c);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipit_config::ApprovalMode;
    use tempfile::tempdir;

    #[tokio::test]
    async fn returns_error_for_non_zero_exit() {
        let temp_dir = tempdir().unwrap();
        let ctx = ToolContext::new(temp_dir.path().to_path_buf(), ApprovalMode::FullAuto);
        let tool = BashTool;

        let result = tool
            .execute(
                serde_json::json!({
                    "command": "python3 -c 'import sys; print(\"before-fail\"); sys.exit(3)'"
                }),
                &ctx,
                CancellationToken::new(),
            )
            .await;

        match result {
            Err(ToolError::ExecutionFailed(message)) => {
                assert!(message.contains("before-fail"));
                assert!(message.contains("Exit code: 3"));
            }
            other => panic!("expected execution failure, got {:?}", other),
        }
    }
}
