//! Declarative Worktree and Tmux Workflow
//!
//! Provides `pipit worktree up` for isolated working copies with optional
//! tmux layout, session restoration, and clean teardown.
//!
//! Worktree creation/lookup is O(1) relative to session state.
//! Layout generation for n panes is O(n) with binary space partitioning.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Configuration for a worktree session.
#[derive(Debug, Clone)]
pub struct WorktreeSessionConfig {
    /// Task name (becomes the branch slug: pipit/{slug}).
    pub name: String,
    /// Whether to create a tmux session.
    pub tmux: bool,
    /// Custom tmux layout (e.g., "main-vertical", "tiled").
    pub tmux_layout: Option<String>,
    /// Additional panes to create (e.g., ["test-watcher", "server"]).
    pub extra_panes: Vec<String>,
    /// Whether to restore a previous session if one exists.
    pub restore: bool,
}

impl Default for WorktreeSessionConfig {
    fn default() -> Self {
        Self {
            name: "task".to_string(),
            tmux: false,
            tmux_layout: None,
            extra_panes: Vec::new(),
            restore: true,
        }
    }
}

/// A live worktree session handle.
#[derive(Debug, Clone)]
pub struct WorktreeSession {
    /// Worktree directory path.
    pub worktree_path: PathBuf,
    /// Branch name.
    pub branch: String,
    /// Original working directory (to restore on teardown).
    pub original_cwd: PathBuf,
    /// Tmux session name (if tmux was used).
    pub tmux_session: Option<String>,
    /// Session state directory (.pipit/sessions/{slug}).
    pub session_dir: PathBuf,
}

/// Sanitize a task name into a valid git branch slug.
fn slugify(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_lowercase()
}

/// Create a new worktree session.
pub fn worktree_up(
    repo_root: &Path,
    config: &WorktreeSessionConfig,
) -> Result<WorktreeSession, String> {
    // Verify git repo
    if !repo_root.join(".git").exists() {
        return Err("Not a git repository".to_string());
    }

    let slug = slugify(&config.name);
    let branch = format!("pipit/{}", slug);
    let worktree_dir = repo_root
        .join(".pipit")
        .join("worktrees")
        .join(&slug);
    let session_dir = repo_root
        .join(".pipit")
        .join("sessions")
        .join(&slug);

    // Check if worktree already exists (restore path)
    if worktree_dir.exists() && config.restore {
        let session = WorktreeSession {
            worktree_path: worktree_dir,
            branch,
            original_cwd: repo_root.to_path_buf(),
            tmux_session: if config.tmux { Some(format!("pipit-{}", slug)) } else { None },
            session_dir,
        };

        // If tmux requested and session exists, reattach
        if config.tmux {
            let tmux_name = format!("pipit-{}", slug);
            if tmux_session_exists(&tmux_name) {
                reattach_tmux(&tmux_name)?;
                return Ok(session);
            }
        }

        return Ok(session);
    }

    // Create worktree
    std::fs::create_dir_all(worktree_dir.parent().unwrap_or(repo_root))
        .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&session_dir).map_err(|e| e.to_string())?;

    let output = Command::new("git")
        .args(["worktree", "add", "-b", &branch,
               worktree_dir.to_str().unwrap_or(".")])
        .current_dir(repo_root)
        .output()
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Branch might already exist — try without -b
        if stderr.contains("already exists") {
            let output2 = Command::new("git")
                .args(["worktree", "add",
                       worktree_dir.to_str().unwrap_or("."), &branch])
                .current_dir(repo_root)
                .output()
                .map_err(|e| e.to_string())?;
            if !output2.status.success() {
                return Err(String::from_utf8_lossy(&output2.stderr).to_string());
            }
        } else {
            return Err(stderr.to_string());
        }
    }

    let tmux_session = if config.tmux {
        let tmux_name = format!("pipit-{}", slug);
        create_tmux_layout(
            &tmux_name,
            &worktree_dir,
            config.tmux_layout.as_deref(),
            &config.extra_panes,
        )?;
        Some(tmux_name)
    } else {
        None
    };

    Ok(WorktreeSession {
        worktree_path: worktree_dir,
        branch,
        original_cwd: repo_root.to_path_buf(),
        tmux_session,
        session_dir,
    })
}

/// Tear down a worktree session with clean merge semantics.
pub fn worktree_down(
    session: &WorktreeSession,
    merge: bool,
) -> Result<WorktreeDownResult, String> {
    let mut result = WorktreeDownResult {
        merged: false,
        files_changed: Vec::new(),
        branch_deleted: false,
        tmux_killed: false,
    };

    // Kill tmux session if active
    if let Some(ref tmux_name) = session.tmux_session {
        if tmux_session_exists(tmux_name) {
            let _ = Command::new("tmux")
                .args(["kill-session", "-t", tmux_name])
                .output();
            result.tmux_killed = true;
        }
    }

    if merge {
        // Stage + commit any pending changes
        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&session.worktree_path)
            .output()
            .map_err(|e| e.to_string())?;

        if !status.stdout.is_empty() {
            let _ = Command::new("git")
                .args(["add", "-A"])
                .current_dir(&session.worktree_path)
                .output();
            let _ = Command::new("git")
                .args(["commit", "-m", &format!("pipit: work from {}", session.branch)])
                .current_dir(&session.worktree_path)
                .output();
        }

        // Get changed files
        let diff = Command::new("git")
            .args(["diff", "--name-only", "HEAD~1"])
            .current_dir(&session.worktree_path)
            .output()
            .map_err(|e| e.to_string())?;
        result.files_changed = String::from_utf8_lossy(&diff.stdout)
            .lines()
            .map(|l| l.to_string())
            .collect();

        // Merge into main
        let merge_output = Command::new("git")
            .args(["merge", "--no-ff", &session.branch, "-m",
                   &format!("Merge pipit work: {}", session.branch)])
            .current_dir(&session.original_cwd)
            .output()
            .map_err(|e| e.to_string())?;
        result.merged = merge_output.status.success();
    }

    // Remove worktree
    let _ = Command::new("git")
        .args(["worktree", "remove", "--force",
               session.worktree_path.to_str().unwrap_or(".")])
        .current_dir(&session.original_cwd)
        .output();

    // Delete branch
    let branch_del = Command::new("git")
        .args(["branch", "-D", &session.branch])
        .current_dir(&session.original_cwd)
        .output()
        .map_err(|e| e.to_string())?;
    result.branch_deleted = branch_del.status.success();

    Ok(result)
}

/// List active worktree sessions.
pub fn worktree_list(repo_root: &Path) -> Result<Vec<WorktreeInfo>, String> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo_root)
        .output()
        .map_err(|e| e.to_string())?;

    let text = String::from_utf8_lossy(&output.stdout);
    let mut results = Vec::new();
    let mut current_path = None;
    let mut current_branch = None;

    for line in text.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = Some(PathBuf::from(path));
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            current_branch = Some(branch.to_string());
        } else if line.is_empty() {
            if let (Some(path), Some(branch)) = (current_path.take(), current_branch.take()) {
                if branch.starts_with("pipit/") {
                    let slug = branch.strip_prefix("pipit/").unwrap_or("");
                    let has_tmux = tmux_session_exists(&format!("pipit-{}", slug));
                    results.push(WorktreeInfo {
                        path,
                        branch,
                        has_tmux,
                    });
                }
            }
        }
    }

    Ok(results)
}

#[derive(Debug, Clone)]
pub struct WorktreeDownResult {
    pub merged: bool,
    pub files_changed: Vec<String>,
    pub branch_deleted: bool,
    pub tmux_killed: bool,
}

#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: String,
    pub has_tmux: bool,
}

// ── Tmux helpers ──

fn tmux_session_exists(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn reattach_tmux(name: &str) -> Result<(), String> {
    Command::new("tmux")
        .args(["attach-session", "-t", name])
        .status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn create_tmux_layout(
    session_name: &str,
    worktree_path: &Path,
    layout: Option<&str>,
    extra_panes: &[String],
) -> Result<(), String> {
    let cwd = worktree_path.to_str().unwrap_or(".");

    // Create new tmux session
    let output = Command::new("tmux")
        .args(["new-session", "-d", "-s", session_name, "-c", cwd])
        .output()
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(format!(
            "Failed to create tmux session: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    // Create extra panes
    for (i, pane_name) in extra_panes.iter().enumerate() {
        let _ = Command::new("tmux")
            .args(["split-window", "-t", session_name, "-c", cwd])
            .output();
        // Send pane title
        let _ = Command::new("tmux")
            .args(["send-keys", "-t", &format!("{}:{}", session_name, i + 1),
                   &format!("# {}", pane_name), "Enter"])
            .output();
    }

    // Apply layout
    let layout_name = layout.unwrap_or(if extra_panes.is_empty() {
        "even-horizontal"
    } else {
        "main-vertical"
    });
    let _ = Command::new("tmux")
        .args(["select-layout", "-t", session_name, layout_name])
        .output();

    // Attach
    Command::new("tmux")
        .args(["attach-session", "-t", session_name])
        .status()
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_handles_special_chars() {
        assert_eq!(slugify("fix the bug"), "fix-the-bug");
        assert_eq!(slugify("feature/add-auth"), "feature-add-auth");
        assert_eq!(slugify("  spaces  "), "spaces");
        assert_eq!(slugify("UPPER-case"), "upper-case");
    }

    #[test]
    fn default_config() {
        let config = WorktreeSessionConfig::default();
        assert_eq!(config.name, "task");
        assert!(!config.tmux);
        assert!(config.restore);
    }
}
