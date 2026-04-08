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
    #[error("Invalid state transition: {from:?} → {to:?}")]
    InvalidTransition {
        from: PromotionState,
        to: PromotionState,
    },
    #[error("Policy check failed: {0}")]
    PolicyFailed(String),
    #[error("Merge conflict: {0}")]
    MergeConflict(String),
}

/// Promotion state machine for transactional merge gate.
///
/// State transitions are monotone and irreversible except rollback:
///   IsolatedEdit → Verified → PolicyChecked → DryRunMerged → Committed → Cleanup
///
/// Each transition validates preconditions. This replaces the brittle
/// "merge and inspect stderr" approach with state correctness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionState {
    /// Changes are isolated in the worktree branch.
    IsolatedEdit,
    /// Verification (tests, lint) has passed.
    Verified,
    /// Policy checks have passed (protected paths, size limits, etc.).
    PolicyChecked,
    /// Dry-run merge succeeded without conflicts.
    DryRunMerged,
    /// Changes are committed and merged into the target branch.
    Committed,
    /// Worktree and branch have been cleaned up.
    Cleanup,
    /// Promotion was rolled back (terminal state).
    RolledBack,
}

/// Handle to an active git worktree with promotion state tracking.
pub struct WorktreeHandle {
    pub path: PathBuf,
    pub branch: String,
    repo_root: PathBuf,
    /// Current state in the promotion FSM.
    pub state: PromotionState,
}

impl WorktreeHandle {
    pub fn project_root(&self) -> &Path {
        &self.path
    }

    /// Get changed files in this worktree.
    pub fn changed_files(&self) -> Result<Vec<String>, WorktreeError> {
        let output = Command::new("git")
            .args(&["status", "--porcelain"])
            .current_dir(&self.path)
            .output()?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }
}

/// Policy function for worktree promotion.
/// Returns Ok(()) if the changes satisfy policy, Err with reason if not.
pub type PromotionPolicy =
    Box<dyn Fn(&WorktreeHandle, &[String]) -> Result<(), String> + Send + Sync>;

/// Default promotion policy: reject protected path modifications.
pub fn default_promotion_policy(protected_paths: Vec<String>) -> PromotionPolicy {
    Box::new(move |_handle, files_changed| {
        for file in files_changed {
            for protected in &protected_paths {
                if file.contains(protected) {
                    return Err(format!(
                        "cannot promote: file '{}' matches protected path '{}'",
                        file, protected
                    ));
                }
            }
        }
        Ok(())
    })
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
            state: PromotionState::IsolatedEdit,
        })
    }

    /// Transition: IsolatedEdit → Verified
    /// Run verification commands (test/lint) on the worktree.
    pub fn verify(
        &self,
        handle: &mut WorktreeHandle,
        test_command: Option<&str>,
        lint_command: Option<&str>,
    ) -> Result<(), WorktreeError> {
        if handle.state != PromotionState::IsolatedEdit {
            return Err(WorktreeError::InvalidTransition {
                from: handle.state,
                to: PromotionState::Verified,
            });
        }

        // Run test command if configured
        if let Some(cmd) = test_command {
            let output = Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(&handle.path)
                .output()?;
            if !output.status.success() {
                return Err(WorktreeError::PolicyFailed(format!(
                    "test command failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
        }

        // Run lint command if configured
        if let Some(cmd) = lint_command {
            let output = Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(&handle.path)
                .output()?;
            if !output.status.success() {
                return Err(WorktreeError::PolicyFailed(format!(
                    "lint command failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
        }

        handle.state = PromotionState::Verified;
        Ok(())
    }

    /// Transition: Verified → PolicyChecked
    /// Run policy checks on the changed files.
    pub fn check_policy(
        &self,
        handle: &mut WorktreeHandle,
        policy: &PromotionPolicy,
    ) -> Result<Vec<String>, WorktreeError> {
        if handle.state != PromotionState::Verified {
            return Err(WorktreeError::InvalidTransition {
                from: handle.state,
                to: PromotionState::PolicyChecked,
            });
        }

        let files_changed = handle.changed_files()?;
        policy(handle, &files_changed).map_err(WorktreeError::PolicyFailed)?;

        handle.state = PromotionState::PolicyChecked;
        Ok(files_changed)
    }

    /// Transition: PolicyChecked → DryRunMerged
    /// Stage, commit, and perform a dry-run merge to detect conflicts.
    pub fn dry_run_merge(&self, handle: &mut WorktreeHandle) -> Result<(), WorktreeError> {
        if handle.state != PromotionState::PolicyChecked {
            return Err(WorktreeError::InvalidTransition {
                from: handle.state,
                to: PromotionState::DryRunMerged,
            });
        }

        // Stage and commit
        let _ = Command::new("git")
            .args(&["add", "-A"])
            .current_dir(&handle.path)
            .output()?;

        let _ = Command::new("git")
            .args(&[
                "commit",
                "-m",
                &format!("pipit: work from {}", handle.branch),
            ])
            .current_dir(&handle.path)
            .output()?;

        // Dry-run merge: --no-commit --no-ff to test without applying
        let merge_test = Command::new("git")
            .args(&["merge", "--no-commit", "--no-ff", &handle.branch])
            .current_dir(&self.repo_root)
            .output()?;

        if !merge_test.status.success() {
            let stderr = String::from_utf8_lossy(&merge_test.stderr);
            // Abort the failed merge attempt
            let _ = Command::new("git")
                .args(&["merge", "--abort"])
                .current_dir(&self.repo_root)
                .output();
            return Err(WorktreeError::MergeConflict(stderr.to_string()));
        }

        // Abort the dry-run (we didn't want to actually merge yet)
        let _ = Command::new("git")
            .args(&["merge", "--abort"])
            .current_dir(&self.repo_root)
            .output();

        handle.state = PromotionState::DryRunMerged;
        Ok(())
    }

    /// Transition: DryRunMerged → Committed
    /// Perform the actual merge.
    pub fn commit_merge(&self, handle: &mut WorktreeHandle) -> Result<MergeResult, WorktreeError> {
        if handle.state != PromotionState::DryRunMerged {
            return Err(WorktreeError::InvalidTransition {
                from: handle.state,
                to: PromotionState::Committed,
            });
        }

        // Get changed files for the result
        let diff = Command::new("git")
            .args(&["diff", "--name-only", "HEAD~1"])
            .current_dir(&handle.path)
            .output()?;
        let files_changed: Vec<String> = String::from_utf8_lossy(&diff.stdout)
            .lines()
            .map(|l| l.to_string())
            .collect();

        // Actual merge
        let merge = Command::new("git")
            .args(&[
                "merge",
                "--no-ff",
                &handle.branch,
                "-m",
                &format!("Merge pipit agent work from {}", handle.branch),
            ])
            .current_dir(&self.repo_root)
            .output()?;

        if !merge.status.success() {
            let stderr = String::from_utf8_lossy(&merge.stderr);
            // This shouldn't happen after a successful dry-run, but handle it
            let _ = Command::new("git")
                .args(&["merge", "--abort"])
                .current_dir(&self.repo_root)
                .output();
            return Err(WorktreeError::MergeConflict(stderr.to_string()));
        }

        handle.state = PromotionState::Committed;
        Ok(MergeResult {
            success: true,
            files_changed,
            conflict: false,
            message: String::new(),
        })
    }

    /// Transition: Committed → Cleanup
    /// Remove worktree and delete branch.
    pub fn cleanup(&self, handle: &mut WorktreeHandle) -> Result<(), WorktreeError> {
        if handle.state != PromotionState::Committed && handle.state != PromotionState::RolledBack {
            return Err(WorktreeError::InvalidTransition {
                from: handle.state,
                to: PromotionState::Cleanup,
            });
        }
        self.remove_worktree_and_branch(handle)?;
        handle.state = PromotionState::Cleanup;
        Ok(())
    }

    /// Rollback: any state → RolledBack
    /// Abort any in-progress merge and clean up.
    pub fn rollback(&self, handle: &mut WorktreeHandle) -> Result<(), WorktreeError> {
        // Abort any in-progress merge on the main repo
        let _ = Command::new("git")
            .args(&["merge", "--abort"])
            .current_dir(&self.repo_root)
            .output();
        handle.state = PromotionState::RolledBack;
        self.remove_worktree_and_branch(handle)?;
        handle.state = PromotionState::Cleanup;
        Ok(())
    }

    /// Legacy merge_and_cleanup — kept for backward compatibility.
    /// Uses the FSM internally but presents the old API.
    pub fn merge_and_cleanup(
        &self,
        mut handle: WorktreeHandle,
    ) -> Result<MergeResult, WorktreeError> {
        // Check if there are any changes
        let status = Command::new("git")
            .args(&["status", "--porcelain"])
            .current_dir(&handle.path)
            .output()?;

        let has_changes = !status.stdout.is_empty();

        if !has_changes {
            self.remove_worktree_and_branch(&mut handle)?;
            return Ok(MergeResult {
                success: true,
                files_changed: Vec::new(),
                conflict: false,
                message: String::new(),
            });
        }

        // Skip verify + policy for backward compat (those are opt-in)
        handle.state = PromotionState::Verified;
        handle.state = PromotionState::PolicyChecked;

        // Stage and commit
        let _ = Command::new("git")
            .args(&["add", "-A"])
            .current_dir(&handle.path)
            .output()?;

        let _ = Command::new("git")
            .args(&[
                "commit",
                "-m",
                &format!("pipit: work from {}", handle.branch),
            ])
            .current_dir(&handle.path)
            .output()?;

        // Get list of changed files
        let diff = Command::new("git")
            .args(&["diff", "--name-only", "HEAD~1"])
            .current_dir(&handle.path)
            .output()?;

        let files_changed: Vec<String> = String::from_utf8_lossy(&diff.stdout)
            .lines()
            .map(|l| l.to_string())
            .collect();

        // Merge into the current branch of the main worktree
        let merge = Command::new("git")
            .args(&[
                "merge",
                "--no-ff",
                &handle.branch,
                "-m",
                &format!("Merge pipit agent work from {}", handle.branch),
            ])
            .current_dir(&self.repo_root)
            .output()?;

        if !merge.status.success() {
            let stderr = String::from_utf8_lossy(&merge.stderr);
            if stderr.contains("CONFLICT") {
                // Clean up even on conflict
                self.remove_worktree_and_branch(&mut handle)?;
                return Ok(MergeResult {
                    success: false,
                    files_changed,
                    conflict: true,
                    message: stderr.to_string(),
                });
            }
            return Err(WorktreeError::Git(stderr.to_string()));
        }

        self.remove_worktree_and_branch(&mut handle)?;

        Ok(MergeResult {
            success: true,
            files_changed,
            conflict: false,
            message: String::new(),
        })
    }

    /// Internal: remove worktree directory and delete branch.
    fn remove_worktree_and_branch(&self, handle: &mut WorktreeHandle) -> Result<(), WorktreeError> {
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

        Ok(())
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
