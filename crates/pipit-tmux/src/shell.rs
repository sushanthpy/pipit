//! TmuxShell — route bash commands through a visible tmux pane.
//!
//! Instead of running bash in a hidden subprocess, TmuxShell sends commands
//! to a tmux pane where the user can watch them execute in real-time.
//! Output is captured via `tmux capture-pane` after the command finishes.
//!
//! Prompt detection uses a unique marker injected via PS1 to reliably
//! detect when a command has completed.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::time::sleep;

use crate::bridge::TmuxBridge;
use crate::session::PaneRole;

/// Unique marker prefix for detecting command completion.
const PROMPT_MARKER: &str = "◊PIPIT_DONE◊";

/// Events emitted during shell execution for TUI integration.
#[derive(Debug, Clone)]
pub enum ShellEvent {
    /// Command was sent to the pane.
    CommandSent { pane_id: String, command: String },
    /// Incremental output captured (for streaming to TUI).
    OutputDelta { lines: Vec<String> },
    /// Command finished with exit code.
    CommandDone { exit_code: i32, duration: Duration },
    /// Pane became idle (prompt returned).
    PaneIdle { pane_id: String },
}

/// TmuxShell executes commands in a visible tmux pane and captures output.
///
/// This is the core integration: every bash tool call goes through here
/// instead of spawning a hidden subprocess.
pub struct TmuxShell {
    /// The pane to execute commands in.
    pane_id: String,
    /// Bridge for pane interaction.
    bridge: Arc<Mutex<TmuxBridge>>,
    /// Current working directory (synced with ToolContext).
    cwd: Arc<Mutex<PathBuf>>,
    /// Whether the shell has been initialized with our prompt marker.
    initialized: bool,
    /// Event callback for TUI integration.
    event_tx: Option<tokio::sync::broadcast::Sender<ShellEvent>>,
}

impl TmuxShell {
    /// Create a TmuxShell targeting a specific pane.
    pub fn new(pane_id: String, bridge: Arc<Mutex<TmuxBridge>>) -> Self {
        let cwd = Arc::new(Mutex::new(PathBuf::from("/")));
        Self {
            pane_id,
            bridge,
            cwd,
            initialized: false,
            event_tx: None,
        }
    }

    /// Create with an event channel for TUI integration.
    pub fn with_events(
        pane_id: String,
        bridge: Arc<Mutex<TmuxBridge>>,
        event_tx: tokio::sync::broadcast::Sender<ShellEvent>,
    ) -> Self {
        let cwd = Arc::new(Mutex::new(PathBuf::from("/")));
        Self {
            pane_id,
            bridge,
            cwd,
            initialized: false,
            event_tx: Some(event_tx),
        }
    }

    /// Get the pane ID.
    pub fn pane_id(&self) -> &str {
        &self.pane_id
    }

    /// Initialize the shell pane with our prompt marker.
    ///
    /// Sets a custom PS1 that includes a unique marker so we can reliably
    /// detect when commands complete by scanning capture-pane output.
    pub fn initialize(&mut self) -> Result<(), ShellError> {
        if self.initialized {
            return Ok(());
        }

        let mut bridge = self.bridge.lock().unwrap();

        // Set a prompt that includes our marker + exit code of last command.
        // Format: ◊PIPIT_DONE◊<exit_code>◊
        let ps1_cmd = format!(
            r#"export PS1='{}$?◊ $ ' && export PROMPT_COMMAND='' && clear"#,
            PROMPT_MARKER
        );
        bridge.type_and_enter(&self.pane_id, &ps1_cmd)?;

        // Wait for the prompt to settle.
        std::thread::sleep(Duration::from_millis(200));

        self.initialized = true;
        tracing::debug!(pane = %self.pane_id, "tmux shell initialized with prompt marker");
        Ok(())
    }

    /// Execute a command in the tmux pane and capture output.
    ///
    /// This is the main entry point — called by the bash tool executor.
    /// Returns (stdout, exit_code) after the command finishes.
    pub async fn execute(
        &mut self,
        command: &str,
        cwd: &Path,
        timeout: Duration,
    ) -> Result<ShellOutput, ShellError> {
        if !self.initialized {
            self.initialize().map_err(|e| {
                ShellError::ExecutionFailed(format!("shell init failed: {}", e))
            })?;
        }

        // Sync working directory if it changed.
        let current_cwd = self.cwd.lock().unwrap().clone();
        if current_cwd != cwd {
            self.cd(cwd).await?;
        }

        // Capture pre-command pane state (line count) for delta detection.
        let pre_output = {
            let mut bridge = self.bridge.lock().unwrap();
            bridge.read(&self.pane_id, 500)?
        };
        let pre_line_count = pre_output.lines().count();

        // Send the command.
        {
            let mut bridge = self.bridge.lock().unwrap();
            bridge.type_and_enter(&self.pane_id, command)?;
        }

        self.emit(ShellEvent::CommandSent {
            pane_id: self.pane_id.clone(),
            command: command.to_string(),
        });

        let start = Instant::now();

        // Poll for completion — look for our prompt marker.
        let result = self.wait_for_completion(timeout, pre_line_count).await?;

        let duration = start.elapsed();

        self.emit(ShellEvent::CommandDone {
            exit_code: result.exit_code,
            duration,
        });

        // Update tracked cwd from the pane's actual directory.
        if let Ok(pane_cwd) = TmuxBridge::pane_cwd(&self.pane_id) {
            *self.cwd.lock().unwrap() = pane_cwd;
        }

        Ok(result)
    }

    /// Change directory in the tmux pane.
    async fn cd(&mut self, path: &Path) -> Result<(), ShellError> {
        let path_str = path.to_string_lossy();
        {
            let mut bridge = self.bridge.lock().unwrap();
            bridge.type_and_enter(&self.pane_id, &format!("cd {}", shell_escape(&path_str)))?;
        }
        // Brief wait for cd to complete.
        sleep(Duration::from_millis(100)).await;
        *self.cwd.lock().unwrap() = path.to_path_buf();
        Ok(())
    }

    /// Poll the pane until we see our prompt marker indicating command completion.
    async fn wait_for_completion(
        &mut self,
        timeout: Duration,
        pre_line_count: usize,
    ) -> Result<ShellOutput, ShellError> {
        let deadline = Instant::now() + timeout;
        let mut poll_interval = Duration::from_millis(50);
        let max_interval = Duration::from_millis(500);
        let mut last_line_count = pre_line_count;

        loop {
            if Instant::now() > deadline {
                // Timeout — send C-c to interrupt and capture what we have.
                let _ = self.bridge.lock().unwrap().interrupt(&self.pane_id);
                return Err(ShellError::Timeout(timeout.as_secs()));
            }

            sleep(poll_interval).await;

            // Capture current pane content.
            let content = {
                let mut bridge = self.bridge.lock().unwrap();
                bridge.read(&self.pane_id, 500)?
            };

            let lines: Vec<&str> = content.lines().collect();
            let line_count = lines.len();

            // Emit deltas if new lines appeared.
            if line_count > last_line_count {
                let new_lines: Vec<String> = lines[last_line_count..]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                self.emit(ShellEvent::OutputDelta {
                    lines: new_lines,
                });
                last_line_count = line_count;
            }

            // Look for our prompt marker in recent lines (scan last 5 lines).
            let scan_start = lines.len().saturating_sub(5);
            for line in &lines[scan_start..] {
                if let Some(rest) = line.strip_prefix(PROMPT_MARKER) {
                    // Extract exit code: ◊PIPIT_DONE◊<code>◊ $
                    let exit_code = rest
                        .trim_start_matches(|c: char| !c.is_ascii_digit())
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect::<String>()
                        .parse::<i32>()
                        .unwrap_or(0);

                    // Extract command output: everything between the sent command
                    // and the prompt marker.
                    let output = extract_output(&content, PROMPT_MARKER);

                    return Ok(ShellOutput {
                        stdout: output,
                        stderr: String::new(),
                        exit_code,
                    });
                }
            }

            // Adaptive polling: slow down if nothing is happening.
            poll_interval = (poll_interval * 2).min(max_interval);
            if line_count > last_line_count {
                // Output is flowing — poll faster.
                poll_interval = Duration::from_millis(50);
            }
        }
    }

    /// Send interrupt (C-c) to the shell pane.
    pub fn interrupt(&mut self) -> Result<(), ShellError> {
        self.bridge.lock().unwrap().interrupt(&self.pane_id)?;
        Ok(())
    }

    fn emit(&self, event: ShellEvent) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(event);
        }
    }
}

/// Output from a tmux shell command execution.
#[derive(Debug, Clone)]
pub struct ShellOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    #[error("shell execution failed: {0}")]
    ExecutionFailed(String),
    #[error("command timed out after {0}s")]
    Timeout(u64),
    #[error("bridge error: {0}")]
    Bridge(#[from] crate::bridge::BridgeError),
}

/// Extract command output from captured pane content.
///
/// Finds the text between the last two prompt markers, which is the
/// output of the most recent command.
fn extract_output(content: &str, marker: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();

    // Find the last occurrence of the marker (end of output).
    let end_idx = lines
        .iter()
        .rposition(|line| line.contains(marker))
        .unwrap_or(lines.len());

    // Find the second-to-last occurrence (start = just after the command prompt).
    let start_idx = lines[..end_idx]
        .iter()
        .rposition(|line| line.contains(marker))
        .map(|i| i + 1)  // skip the prompt line itself
        .unwrap_or(0);

    // Skip the first line after start (the command itself was echoed).
    let cmd_start = if start_idx < end_idx {
        start_idx + 1
    } else {
        start_idx
    };

    if cmd_start < end_idx {
        lines[cmd_start..end_idx]
            .join("\n")
            .trim()
            .to_string()
    } else {
        String::new()
    }
}

/// Shell-escape a string for safe inclusion in a bash command.
fn shell_escape(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_alphanumeric() || c == '/' || c == '.' || c == '-' || c == '_')
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_output() {
        let content = "\
◊PIPIT_DONE◊0◊ $ ls -la
total 16
-rw-r--r--  1 user  staff  100 Apr 11 10:00 file.txt
drwxr-xr-x  3 user  staff   96 Apr 11 10:00 src
◊PIPIT_DONE◊0◊ $ ";

        let output = extract_output(content, "◊PIPIT_DONE◊");
        assert!(output.contains("file.txt"));
        assert!(output.contains("src"));
    }

    #[test]
    fn test_extract_output_with_exit_code() {
        let content = "\
◊PIPIT_DONE◊0◊ $ false
◊PIPIT_DONE◊1◊ $ ";

        let output = extract_output(content, "◊PIPIT_DONE◊");
        assert!(output.is_empty());
    }

    #[test]
    fn test_shell_escape_safe() {
        assert_eq!(shell_escape("/usr/local/bin"), "/usr/local/bin");
        assert_eq!(shell_escape("file.txt"), "file.txt");
    }

    #[test]
    fn test_shell_escape_special() {
        assert_eq!(shell_escape("hello world"), "'hello world'");
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }
}
