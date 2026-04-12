//! Tmux session and pane management.
//!
//! Manages the lifecycle of tmux sessions and panes for pipit agent workflows.
//! Each pipit session creates a tmux session with labeled panes:
//!
//! - **agent** pane: shows agent reasoning, streaming content (classic mode output)
//! - **shell** pane: visible bash execution — every bash tool call runs here
//! - **user** pane: (optional) where user types follow-up prompts
//!
//! Layout: horizontal split — agent (left 60%) | shell (right 40%)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

/// Unique session identifier (tmux session name).
pub type SessionId = String;

/// Tmux pane identifier (e.g. "%5").
pub type PaneId = String;

/// Role-based pane identification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PaneRole {
    /// Agent reasoning/streaming output.
    Agent,
    /// Visible shell for bash tool execution.
    Shell,
    /// User input pane.
    User,
    /// Extra panes created by agents or tools.
    Extra(u8),
}

impl std::fmt::Display for PaneRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PaneRole::Agent => write!(f, "agent"),
            PaneRole::Shell => write!(f, "shell"),
            PaneRole::User => write!(f, "user"),
            PaneRole::Extra(n) => write!(f, "extra-{}", n),
        }
    }
}

/// Information about a single tmux pane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneInfo {
    pub id: PaneId,
    pub role: PaneRole,
    pub width: u16,
    pub height: u16,
    pub current_command: String,
    pub current_path: PathBuf,
    pub is_active: bool,
}

/// A managed tmux session for a pipit agent run.
#[derive(Debug)]
pub struct TmuxSession {
    /// Tmux session name.
    session_name: String,
    /// Pane IDs by role.
    panes: HashMap<PaneRole, PaneId>,
    /// Working directory for the session.
    cwd: PathBuf,
    /// Socket path override (for testing or multi-server setups).
    socket: Option<String>,
    /// Whether we created this session (vs attached to existing).
    owned: bool,
}

impl TmuxSession {
    /// Create a new tmux session for a pipit agent run.
    ///
    /// Creates a detached session with two panes: agent (left) and shell (right).
    /// The shell pane starts in the project root directory.
    pub fn create(project_root: &Path, session_name: Option<&str>) -> Result<Self, SessionError> {
        let name = session_name
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("pipit-{}", &uuid_short()));

        let cwd = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());

        // Create detached session — this becomes the "shell" pane (pane 0).
        let output = tmux_cmd(&[
            "new-session",
            "-d",
            "-s",
            &name,
            "-c",
            &cwd.to_string_lossy(),
            "-x",
            "200",
            "-y",
            "50",
            "-P",
            "-F",
            "#{pane_id}",
        ])?;
        let shell_pane_id = output.trim().to_string();

        // Split horizontally: left 60% (agent) | right 40% (shell keeps original).
        // The split creates a new pane (agent) to the left of the shell.
        let output = tmux_cmd(&[
            "split-window",
            "-h",
            "-b",         // insert before (left of current)
            "-p",
            "60",
            "-t",
            &format!("{}:{}", name, shell_pane_id),
            "-c",
            &cwd.to_string_lossy(),
            "-P",
            "-F",
            "#{pane_id}",
        ])?;
        let agent_pane_id = output.trim().to_string();

        // Label panes for identification.
        let _ = tmux_cmd(&[
            "select-pane",
            "-t",
            &agent_pane_id,
            "-T",
            "pipit:agent",
        ]);
        let _ = tmux_cmd(&[
            "select-pane",
            "-t",
            &shell_pane_id,
            "-T",
            "pipit:shell",
        ]);

        // Focus the shell pane by default (user watches commands run).
        let _ = tmux_cmd(&["select-pane", "-t", &shell_pane_id]);

        let mut panes = HashMap::new();
        panes.insert(PaneRole::Agent, agent_pane_id);
        panes.insert(PaneRole::Shell, shell_pane_id);

        tracing::info!(
            session = %name,
            "tmux session created with agent + shell panes"
        );

        Ok(Self {
            session_name: name,
            panes,
            cwd,
            socket: None,
            owned: true,
        })
    }

    /// Attach to an existing tmux session by name.
    pub fn attach_existing(session_name: &str) -> Result<Self, SessionError> {
        // Verify session exists.
        let output = tmux_cmd(&[
            "list-panes",
            "-t",
            session_name,
            "-F",
            "#{pane_id} #{pane_title}",
        ])?;

        let mut panes = HashMap::new();
        for line in output.lines() {
            let parts: Vec<&str> = line.splitn(2, ' ').collect();
            if parts.len() == 2 {
                let pane_id = parts[0].to_string();
                let title = parts[1];
                let role = match title {
                    t if t.contains("agent") => PaneRole::Agent,
                    t if t.contains("shell") => PaneRole::Shell,
                    t if t.contains("user") => PaneRole::User,
                    _ => continue,
                };
                panes.insert(role, pane_id);
            }
        }

        // If no labeled panes found, assume first pane is shell.
        if panes.is_empty() {
            if let Some(first_line) = output.lines().next() {
                if let Some(id) = first_line.split_whitespace().next() {
                    panes.insert(PaneRole::Shell, id.to_string());
                }
            }
        }

        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));

        Ok(Self {
            session_name: session_name.to_string(),
            panes,
            cwd,
            socket: None,
            owned: false,
        })
    }

    /// Get the session name.
    pub fn name(&self) -> &str {
        &self.session_name
    }

    /// Get the pane ID for a given role.
    pub fn pane(&self, role: PaneRole) -> Option<&str> {
        self.panes.get(&role).map(|s| s.as_str())
    }

    /// Get the shell pane ID (convenience).
    pub fn shell_pane(&self) -> Option<&str> {
        self.pane(PaneRole::Shell)
    }

    /// Get the agent pane ID (convenience).
    pub fn agent_pane(&self) -> Option<&str> {
        self.pane(PaneRole::Agent)
    }

    /// Get the working directory.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// List all panes with current state.
    pub fn list_panes(&self) -> Result<Vec<PaneInfo>, SessionError> {
        let output = tmux_cmd(&[
            "list-panes",
            "-t",
            &self.session_name,
            "-F",
            "#{pane_id}\t#{pane_title}\t#{pane_width}\t#{pane_height}\t#{pane_current_command}\t#{pane_current_path}\t#{pane_active}",
        ])?;

        let mut result = Vec::new();
        for line in output.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() >= 7 {
                let id = fields[0].to_string();
                let title = fields[1];
                let role = self
                    .panes
                    .iter()
                    .find(|(_, v)| v.as_str() == id)
                    .map(|(k, _)| *k)
                    .unwrap_or_else(|| {
                        if title.contains("agent") {
                            PaneRole::Agent
                        } else if title.contains("shell") {
                            PaneRole::Shell
                        } else {
                            PaneRole::Extra(0)
                        }
                    });

                result.push(PaneInfo {
                    id,
                    role,
                    width: fields[2].parse().unwrap_or(80),
                    height: fields[3].parse().unwrap_or(24),
                    current_command: fields[4].to_string(),
                    current_path: PathBuf::from(fields[5]),
                    is_active: fields[6] == "1",
                });
            }
        }
        Ok(result)
    }

    /// Create an additional pane (for multi-agent or extra shells).
    pub fn create_extra_pane(
        &mut self,
        label: &str,
        direction: SplitDirection,
    ) -> Result<PaneId, SessionError> {
        let target = self
            .panes
            .get(&PaneRole::Shell)
            .cloned()
            .unwrap_or_else(|| self.session_name.clone());

        let dir_flag = match direction {
            SplitDirection::Horizontal => "-h",
            SplitDirection::Vertical => "-v",
        };

        let output = tmux_cmd(&[
            "split-window",
            dir_flag,
            "-t",
            &target,
            "-c",
            &self.cwd.to_string_lossy(),
            "-P",
            "-F",
            "#{pane_id}",
        ])?;

        let pane_id = output.trim().to_string();
        let _ = tmux_cmd(&["select-pane", "-t", &pane_id, "-T", label]);

        // Find next extra slot.
        let slot = (0..255u8)
            .find(|n| !self.panes.contains_key(&PaneRole::Extra(*n)))
            .unwrap_or(0);
        self.panes.insert(PaneRole::Extra(slot), pane_id.clone());

        Ok(pane_id)
    }

    /// Kill the session (cleanup). Only kills sessions we created.
    pub fn kill(&self) -> Result<(), SessionError> {
        if self.owned {
            let _ = tmux_cmd(&["kill-session", "-t", &self.session_name]);
            tracing::info!(session = %self.session_name, "tmux session killed");
        }
        Ok(())
    }

    /// Check if the session is still alive.
    pub fn is_alive(&self) -> bool {
        tmux_cmd(&["has-session", "-t", &self.session_name]).is_ok()
    }
}

impl Drop for TmuxSession {
    fn drop(&mut self) {
        // Don't auto-kill — let the user inspect the session after the agent finishes.
        // The user can `tmux kill-session -t <name>` manually.
        tracing::debug!(session = %self.session_name, "TmuxSession dropped (session preserved)");
    }
}

/// Direction for splitting panes.
#[derive(Debug, Clone, Copy)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("tmux not found — install tmux to use this feature")]
    TmuxNotFound,
    #[error("tmux command failed: {0}")]
    CommandFailed(String),
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("pane not found: {role}")]
    PaneNotFound { role: String },
}

// ─── Internal helpers ────────────────────────────────────────────────

/// Run a tmux command and return stdout.
fn tmux_cmd(args: &[&str]) -> Result<String, SessionError> {
    let output = Command::new("tmux")
        .args(args)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SessionError::TmuxNotFound
            } else {
                SessionError::CommandFailed(format!("spawn failed: {}", e))
            }
        })?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(SessionError::CommandFailed(format!(
            "tmux {} → {}",
            args.first().unwrap_or(&"?"),
            stderr
        )))
    }
}

/// Generate a short unique ID for session naming.
fn uuid_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{:x}", t & 0xFFFFFF)
}
