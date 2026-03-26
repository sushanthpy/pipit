use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error("Git error: {0}")]
    Git(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Not a git repository")]
    NotGitRepo,
}

/// Handle to an active git worktree.
pub struct WorktreeHandle {
    pub path: PathBuf,
    pub branch: String,
    repo_root: PathBuf,
}

impl WorktreeHandle {
    pub fn project_root(&self) -> &Path {
        &self.path
    }
}

/// Manages git worktrees for agent isolation.
pub struct WorktreeManager {
    repo_root: PathBuf,
}

impl WorktreeManager {
    pub fn new(repo_root: &Path) -> Result<Self, WorktreeError> {
        // Verify it's a git repo
        if !repo_root.join(".git").exists() {
            return Err(WorktreeError::NotGitRepo);
        }
        Ok(Self {
            repo_root: repo_root.to_path_buf(),
        })
    }

    /// Create an isolated worktree for an agent.
    pub fn create(&self, name: Option<&str>) -> Result<WorktreeHandle, WorktreeError> {
        let id = name.unwrap_or("agent");
        let branch = format!("pipit/{}-{}", id, &uuid::Uuid::new_v4().to_string()[..8]);
        let worktree_dir = self
            .repo_root
            .join(".pipit")
            .join("worktrees")
            .join(branch.replace('/', "-"));

        std::fs::create_dir_all(worktree_dir.parent().unwrap_or(&self.repo_root))?;

        let output = Command::new("git")
            .args(&[
                "worktree",
                "add",
                "-b",
                &branch,
                worktree_dir.to_str().unwrap_or("."),
            ])
            .current_dir(&self.repo_root)
            .output()?;

        if !output.status.success() {
            return Err(WorktreeError::Git(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        Ok(WorktreeHandle {
            path: worktree_dir,
            branch,
            repo_root: self.repo_root.clone(),
        })
    }

    /// Merge a worktree's changes back and clean up.
    pub fn merge_and_cleanup(&self, handle: WorktreeHandle) -> Result<MergeResult, WorktreeError> {
        // Check if there are any changes
        let status = Command::new("git")
            .args(&["status", "--porcelain"])
            .current_dir(&handle.path)
            .output()?;

        let has_changes = !status.stdout.is_empty();
        let mut files_changed = Vec::new();

        if has_changes {
            // Stage and commit all changes
            let _ = Command::new("git")
                .args(&["add", "-A"])
                .current_dir(&handle.path)
                .output()?;

            let _ = Command::new("git")
                .args(&["commit", "-m", &format!("pipit: work from {}", handle.branch)])
                .current_dir(&handle.path)
                .output()?;

            // Get list of changed files
            let diff = Command::new("git")
                .args(&["diff", "--name-only", "HEAD~1"])
                .current_dir(&handle.path)
                .output()?;

            files_changed = String::from_utf8_lossy(&diff.stdout)
                .lines()
                .map(|l| l.to_string())
                .collect();

            // Merge into the current branch of the main worktree
            let merge = Command::new("git")
                .args(&["merge", "--no-ff", &handle.branch, "-m",
                       &format!("Merge pipit agent work from {}", handle.branch)])
                .current_dir(&self.repo_root)
                .output()?;

            if !merge.status.success() {
                let stderr = String::from_utf8_lossy(&merge.stderr);
                if stderr.contains("CONFLICT") {
                    return Ok(MergeResult {
                        success: false,
                        files_changed,
                        conflict: true,
                        message: stderr.to_string(),
                    });
                }
                return Err(WorktreeError::Git(stderr.to_string()));
            }
        }

        // Clean up worktree
        match Command::new("git")
            .args(&["worktree", "remove", handle.path.to_str().unwrap_or(".")])
            .current_dir(&self.repo_root)
            .output()
        {
            Ok(o) if !o.status.success() => {
                tracing::warn!(
                    "Failed to remove worktree at {}: {}",
                    handle.path.display(),
                    String::from_utf8_lossy(&o.stderr)
                );
            }
            Err(e) => tracing::warn!("Failed to run git worktree remove: {}", e),
            _ => {}
        }

        // Delete branch
        match Command::new("git")
            .args(&["branch", "-d", &handle.branch])
            .current_dir(&self.repo_root)
            .output()
        {
            Ok(o) if !o.status.success() => {
                tracing::warn!(
                    "Failed to delete branch {}: {}",
                    handle.branch,
                    String::from_utf8_lossy(&o.stderr)
                );
            }
            Err(e) => tracing::warn!("Failed to run git branch -d: {}", e),
            _ => {}
        }

        Ok(MergeResult {
            success: true,
            files_changed,
            conflict: false,
            message: String::new(),
        })
    }

    /// List active worktrees.
    pub fn list(&self) -> Result<Vec<String>, WorktreeError> {
        let output = Command::new("git")
            .args(&["worktree", "list", "--porcelain"])
            .current_dir(&self.repo_root)
            .output()?;

        let text = String::from_utf8_lossy(&output.stdout);
        let worktrees: Vec<String> = text
            .lines()
            .filter(|l| l.starts_with("worktree "))
            .map(|l| l.strip_prefix("worktree ").unwrap_or("").to_string())
            .collect();

        Ok(worktrees)
    }
}

#[derive(Debug)]
pub struct MergeResult {
    pub success: bool,
    pub files_changed: Vec<String>,
    pub conflict: bool,
    pub message: String,
}
