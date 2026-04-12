//! Pipit Tmux Bridge
//!
//! Transparent tmux integration for pipit agents. Three layers:
//!
//! 1. **Session** — Create/manage tmux sessions and panes for agent workflows.
//! 2. **Bridge** — Read/type/keys cross-pane protocol (smux-compatible).
//! 3. **Shell** — Route bash tool commands to visible tmux panes with
//!    prompt detection and output capture.

pub mod bridge;
pub mod session;
pub mod shell;

pub use bridge::{BridgeError, TmuxBridge};
pub use session::{PaneId, PaneInfo, PaneRole, SessionId, TmuxSession};
pub use shell::{ShellEvent, TmuxShell};

/// Check whether tmux is available on this system.
pub fn is_tmux_available() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Check whether we are currently inside a tmux session.
pub fn inside_tmux() -> bool {
    std::env::var("TMUX").is_ok()
}

/// Get the tmux version string (e.g. "tmux 3.4").
pub fn tmux_version() -> Option<String> {
    let output = std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}
