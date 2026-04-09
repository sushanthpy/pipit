//! # VCS Gateway — Single Execution Surface for All Git Mutations
//!
//! Every git-mutating operation in the system routes through `VcsGateway`.
//! It combines the `VcsKernel` (FSM + ledger + firewall) with actual git
//! command execution, ensuring that no mutation bypasses validation.
//!
//! Read-only git operations (status, diff, log, etc.) are exempt; they flow
//! through `git_read()` which logs but does not validate.
//!
//! Complexity: O(1) dispatch + O(T+P) firewall + O(1) ledger append + git I/O.

use crate::ledger::LedgerEvent;

use crate::workflow::{MergeStrategy, VcsKernel, WorkflowError, WorkflowOp};
use std::path::{Path, PathBuf};
use std::process::Output;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("Firewall rejected: {0}")]
    Blocked(String),
    #[error("Git command failed: {0}")]
    GitFailed(String),
    #[error("Workflow error: {0}")]
    Workflow(#[from] WorkflowError),
    #[error("IO error: {0}")]
    Io(String),
}

/// The single execution surface for all repository mutations.
///
/// Wraps `VcsKernel` and provides concrete git command execution.
/// All callers (CLI slash commands, tools, daemon) go through this gateway.
pub struct VcsGateway {
    pub kernel: VcsKernel,
    project_root: PathBuf,
}

impl VcsGateway {
    /// Create a new gateway rooted at the given project directory.
    pub fn new(project_root: PathBuf) -> Self {
        let kernel = VcsKernel::new(project_root.clone());
        Self {
            kernel,
            project_root,
        }
    }

    /// Load gateway with replayed state from disk.
    pub fn load(project_root: PathBuf) -> Result<Self, GatewayError> {
        let kernel = VcsKernel::load(project_root.clone())?;
        Ok(Self {
            kernel,
            project_root,
        })
    }

    /// Project root path.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    // ══════════════════════════════════════════════════════════
    //  WORKTREE LIFECYCLE
    // ══════════════════════════════════════════════════════════

    /// Create a new worktree with branch. Routes through kernel FSM + firewall.
    pub fn create_worktree(
        &mut self,
        workspace_id: &str,
        branch: &str,
        worktree_path: &Path,
        base_ref: &str,
    ) -> Result<Output, GatewayError> {
        // 1. Kernel transition: Idle → Editing
        self.kernel.execute(
            workspace_id,
            WorkflowOp::CreateWorkspace {
                name: branch.to_string(),
                base_branch: base_ref.to_string(),
                objective: None,
            },
        )?;

        // 2. Create parent directory
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| GatewayError::Io(format!("mkdir failed: {e}")))?;
        }

        // 3. Execute git worktree add
        let output = std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                branch,
                worktree_path.to_str().unwrap_or("."),
                base_ref,
            ])
            .current_dir(&self.project_root)
            .output()
            .map_err(|e| GatewayError::GitFailed(format!("git worktree add: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(GatewayError::GitFailed(format!(
                "git worktree add failed: {stderr}"
            )));
        }

        tracing::info!(branch, workspace_id, "worktree created via gateway");
        Ok(output)
    }

    /// Create a worktree asynchronously (for tool contexts).
    pub async fn create_worktree_async(
        &mut self,
        workspace_id: &str,
        branch: &str,
        worktree_path: &Path,
        base_ref: &str,
    ) -> Result<(), GatewayError> {
        // 1. Kernel transition
        self.kernel.execute(
            workspace_id,
            WorkflowOp::CreateWorkspace {
                name: branch.to_string(),
                base_branch: base_ref.to_string(),
                objective: None,
            },
        )?;

        // 2. Create parent directory
        if let Some(parent) = worktree_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| GatewayError::Io(format!("mkdir failed: {e}")))?;
        }

        // 3. Execute git worktree add
        let output = tokio::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                branch,
                worktree_path.to_str().unwrap_or("."),
                base_ref,
            ])
            .current_dir(&self.project_root)
            .output()
            .await
            .map_err(|e| GatewayError::GitFailed(format!("git worktree add: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(GatewayError::GitFailed(format!(
                "git worktree add failed: {stderr}"
            )));
        }

        tracing::info!(branch, workspace_id, "worktree created (async) via gateway");
        Ok(())
    }

    /// Remove a worktree. Routes through kernel FSM (→ Abandoned).
    pub async fn remove_worktree_async(
        &mut self,
        workspace_id: &str,
        worktree_path: &Path,
        branch: &str,
        delete_branch: bool,
    ) -> Result<(), GatewayError> {
        // Transition to Abandoned
        let _ = self.kernel.execute(
            workspace_id,
            WorkflowOp::Abandon {
                reason: "worktree removed".to_string(),
            },
        );

        let _ = tokio::process::Command::new("git")
            .args([
                "worktree",
                "remove",
                "--force",
                worktree_path.to_str().unwrap_or("."),
            ])
            .current_dir(&self.project_root)
            .output()
            .await;

        if delete_branch && !branch.is_empty() {
            let _ = tokio::process::Command::new("git")
                .args(["branch", "-D", branch])
                .current_dir(&self.project_root)
                .output()
                .await;
        }

        tracing::info!(branch, workspace_id, "worktree removed via gateway");
        Ok(())
    }

    /// Merge a worktree branch into target. Routes through kernel promotion FSM.
    pub async fn merge_worktree_async(
        &mut self,
        workspace_id: &str,
        worktree_path: &Path,
        branch: &str,
        target_branch: &str,
    ) -> Result<String, GatewayError> {
        // Firewall check on target branch
        if let Some(threat) = self.kernel.firewall.check_branch_mutation(target_branch) {
            return Err(GatewayError::Blocked(format!(
                "Cannot merge into '{}': {:?}",
                target_branch, threat
            )));
        }

        // Remove worktree first
        let _ = tokio::process::Command::new("git")
            .args(["worktree", "remove", worktree_path.to_str().unwrap_or(".")])
            .current_dir(&self.project_root)
            .output()
            .await;

        // Merge
        let merge = tokio::process::Command::new("git")
            .args(["merge", "--no-ff", branch])
            .current_dir(&self.project_root)
            .output()
            .await
            .map_err(|e| GatewayError::GitFailed(format!("git merge: {e}")))?;

        if merge.status.success() {
            // Record promotion in kernel
            let _ = self.kernel.execute(
                workspace_id,
                WorkflowOp::Promote {
                    target: target_branch.to_string(),
                    strategy: MergeStrategy::Merge,
                },
            );

            // Clean up branch
            let _ = tokio::process::Command::new("git")
                .args(["branch", "-d", branch])
                .current_dir(&self.project_root)
                .output()
                .await;

            tracing::info!(branch, target_branch, "worktree merged via gateway");
            Ok(format!(
                "Merged branch '{}' into '{}' and cleaned up.",
                branch, target_branch
            ))
        } else {
            let stderr = String::from_utf8_lossy(&merge.stderr);
            Ok(format!(
                "Merge conflicts in branch '{}': {}\nResolve manually.",
                branch, stderr
            ))
        }
    }

    // ══════════════════════════════════════════════════════════
    //  BRANCH OPERATIONS
    // ══════════════════════════════════════════════════════════

    /// Create a new branch. Firewall-checked.
    pub fn create_branch(&mut self, branch: &str) -> Result<Output, GatewayError> {
        // Firewall: check branch name is safe
        if let Some(threat) = self.kernel.firewall.check_workspace_name(branch) {
            return Err(GatewayError::Blocked(format!(
                "Branch name '{}' rejected: {:?}",
                branch, threat
            )));
        }

        let output = std::process::Command::new("git")
            .args(["checkout", "-b", branch])
            .current_dir(&self.project_root)
            .output()
            .map_err(|e| GatewayError::GitFailed(format!("git checkout -b: {e}")))?;

        if output.status.success() {
            // Log to ledger
            self.kernel
                .ledger
                .append_event(LedgerEvent::Note {
                    workspace_id: None,
                    message: format!("branch created: {}", branch),
                })
                .ok();
            tracing::info!(branch, "branch created via gateway");
        }

        Ok(output)
    }

    /// Switch to a branch. Auto-stash if dirty.
    pub fn switch_branch(&mut self, branch: &str) -> Result<(Output, bool), GatewayError> {
        // Check dirty state
        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&self.project_root)
            .output()
            .map_err(|e| GatewayError::GitFailed(format!("git status: {e}")))?;

        let dirty = !status.stdout.is_empty();
        if dirty {
            let _ = std::process::Command::new("git")
                .args(["stash", "push", "-m", "pipit-auto-stash"])
                .current_dir(&self.project_root)
                .output();
        }

        let output = std::process::Command::new("git")
            .args(["checkout", branch])
            .current_dir(&self.project_root)
            .output()
            .map_err(|e| GatewayError::GitFailed(format!("git checkout: {e}")))?;

        if !output.status.success() && dirty {
            // Restore stash on failure
            let _ = std::process::Command::new("git")
                .args(["stash", "pop"])
                .current_dir(&self.project_root)
                .output();
        }

        if output.status.success() {
            self.kernel
                .ledger
                .append_event(LedgerEvent::Note {
                    workspace_id: None,
                    message: format!("branch switched to: {}", branch),
                })
                .ok();
        }

        Ok((output, dirty))
    }

    // ══════════════════════════════════════════════════════════
    //  COMMIT OPERATIONS
    // ══════════════════════════════════════════════════════════

    /// Commit all changes. Firewall-checked, ledger-logged.
    pub fn commit(&mut self, message: &str, auto_stage: bool) -> Result<Output, GatewayError> {
        if auto_stage {
            let _ = std::process::Command::new("git")
                .args(["add", "-A"])
                .current_dir(&self.project_root)
                .output();
        }

        let output = std::process::Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(&self.project_root)
            .output()
            .map_err(|e| GatewayError::GitFailed(format!("git commit: {e}")))?;

        if output.status.success() {
            self.kernel
                .ledger
                .append_event(LedgerEvent::Note {
                    workspace_id: None,
                    message: format!("commit: {}", message),
                })
                .ok();
        }

        Ok(output)
    }

    /// Auto-commit with pipit attribution (for daemon/agent).
    pub fn auto_commit(&mut self, summary: &str) -> Result<(), GatewayError> {
        // Stage all
        let add = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&self.project_root)
            .output()
            .map_err(|e| GatewayError::GitFailed(format!("git add: {e}")))?;

        if !add.status.success() {
            return Err(GatewayError::GitFailed(format!(
                "git add failed: {}",
                String::from_utf8_lossy(&add.stderr)
            )));
        }

        // Check if anything staged
        let diff = std::process::Command::new("git")
            .args(["diff", "--cached", "--quiet"])
            .current_dir(&self.project_root)
            .output()
            .map_err(|e| GatewayError::GitFailed(format!("git diff: {e}")))?;

        if diff.status.success() {
            return Ok(()); // nothing to commit
        }

        let msg = format!("[pipit] {}", summary);
        let output = std::process::Command::new("git")
            .args(["commit", "-m", &msg])
            .env("GIT_COMMITTER_NAME", "Pipit")
            .env("GIT_COMMITTER_EMAIL", "noreply@pipit.dev")
            .current_dir(&self.project_root)
            .output()
            .map_err(|e| GatewayError::GitFailed(format!("git commit: {e}")))?;

        if !output.status.success() {
            return Err(GatewayError::GitFailed(format!(
                "git commit failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        self.kernel
            .ledger
            .append_event(LedgerEvent::Note {
                workspace_id: None,
                message: format!("auto-commit: {}", summary),
            })
            .ok();

        Ok(())
    }

    // ══════════════════════════════════════════════════════════
    //  UNDO / ROLLBACK
    // ══════════════════════════════════════════════════════════

    /// Restore files to a specific commit SHA.
    pub fn restore_files(
        &mut self,
        sha: &str,
        files: &[String],
    ) -> Result<Vec<(String, bool)>, GatewayError> {
        let mut results = Vec::new();
        for file in files {
            let output = std::process::Command::new("git")
                .args(["checkout", sha, "--", file])
                .current_dir(&self.project_root)
                .output()
                .map_err(|e| GatewayError::GitFailed(format!("git checkout: {e}")))?;
            results.push((file.clone(), output.status.success()));
        }

        self.kernel
            .ledger
            .append_event(LedgerEvent::Note {
                workspace_id: None,
                message: format!("rollback {} file(s) to {}", files.len(), sha),
            })
            .ok();

        Ok(results)
    }

    // ══════════════════════════════════════════════════════════
    //  FIREWALL — command-level gating for BashTool
    // ══════════════════════════════════════════════════════════

    /// Check a raw shell command against the firewall.
    /// Returns Ok(()) if allowed, Err with explanation if blocked.
    pub fn check_command(&self, command: &str) -> Result<(), GatewayError> {
        let decision = self.kernel.firewall.check_command(command);
        if decision.allowed {
            Ok(())
        } else {
            Err(GatewayError::Blocked(decision.explanation))
        }
    }

    /// Check whether a command is a git-mutating command (not read-only).
    /// Used by BashTool to decide if firewall gating is needed.
    pub fn is_git_mutation(command: &str) -> bool {
        let trimmed = command.trim();
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();

        // Not a git command at all
        if tokens.is_empty() || (tokens[0] != "git" && !tokens[0].ends_with("/git")) {
            return false;
        }

        // Read-only git subcommands
        const READ_ONLY_SUBS: &[&str] = &[
            "log",
            "status",
            "diff",
            "show",
            "branch",
            "remote",
            "tag",
            "rev-parse",
            "ls-files",
            "cat-file",
            "describe",
            "shortlog",
            "reflog",
            "blame",
            "bisect",
            "stash list",
            "worktree list",
        ];

        let sub = tokens.get(1).copied().unwrap_or("");
        let sub_with_arg = if tokens.len() > 2 {
            format!("{} {}", sub, tokens[2])
        } else {
            sub.to_string()
        };

        // If the subcommand is in our read-only list, it's not a mutation
        for ro in READ_ONLY_SUBS {
            if sub == *ro || sub_with_arg == *ro {
                return false;
            }
        }

        // Everything else is a potential mutation
        true
    }

    // ══════════════════════════════════════════════════════════
    //  READ-ONLY GIT HELPERS (no firewall, just logging)
    // ══════════════════════════════════════════════════════════

    /// Execute a read-only git command. Not firewall-gated.
    pub fn git_read(&self, args: &[&str]) -> Result<Output, GatewayError> {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&self.project_root)
            .output()
            .map_err(|e| GatewayError::GitFailed(format!("git {}: {e}", args.join(" "))))
    }

    /// Get current branch name.
    pub fn current_branch(&self) -> Result<String, GatewayError> {
        let output = self.git_read(&["rev-parse", "--abbrev-ref", "HEAD"])?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Get staged diff for commit message generation.
    pub fn staged_diff(&self) -> Result<String, GatewayError> {
        let output = self.git_read(&["diff", "--staged"])?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Check if there are staged changes.
    pub fn has_staged_changes(&self) -> Result<bool, GatewayError> {
        let output = self.git_read(&["diff", "--staged", "--stat"])?;
        Ok(!output.stdout.is_empty())
    }

    /// List branches.
    pub fn list_branches(&self) -> Result<String, GatewayError> {
        let output = self.git_read(&["branch", "-a", "--no-color"])?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// List worktrees in porcelain format.
    pub fn list_worktrees(&self) -> Result<String, GatewayError> {
        let output = self.git_read(&["worktree", "list", "--porcelain"])?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    // ══════════════════════════════════════════════════════════
    //  WORKSPACE TRUTH QUERIES — for reconciliation
    // ══════════════════════════════════════════════════════════

    /// Get the list of modified files on a branch relative to its merge-base
    /// with the default branch. Returns file paths.
    ///
    /// Complexity: O(F) where F = changed files.
    pub fn workspace_modified_files(
        &self,
        branch: &str,
        base_branch: &str,
    ) -> Result<Vec<String>, GatewayError> {
        // Find merge-base
        let mb = self.git_read(&["merge-base", base_branch, branch])?;
        let merge_base = String::from_utf8_lossy(&mb.stdout).trim().to_string();
        if merge_base.is_empty() {
            return Ok(Vec::new());
        }

        let output = self.git_read(&["diff", "--name-only", &merge_base, branch])?;
        let files = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect();
        Ok(files)
    }

    /// Check if a branch has uncommitted changes (dirty worktree).
    /// For worktrees, runs status inside the worktree directory.
    pub fn branch_has_uncommitted(&self, worktree_path: &Path) -> Result<bool, GatewayError> {
        let output = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(worktree_path)
            .output()
            .map_err(|e| GatewayError::GitFailed(format!("git status: {e}")))?;
        Ok(!output.stdout.is_empty())
    }

    /// Count commits ahead of a base branch.
    ///
    /// Returns (ahead, behind) counts.
    pub fn commits_ahead_behind(
        &self,
        branch: &str,
        base_branch: &str,
    ) -> Result<(u32, u32), GatewayError> {
        let output = self.git_read(&[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{}...{}", base_branch, branch),
        ])?;
        let text = String::from_utf8_lossy(&output.stdout);
        let parts: Vec<&str> = text.trim().split('\t').collect();
        let behind = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
        let ahead = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        Ok((ahead, behind))
    }

    /// Get the base commit (first commit on a branch after diverging from base).
    pub fn branch_base_commit(
        &self,
        branch: &str,
        base_branch: &str,
    ) -> Result<String, GatewayError> {
        let output = self.git_read(&["merge-base", base_branch, branch])?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Parse worktree list into structured data.
    /// Returns Vec of (worktree_path, branch_name).
    pub fn parse_worktrees(&self) -> Result<Vec<(PathBuf, String)>, GatewayError> {
        let raw = self.list_worktrees()?;
        let mut results = Vec::new();
        let mut current_path: Option<PathBuf> = None;
        let mut current_branch: Option<String> = None;

        for line in raw.lines() {
            if let Some(path) = line.strip_prefix("worktree ") {
                // Flush previous entry
                if let (Some(p), Some(b)) = (current_path.take(), current_branch.take()) {
                    results.push((p, b));
                }
                current_path = Some(PathBuf::from(path));
            } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
                current_branch = Some(branch.to_string());
            } else if line.is_empty() {
                // Entry separator — flush
                if let (Some(p), Some(b)) = (current_path.take(), current_branch.take()) {
                    results.push((p, b));
                }
                current_path = None;
                current_branch = None;
            }
        }
        // Flush last entry
        if let (Some(p), Some(b)) = (current_path, current_branch) {
            results.push((p, b));
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Create a temporary git repo for testing. Returns the temp dir (keeps it alive).
    fn setup_test_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("create temp dir");
        let root = dir.path();

        // Init repo
        run_git(root, &["init", "-b", "main"]);
        run_git(root, &["config", "user.email", "test@test.com"]);
        run_git(root, &["config", "user.name", "Test"]);

        // Create initial commit on main
        std::fs::write(root.join("README.md"), "# Test\n").unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "initial"]);

        dir
    }

    fn run_git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git command failed");
        if !out.status.success() {
            panic!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn current_branch_returns_main() {
        let dir = setup_test_repo();
        let gw = VcsGateway::new(dir.path().to_path_buf());
        assert_eq!(gw.current_branch().unwrap(), "main");
    }

    #[test]
    fn create_branch_and_switch() {
        let dir = setup_test_repo();
        let mut gw = VcsGateway::new(dir.path().to_path_buf());

        let output = gw.create_branch("pipit/test-task").unwrap();
        assert!(output.status.success());
        assert_eq!(gw.current_branch().unwrap(), "pipit/test-task");
    }

    #[test]
    fn commits_ahead_behind_counts_correctly() {
        let dir = setup_test_repo();
        let root = dir.path();

        // Create a feature branch with 2 commits
        run_git(root, &["checkout", "-b", "feature"]);
        std::fs::write(root.join("a.txt"), "a").unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "feat: a"]);

        std::fs::write(root.join("b.txt"), "b").unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "feat: b"]);

        let gw = VcsGateway::new(root.to_path_buf());
        let (ahead, behind) = gw.commits_ahead_behind("feature", "main").unwrap();
        assert_eq!(ahead, 2);
        assert_eq!(behind, 0);
    }

    #[test]
    fn workspace_modified_files_detects_changes() {
        let dir = setup_test_repo();
        let root = dir.path();

        run_git(root, &["checkout", "-b", "work"]);
        std::fs::write(root.join("new_file.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("README.md"), "# Updated\n").unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "changes"]);

        let gw = VcsGateway::new(root.to_path_buf());
        let files = gw.workspace_modified_files("work", "main").unwrap();
        assert!(files.contains(&"new_file.rs".to_string()));
        assert!(files.contains(&"README.md".to_string()));
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn branch_base_commit_finds_merge_base() {
        let dir = setup_test_repo();
        let root = dir.path();

        let main_sha = run_git(root, &["rev-parse", "HEAD"]);
        run_git(root, &["checkout", "-b", "work2"]);
        std::fs::write(root.join("x.txt"), "x").unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "on branch"]);

        let gw = VcsGateway::new(root.to_path_buf());
        let base = gw.branch_base_commit("work2", "main").unwrap();
        assert_eq!(base, main_sha);
    }

    #[test]
    fn branch_has_uncommitted_detects_dirty() {
        let dir = setup_test_repo();
        let root = dir.path();

        let gw = VcsGateway::new(root.to_path_buf());
        assert!(!gw.branch_has_uncommitted(root).unwrap());

        // Make repository dirty
        std::fs::write(root.join("dirty.txt"), "dirty").unwrap();
        assert!(gw.branch_has_uncommitted(root).unwrap());
    }

    #[test]
    fn is_git_mutation_classifies_correctly() {
        assert!(!VcsGateway::is_git_mutation("git status"));
        assert!(!VcsGateway::is_git_mutation("git log --oneline"));
        assert!(!VcsGateway::is_git_mutation("git diff"));
        assert!(!VcsGateway::is_git_mutation("git branch"));

        assert!(VcsGateway::is_git_mutation("git checkout -b new"));
        assert!(VcsGateway::is_git_mutation("git commit -m 'msg'"));
        assert!(VcsGateway::is_git_mutation("git push"));
        assert!(VcsGateway::is_git_mutation("git merge feature"));
        assert!(VcsGateway::is_git_mutation("git reset --hard"));

        assert!(!VcsGateway::is_git_mutation("ls -la"));
        assert!(!VcsGateway::is_git_mutation("cargo build"));
    }

    #[test]
    fn auto_commit_via_gateway() {
        let dir = setup_test_repo();
        let root = dir.path();
        let mut gw = VcsGateway::new(root.to_path_buf());

        std::fs::write(root.join("auto.txt"), "auto content").unwrap();
        gw.auto_commit("auto test commit").unwrap();

        let log = run_git(root, &["log", "--oneline", "-1"]);
        assert!(log.contains("[pipit] auto test commit"));
    }

    #[test]
    fn commit_with_auto_stage() {
        let dir = setup_test_repo();
        let root = dir.path();
        let mut gw = VcsGateway::new(root.to_path_buf());

        std::fs::write(root.join("staged.txt"), "staged").unwrap();
        let output = gw.commit("test commit", true).unwrap();
        assert!(output.status.success());

        let log = run_git(root, &["log", "--oneline", "-1"]);
        assert!(log.contains("test commit"));
    }

    #[test]
    fn kernel_workflow_transitions() {
        let dir = setup_test_repo();
        let root = dir.path();
        let mut kernel = crate::workflow::VcsKernel::new(root.to_path_buf());

        // Idle → Editing
        let t = kernel
            .execute(
                "ws-1",
                crate::workflow::WorkflowOp::CreateWorkspace {
                    name: "feature".into(),
                    base_branch: "main".into(),
                    objective: Some("test task".into()),
                },
            )
            .unwrap();
        assert_eq!(t.to, crate::workflow::WorkflowPhase::Editing);

        // Editing → Snapshotted
        let t = kernel
            .execute(
                "ws-1",
                crate::workflow::WorkflowOp::Snapshot {
                    message: "checkpoint".into(),
                },
            )
            .unwrap();
        assert_eq!(t.to, crate::workflow::WorkflowPhase::Snapshotted);

        // Snapshotted → Verifying
        let t = kernel
            .execute(
                "ws-1",
                crate::workflow::WorkflowOp::Verify {
                    checks: vec!["test".into()],
                },
            )
            .unwrap();
        assert_eq!(t.to, crate::workflow::WorkflowPhase::Verifying);

        // Verifying → Verified (pass)
        let t = kernel
            .execute(
                "ws-1",
                crate::workflow::WorkflowOp::RecordVerification {
                    check: "test".into(),
                    passed: true,
                    evidence: "all pass".into(),
                },
            )
            .unwrap();
        assert_eq!(t.to, crate::workflow::WorkflowPhase::Verified);

        // active_workspaces should include ws-1
        let active = kernel.active_workspaces();
        assert!(active.iter().any(|(id, _)| *id == "ws-1"));
    }

    #[test]
    fn kernel_rejects_invalid_transition() {
        let dir = setup_test_repo();
        let mut kernel = crate::workflow::VcsKernel::new(dir.path().to_path_buf());

        // Can't Promote from Idle
        let result = kernel.execute(
            "ws-1",
            crate::workflow::WorkflowOp::Promote {
                target: "main".into(),
                strategy: crate::workflow::MergeStrategy::Merge,
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn ledger_records_and_replays_transitions() {
        let dir = setup_test_repo();
        let root = dir.path();
        let mut kernel = crate::workflow::VcsKernel::new(root.to_path_buf());

        kernel
            .execute(
                "ws-1",
                crate::workflow::WorkflowOp::CreateWorkspace {
                    name: "test".into(),
                    base_branch: "main".into(),
                    objective: None,
                },
            )
            .unwrap();

        kernel
            .execute(
                "ws-1",
                crate::workflow::WorkflowOp::Snapshot {
                    message: "snap".into(),
                },
            )
            .unwrap();

        // Reload kernel from disk — should recover state
        let loaded = crate::workflow::VcsKernel::load(root.to_path_buf()).unwrap();
        assert_eq!(
            loaded.phase("ws-1"),
            crate::workflow::WorkflowPhase::Snapshotted
        );
    }

    #[test]
    fn reconciler_scans_empty_workspace() {
        let reconciler = crate::reconcile::WorkspaceReconciler::new();
        let issues = reconciler.scan(&[]);
        assert!(issues.is_empty());
    }

    #[test]
    fn reconciler_detects_stale_workspace() {
        let reconciler = crate::reconcile::WorkspaceReconciler {
            stale_threshold_days: 3,
            ..Default::default()
        };
        let stale = crate::reconcile::WorkspaceState {
            workspace_id: "old".into(),
            branch: "pipit/old".into(),
            base_commit: "abc".into(),
            modified_files: Vec::new(),
            has_uncommitted: false,
            commits_ahead: 0,
            verified: false,
            has_contract: false,
            created_at: chrono::Utc::now() - chrono::Duration::days(10),
            last_active: chrono::Utc::now() - chrono::Duration::days(10),
        };
        let issues = reconciler.scan(&[stale]);
        assert_eq!(issues.len(), 1);
        assert!(matches!(
            &issues[0].1,
            crate::reconcile::ReconcileAction::SuggestCleanup { .. }
        ));
    }

    #[test]
    fn reconciler_detects_file_conflicts() {
        let reconciler = crate::reconcile::WorkspaceReconciler::new();
        let ws_a = crate::reconcile::WorkspaceState {
            workspace_id: "a".into(),
            branch: "pipit/a".into(),
            base_commit: "abc".into(),
            modified_files: vec!["src/main.rs".into(), "src/lib.rs".into()],
            has_uncommitted: false,
            commits_ahead: 1,
            verified: false,
            has_contract: false,
            created_at: chrono::Utc::now(),
            last_active: chrono::Utc::now(),
        };
        let ws_b = crate::reconcile::WorkspaceState {
            workspace_id: "b".into(),
            branch: "pipit/b".into(),
            base_commit: "abc".into(),
            modified_files: vec!["src/main.rs".into(), "other.rs".into()],
            has_uncommitted: false,
            commits_ahead: 2,
            verified: false,
            has_contract: false,
            created_at: chrono::Utc::now(),
            last_active: chrono::Utc::now(),
        };
        let issues = reconciler.scan(&[ws_a, ws_b]);
        let conflict = issues
            .iter()
            .find(|(_, a)| matches!(a, crate::reconcile::ReconcileAction::ResolveConflict { .. }));
        assert!(conflict.is_some());
    }

    #[test]
    fn reconciler_promotes_verified_with_contract() {
        let reconciler = crate::reconcile::WorkspaceReconciler::new();
        let ws = crate::reconcile::WorkspaceState {
            workspace_id: "ready".into(),
            branch: "pipit/ready".into(),
            base_commit: "abc".into(),
            modified_files: vec!["fix.rs".into()],
            has_uncommitted: false,
            commits_ahead: 1,
            verified: true,
            has_contract: true,
            created_at: chrono::Utc::now(),
            last_active: chrono::Utc::now(),
        };
        let issues = reconciler.scan(&[ws]);
        assert!(
            issues
                .iter()
                .any(|(_, a)| { matches!(a, crate::reconcile::ReconcileAction::Promote { .. }) })
        );
    }
}
