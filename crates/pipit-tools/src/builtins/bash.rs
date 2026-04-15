use crate::{Tool, ToolContext, ToolDisplay, ToolError, ToolResult};
use async_trait::async_trait;
use pipit_config::ApprovalMode;
use serde_json::Value;
use std::path::PathBuf;
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
                    "description": "Timeout in seconds (default: 300)"
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory for this command, relative to project root (default: current cwd)"
                }
            },
            "required": ["command"]
        })
    }

    fn description(&self) -> &str {
        "Execute a shell command and return stdout/stderr."
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

        let timeout_secs = args["timeout"].as_u64().unwrap_or(300);

        // Defense-in-depth — block dangerous patterns.
        // Normalize the command to defeat encoding tricks (hex escapes, variable
        // interpolation patterns, backslash evasion, etc.) before checking.
        let cmd_normalized = normalize_command(command);
        let cmd_lower = cmd_normalized.to_lowercase();

        const DANGEROUS_PATTERNS: &[&str] = &[
            "rm -rf /",
            "rm -rf /*",
            "rm -r -f /",
            "rm --recursive --force /",
            "mkfs",
            "dd if=",
            ":(){",
            "> /dev/sd",
            "> /dev/nvm",
            "chmod -R 000 /",
            "find / -delete",
        ];
        for d in DANGEROUS_PATTERNS {
            if cmd_lower.contains(d) {
                return Err(ToolError::PermissionDenied(format!(
                    "Blocked dangerous command pattern: {}",
                    d
                )));
            }
        }

        // Block known destructive binaries even when invoked via absolute path
        const BLOCKED_BINARIES: &[&str] = &["mkfs", "fdisk", "parted", "wipefs"];
        for bin in BLOCKED_BINARIES {
            // Match /usr/sbin/mkfs, /sbin/mkfs, etc.
            if cmd_lower
                .split_whitespace()
                .any(|tok| tok == *bin || tok.ends_with(&format!("/{}", bin)))
            {
                return Err(ToolError::PermissionDenied(format!(
                    "Blocked dangerous binary: {}",
                    bin
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

        // ── sed expression validation ──
        // Validate sed -i commands to prevent unintended file destruction.
        if cmd_lower.contains("sed") && (command.contains("-i") || command.contains("--in-place")) {
            if let Err(msg) = validate_sed_expression(command) {
                return Err(ToolError::PermissionDenied(format!(
                    "sed validation: {msg}"
                )));
            }
        }

        // ── Layer 1.5: Adversarial semantic analysis (tree-sitter-backed) ──
        // Catches Zsh escape vectors, EQUALS expansion, IFS injection,
        // heredoc smuggling, process substitution exfil, obfuscated flags,
        // bare-repo planting, config writes, and eval/exec abuse.
        match super::bash_security::analyze_command(command, &ctx.project_root) {
            super::bash_security::SecurityVerdict::Reject(reason) => {
                return Err(ToolError::PermissionDenied(reason));
            }
            super::bash_security::SecurityVerdict::Review(reason) => {
                tracing::warn!(command = command, reason = %reason, "bash security: needs review");
                // In full-auto mode, treat review as reject for safety
                if ctx.approval_mode == ApprovalMode::FullAuto {
                    return Err(ToolError::PermissionDenied(format!(
                        "Blocked (full_auto): {}", reason
                    )));
                }
            }
            super::bash_security::SecurityVerdict::Safe => {}
        }

        // ── cd interception: persist directory changes across calls ──
        //
        // Each bash call spawns a fresh subprocess, so `cd /foo` doesn't
        // persist. We intercept pure `cd` commands and update ctx.cwd.
        let effective_cwd = if let Some(cwd_arg) = args["cwd"].as_str() {
            let requested = ctx.project_root.join(cwd_arg);
            let resolved = requested.canonicalize().unwrap_or(requested);
            if !resolved.starts_with(&ctx.project_root) {
                return Err(ToolError::PermissionDenied(
                    "cwd is outside project root".to_string(),
                ));
            }
            resolved
        } else {
            ctx.current_dir()
        };
        let trimmed_cmd = command.trim();

        // Pure cd command: `cd /path` (no &&, ;, or |)
        if trimmed_cmd == "cd"
            || (trimmed_cmd.starts_with("cd ")
                && !trimmed_cmd.contains("&&")
                && !trimmed_cmd.contains(';')
                && !trimmed_cmd.contains('|'))
        {
            let target = if trimmed_cmd == "cd" {
                // Bare `cd` → home directory
                std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| ctx.project_root.clone())
            } else {
                let path_arg = trimmed_cmd.strip_prefix("cd ").unwrap().trim();
                let path_arg = path_arg.trim_matches('"').trim_matches('\'');

                // Expand ~ to home
                let expanded = if path_arg.starts_with("~/") || path_arg == "~" {
                    if let Ok(home) = std::env::var("HOME") {
                        PathBuf::from(home).join(path_arg.strip_prefix("~/").unwrap_or(""))
                    } else {
                        PathBuf::from(path_arg)
                    }
                } else {
                    PathBuf::from(path_arg)
                };

                // Resolve relative to current cwd
                if expanded.is_absolute() {
                    expanded
                } else {
                    effective_cwd.join(&expanded)
                }
            };

            // Canonicalize (resolve .., symlinks)
            let resolved = match target.canonicalize() {
                Ok(p) => p,
                Err(e) => {
                    return Ok(ToolResult::text(format!("cd: {}: {}", target.display(), e)));
                }
            };

            if !resolved.is_dir() {
                return Ok(ToolResult::text(format!(
                    "cd: {}: No such directory",
                    resolved.display()
                )));
            }

            // Prevent escaping project root
            if !resolved.starts_with(&ctx.project_root) {
                return Ok(ToolResult::text(format!(
                    "cd: cannot change directory to {} (outside project root {})",
                    resolved.display(),
                    ctx.project_root.display()
                )));
            }

            ctx.set_cwd(resolved.clone());
            return Ok(ToolResult::text(format!(
                "Changed directory to {}",
                resolved.display()
            )));
        }

        // #23: Three-layer reference monitor for command safety:
        //
        // Layer 1 (above): Lexical early-reject — dangerous patterns + blocked binaries
        //   Cost: O(n) in command length. Catches known-bad patterns fast.
        //
        // Layer 2: Policy allowlist check — if configured, only permit approved binaries
        //   Cost: O(t + p) where t = token count, p = policy predicates.
        let sandbox_config = super::sandbox::load_sandbox_config(&ctx.project_root);
        if let Err(reason) = super::sandbox::check_binary_allowlist(command, &sandbox_config) {
            return Err(ToolError::PermissionDenied(format!(
                "Binary allowlist violation: {}",
                reason
            )));
        }

        // Layer 2b: VCS semantic firewall — gate git-mutating commands through
        // the pipit-vcs firewall to prevent privilege escalation, force push,
        // config injection, hook planting, and other semantic git attacks.
        if pipit_vcs::VcsGateway::is_git_mutation(&cmd_normalized) {
            let gateway = pipit_vcs::VcsGateway::new(ctx.project_root.clone());
            if let Err(e) = gateway.check_command(&cmd_normalized) {
                return Err(ToolError::PermissionDenied(format!(
                    "VCS firewall blocked: {e}"
                )));
            }
        }

        // Layer 3: Kernel isolation — sandbox via bwrap/seatbelt for syscall-level enforcement.
        //   The lexical filter is demoted to an early reject layer.
        //   Real control is sandbox + capability policy.
        let mut child_cmd =
            super::sandbox::sandboxed_command(command, &effective_cwd, &sandbox_config);
        child_cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // Close stdin so interactive commands (pagers, REPLs) don't hang
            .stdin(std::process::Stdio::null());

        // Create a new process group so we can kill the entire tree on
        // timeout/cancel. Without this, child subprocesses (npm, cargo,
        // make) survive as orphans after the parent shell is killed.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                child_cmd.pre_exec(|| {
                    // setsid() creates a new session + process group.
                    // All child processes inherit this group ID.
                    libc::setsid();
                    Ok(())
                });
            }
        }

        let mut child = child_cmd
            .spawn()
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to spawn: {}", e)))?;

        let timeout = Duration::from_secs(timeout_secs);

        // Get the PID for process group kill on timeout/cancel
        let child_id = child.id();

        let result = tokio::select! {
            output = child.wait_with_output() => {
                output.map_err(|e| ToolError::ExecutionFailed(format!("Process error: {}", e)))?
            }
            _ = tokio::time::sleep(timeout) => {
                // Kill the entire process group, not just the shell
                kill_process_group(child_id);
                return Err(ToolError::Timeout(timeout_secs));
            }
            _ = cancel.cancelled() => {
                kill_process_group(child_id);
                return Err(ToolError::ExecutionFailed("Cancelled".to_string()));
            }
        };

        let stdout = String::from_utf8_lossy(&result.stdout).to_string();
        let stderr = String::from_utf8_lossy(&result.stderr).to_string();
        let exit_code = result.status.code();

        // Truncate long output — error-aware: show more tail on failure
        let max_len = 32_000;
        let is_error = !result.status.success();
        let stdout_truncated = if stdout.len() > max_len {
            let lines: Vec<&str> = stdout.lines().collect();
            let total = lines.len();
            // On error, prioritize tail (tracebacks, assertion failures)
            let (head_n, tail_n) = if is_error { (20, 80) } else { (50, 50) };
            if total > head_n + tail_n {
                format!(
                    "{}\n\n[...truncated {} lines...]\n\n{}",
                    lines[..head_n].join("\n"),
                    total - head_n - tail_n,
                    lines[total - tail_n..].join("\n"),
                )
            } else {
                // Safe UTF-8 truncation — don't split mid-codepoint
                let safe_end = stdout
                    .char_indices()
                    .take_while(|(i, _)| *i < max_len)
                    .last()
                    .map(|(i, c)| i + c.len_utf8())
                    .unwrap_or(0);
                stdout[..safe_end].to_string()
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
            if let Some(code) = exit_code {
                if code != 0 {
                    output = format!(
                        "Command exited with code {} but produced no output.\n\
                         This usually means: the command was not found, a binary is missing, \
                         or the command failed silently.\n\
                         Try: `which <command>` or `command -v <command>` to check availability.",
                        code
                    );
                } else {
                    output = "Command completed successfully (no output).".to_string();
                }
            } else {
                output = "Command completed (no output).".to_string();
            }
        }

        if !result.status.success() {
            // Return as Ok with error content — the model needs to see
            // the failure details to adapt its approach. Returning Err
            // here causes the agent loop to treat it as a system error.
            let mut tool_result = ToolResult::text(output);
            tool_result.display = Some(ToolDisplay::ShellOutput {
                command: command.to_string(),
                stdout: stdout_truncated,
                stderr,
                exit_code,
            });
            return Ok(tool_result);
        }

        // Classify the command as read-only or mutating based on the leading
        // binary.  Read-only commands (ls, cat, echo, grep, find, which, …)
        // should NOT inflate the mutation counters in the adaptive budget and
        // proof state, because they don't change any files.
        let is_read_only = is_read_only_command(trimmed_cmd);

        let mut result = if is_read_only {
            ToolResult::text(output)
        } else {
            ToolResult::mutating(output)
        };
        result.display = Some(ToolDisplay::ShellOutput {
            command: command.to_string(),
            stdout: stdout_truncated,
            stderr,
            exit_code,
        });

        Ok(result)
    }
}

/// Kill an entire process group by PID.
/// Uses SIGKILL on the process group (negative PID) to ensure all child
/// processes are terminated — not just the top-level shell.
fn kill_process_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // kill(-pid, SIGKILL) sends to the entire process group
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
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
            s[start + first_quote + 1..]
                .find('\'')
                .map(|q| start + first_quote + 1 + q)
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

/// Validate a sed expression for correctness and safety before allowing `sed -i`.
///
/// Checks:
/// - Empty replacement with global flag (s/.*/\//g → deletes all content)
/// - Unbounded delete command (d without address → deletes all lines)
/// - Missing target file (sed -i without file arg)
fn validate_sed_expression(command: &str) -> Result<(), String> {
    let parts: Vec<&str> = command.split_whitespace().collect();

    // Find the sed command and flags
    let sed_idx = parts.iter().position(|p| p.ends_with("sed") || *p == "sed");
    if sed_idx.is_none() {
        return Ok(());
    }
    let sed_idx = sed_idx.unwrap();

    let mut has_in_place = false;
    let mut has_expression = false;
    let mut has_file = false;
    let mut expression = String::new();

    let mut i = sed_idx + 1;
    while i < parts.len() {
        let part = parts[i];
        if part == "-i" || part.starts_with("-i") || part == "--in-place" {
            has_in_place = true;
        } else if part == "-e" {
            has_expression = true;
            if i + 1 < parts.len() {
                expression = parts[i + 1].to_string();
                i += 1;
            }
        } else if part.starts_with('-') {
            // Other flags
        } else if !has_expression && expression.is_empty() {
            // First non-flag arg is the expression
            expression = part.to_string();
            has_expression = true;
        } else {
            // Subsequent non-flag args are files
            has_file = true;
        }
        i += 1;
    }

    if has_in_place && !has_file {
        return Err("sed -i without target file".to_string());
    }

    // Check the expression for dangerous patterns
    if !expression.is_empty() {
        let expr_lower = expression.to_lowercase();
        // Unbounded delete: just 'd' without address
        if expr_lower.trim_matches('\'').trim_matches('"') == "d" {
            return Err("sed with unbounded delete (d) would remove all lines".to_string());
        }
        // Empty substitution with global flag: s/pattern//g
        if let Some(rest) = expr_lower.strip_prefix('\'') {
            if rest.starts_with("s") && rest.len() > 2 {
                let delim = rest.chars().nth(1).unwrap_or('/');
                let inner = &rest[2..];
                // Find replacement section
                if let Some(pos) = inner.find(delim) {
                    let replacement = &inner[pos + 1..];
                    if let Some(end) = replacement.find(delim) {
                        let repl = &replacement[..end];
                        let flags = &replacement[end + 1..].trim_end_matches('\'');
                        if repl.is_empty() && flags.contains('g') {
                            // Check if pattern matches everything
                            let pattern = &inner[..pos];
                            if pattern == ".*" || pattern == ".+" || pattern == "." {
                                return Err("sed substitution deletes all content (empty replacement with .* pattern)".to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Heuristic: determine if a shell command is read-only based on the leading
/// binary or pipeline structure.
///
/// Commands like `ls`, `cat`, `grep`, `find`, `echo`, `which`, `env`, `pwd`,
/// `head`, `tail`, `wc`, `sort`, `diff`, `file`, `stat`, `du`, `df`, `uname`,
/// `whoami`, `date`, `hostname`, `id`, `printenv`, `type`, `test`, `[`, `true`,
/// `false`, `tree`, `rg`, `fd`, `bat`, `jq`, `yq`, `less`, `more`, `man`,
/// `cargo check`, `cargo test --no-run`, `npm test`, `python -c ...` (no file writes)
/// are classified as read-only.
///
/// Pipelines are read-only only if ALL stages are read-only.
///
/// When in doubt, classify as mutating — false negatives are safer than false positives.
fn is_read_only_command(cmd: &str) -> bool {
    // Shell redirects (>, >>, 2>) are always mutating regardless of the binary
    if cmd.contains('>') {
        return false;
    }

    const READ_ONLY_BINARIES: &[&str] = &[
        "ls",
        "cat",
        "echo",
        "grep",
        "egrep",
        "fgrep",
        "find",
        "which",
        "env",
        "pwd",
        "head",
        "tail",
        "wc",
        "sort",
        "uniq",
        "diff",
        "file",
        "stat",
        "du",
        "df",
        "uname",
        "whoami",
        "date",
        "hostname",
        "id",
        "printenv",
        "type",
        "test",
        "[",
        "true",
        "false",
        "tree",
        "rg",
        "fd",
        "bat",
        "jq",
        "yq",
        "less",
        "more",
        "man",
        "readlink",
        "basename",
        "dirname",
        "realpath",
        "sha256sum",
        "md5sum",
        "command",
        "help",
        "cal",
        "uptime",
        "ps",
        "top",
        "htop",
        "lsof",
        "netstat",
        "ss",
        "curl",
        "wget", // curl/wget without -o are read-only (print to stdout)
        "git log",
        "git status",
        "git diff",
        "git show",
        "git branch",
        "git remote",
        "git tag",
        "git rev-parse",
        "git ls-files",
        "cargo check",
        "cargo clippy",
        "cargo doc",
        "npm ls",
        "npm list",
        "npm outdated",
        "npm view",
        "node -e",
        "node -p",
        "python3 -c",
        "python -c",
    ];

    // Split on pipes and check each stage
    let stages: Vec<&str> = cmd.split('|').collect();
    stages.iter().all(|stage| {
        let trimmed = stage.trim();
        // Strip leading env vars (KEY=val cmd ...)
        let without_env = trimmed
            .split_whitespace()
            .skip_while(|tok| tok.contains('=') && !tok.starts_with('-'))
            .collect::<Vec<_>>()
            .join(" ");
        let effective = without_env.trim();

        READ_ONLY_BINARIES.iter().any(|bin| {
            // Multi-word binaries (e.g. "git log") need prefix match
            if bin.contains(' ') {
                effective.starts_with(bin)
            } else {
                // Single binary: first token matches exactly, or is an absolute path ending in /bin
                let first_token = effective.split_whitespace().next().unwrap_or("");
                first_token == *bin || first_token.ends_with(&format!("/{}", bin))
            }
        })
    })
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

        // Non-zero exit returns Ok with error details (not Err) so the model
        // can see what happened and adapt its approach.
        match result {
            Ok(tool_result) => {
                assert!(tool_result.content.contains("before-fail"));
                assert!(tool_result.content.contains("Exit code: 3"));
            }
            Err(e) => panic!("expected Ok with error info, got Err: {:?}", e),
        }
    }

    #[test]
    fn read_only_classification() {
        // Clearly read-only
        assert!(is_read_only_command("ls -la"));
        assert!(is_read_only_command("cat foo.txt"));
        assert!(is_read_only_command("grep -r TODO src/"));
        assert!(is_read_only_command("echo hello"));
        assert!(is_read_only_command("git log --oneline -10"));
        assert!(is_read_only_command("git status"));
        assert!(is_read_only_command("cargo check"));
        assert!(is_read_only_command("wc -l src/*.rs"));
        assert!(is_read_only_command("find . -name '*.rs'"));
        assert!(is_read_only_command("rg TODO"));
        assert!(is_read_only_command("head -20 main.rs"));
        assert!(is_read_only_command("python3 -c 'print(1+1)'"));

        // Read-only pipeline
        assert!(is_read_only_command("cat foo.txt | grep bar | wc -l"));

        // Absolute paths to read-only binaries
        assert!(is_read_only_command("/usr/bin/ls -la"));
        assert!(is_read_only_command("/usr/bin/grep foo bar"));

        // With env vars
        assert!(is_read_only_command("LANG=C sort file.txt"));

        // Clearly mutating
        assert!(!is_read_only_command("cargo build"));
        assert!(!is_read_only_command("npm install"));
        assert!(!is_read_only_command("mkdir -p src/new"));
        assert!(!is_read_only_command("rm foo.txt"));
        assert!(!is_read_only_command("cp a.txt b.txt"));
        assert!(!is_read_only_command("mv a.txt b.txt"));
        assert!(!is_read_only_command("touch new.txt"));
        assert!(!is_read_only_command("sed -i 's/foo/bar/' file.txt"));
        assert!(!is_read_only_command("git commit -m 'msg'"));

        // Pipelines with a mutating stage
        assert!(!is_read_only_command("echo hello > out.txt"));
        assert!(!is_read_only_command("cat foo | tee bar.txt"));
    }
}
