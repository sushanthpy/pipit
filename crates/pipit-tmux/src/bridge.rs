//! Cross-pane bridge protocol.
//!
//! Implements the read → type → keys protocol for communicating with tmux panes.
//! Uses a read-guarded tmux bridge convention:
//!
//! - **read**: capture last N lines from a pane
//! - **type**: send literal text to a pane
//! - **keys**: send special keys (Enter, C-c, Escape, etc.)
//! - **message**: send text with sender identification framing
//!
//! The read-guard pattern (must read before write) is enforced to prevent
//! blind pane interaction.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

/// Cross-pane bridge for reading, typing, and sending keys to tmux panes.
#[derive(Debug)]
pub struct TmuxBridge {
    /// Set of pane IDs that have been read (read-guard).
    read_panes: HashSet<String>,
}

impl TmuxBridge {
    pub fn new() -> Self {
        Self {
            read_panes: HashSet::new(),
        }
    }

    /// Capture the last `lines` lines from a pane.
    ///
    /// Marks the pane as "read" (satisfying the read-guard for subsequent
    /// type/keys operations).
    pub fn read(&mut self, pane_id: &str, lines: usize) -> Result<String, BridgeError> {
        let start = -(lines as i64);
        let output = tmux_cmd(&[
            "capture-pane",
            "-t",
            pane_id,
            "-p",        // print to stdout
            "-S",
            &start.to_string(),
        ])?;
        self.read_panes.insert(pane_id.to_string());
        Ok(output)
    }

    /// Type literal text into a pane (without pressing Enter).
    ///
    /// Requires a prior `read()` call on the same pane (read-guard).
    pub fn type_text(&mut self, pane_id: &str, text: &str) -> Result<(), BridgeError> {
        if !self.read_panes.contains(pane_id) {
            return Err(BridgeError::ReadGuardViolation(pane_id.to_string()));
        }

        // Use send-keys with -l (literal) to avoid key interpretation.
        tmux_cmd(&["send-keys", "-t", pane_id, "-l", text])?;
        self.read_panes.remove(pane_id);
        Ok(())
    }

    /// Send special keys to a pane (Enter, C-c, Escape, etc.).
    ///
    /// Requires a prior `read()` call on the same pane (read-guard).
    pub fn send_keys(&mut self, pane_id: &str, keys: &[&str]) -> Result<(), BridgeError> {
        if !self.read_panes.contains(pane_id) {
            return Err(BridgeError::ReadGuardViolation(pane_id.to_string()));
        }

        let mut args = vec!["send-keys", "-t", pane_id];
        args.extend(keys);
        tmux_cmd(&args)?;
        self.read_panes.remove(pane_id);
        Ok(())
    }

    /// Type text and press Enter in one atomic sequence.
    ///
    /// Convenience method: read → type → Enter.
    pub fn type_and_enter(&mut self, pane_id: &str, text: &str) -> Result<(), BridgeError> {
        // Auto-read if not already read (convenience for shell execution).
        if !self.read_panes.contains(pane_id) {
            let _ = self.read(pane_id, 1)?;
        }
        tmux_cmd(&["send-keys", "-t", pane_id, "-l", text])?;
        tmux_cmd(&["send-keys", "-t", pane_id, "Enter"])?;
        self.read_panes.remove(pane_id);
        Ok(())
    }

    /// Send a framed message to a pane (with sender identification).
    ///
    /// Format: `[pipit from:<role> pane:<self_pane>] <text>`
    pub fn message(
        &mut self,
        target_pane: &str,
        from_role: &str,
        self_pane: &str,
        text: &str,
    ) -> Result<(), BridgeError> {
        let framed = format!("[pipit from:{} pane:{}] {}", from_role, self_pane, text);
        self.type_and_enter(target_pane, &framed)
    }

    /// Get the current working directory of a pane.
    pub fn pane_cwd(pane_id: &str) -> Result<PathBuf, BridgeError> {
        let output = tmux_cmd(&[
            "display-message",
            "-t",
            pane_id,
            "-p",
            "#{pane_current_path}",
        ])?;
        Ok(PathBuf::from(output.trim()))
    }

    /// Get the current command running in a pane.
    pub fn pane_command(pane_id: &str) -> Result<String, BridgeError> {
        let output = tmux_cmd(&[
            "display-message",
            "-t",
            pane_id,
            "-p",
            "#{pane_current_command}",
        ])?;
        Ok(output.trim().to_string())
    }

    /// Check if a pane's shell is idle (at a prompt, not running a command).
    pub fn is_pane_idle(pane_id: &str) -> Result<bool, BridgeError> {
        let cmd = Self::pane_command(pane_id)?;
        // Typical idle shell commands.
        Ok(matches!(
            cmd.as_str(),
            "zsh" | "bash" | "fish" | "sh" | "dash" | "ksh" | "tcsh" | "csh"
        ))
    }

    /// Send C-c (interrupt) to a pane.
    pub fn interrupt(&mut self, pane_id: &str) -> Result<(), BridgeError> {
        // Bypass read-guard for interrupts — safety override.
        tmux_cmd(&["send-keys", "-t", pane_id, "C-c"])?;
        Ok(())
    }

    /// Clear a pane's screen.
    pub fn clear(&mut self, pane_id: &str) -> Result<(), BridgeError> {
        tmux_cmd(&["send-keys", "-t", pane_id, "C-l"])?;
        Ok(())
    }

    /// List all panes across all sessions.
    pub fn list_all_panes() -> Result<Vec<PaneListEntry>, BridgeError> {
        let output = tmux_cmd(&[
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}\t#{session_name}\t#{window_index}\t#{pane_index}\t#{pane_title}\t#{pane_current_command}\t#{pane_current_path}\t#{pane_width}\t#{pane_height}",
        ])?;

        let mut result = Vec::new();
        for line in output.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() >= 9 {
                result.push(PaneListEntry {
                    pane_id: f[0].to_string(),
                    session: f[1].to_string(),
                    window: f[2].parse().unwrap_or(0),
                    pane_index: f[3].parse().unwrap_or(0),
                    title: f[4].to_string(),
                    command: f[5].to_string(),
                    path: PathBuf::from(f[6]),
                    width: f[7].parse().unwrap_or(80),
                    height: f[8].parse().unwrap_or(24),
                });
            }
        }
        Ok(result)
    }
}

impl Default for TmuxBridge {
    fn default() -> Self {
        Self::new()
    }
}

/// Entry from listing all tmux panes.
#[derive(Debug, Clone)]
pub struct PaneListEntry {
    pub pane_id: String,
    pub session: String,
    pub window: u32,
    pub pane_index: u32,
    pub title: String,
    pub command: String,
    pub path: PathBuf,
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("must read pane {0} before interacting — call bridge.read() first")]
    ReadGuardViolation(String),
    #[error("tmux command failed: {0}")]
    CommandFailed(String),
    #[error("tmux not found")]
    TmuxNotFound,
}

/// Run a tmux command and return stdout.
fn tmux_cmd(args: &[&str]) -> Result<String, BridgeError> {
    let output = Command::new("tmux")
        .args(args)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                BridgeError::TmuxNotFound
            } else {
                BridgeError::CommandFailed(format!("spawn: {}", e))
            }
        })?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(BridgeError::CommandFailed(format!(
            "tmux {} → {}",
            args.first().unwrap_or(&"?"),
            stderr
        )))
    }
}
