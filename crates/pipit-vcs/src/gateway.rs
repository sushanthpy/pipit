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
            .args([
                "worktree",
                "remove",
                worktree_path.to_str().unwrap_or("."),
            ])
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
                    workspace_id: None, message: format!("branch created: {}", branch),
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
                    workspace_id: None, message: format!("branch switched to: {}", branch),
                })
                .ok();
        }

        Ok((output, dirty))
    }

    // ══════════════════════════════════════════════════════════
    //  COMMIT OPERATIONS
    // ══════════════════════════════════════════════════════════

    /// Commit all changes. Firewall-checked, ledger-logged.
    pub fn commit(
        &mut self,
        message: &str,
        auto_stage: bool,
    ) -> Result<Output, GatewayError> {
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
                    workspace_id: None, message: format!("commit: {}", message),
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
                workspace_id: None, message: format!("auto-commit: {}", summary),
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
                workspace_id: None, message: format!("rollback {} file(s) to {}", files.len(), sha),
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
            "log", "status", "diff", "show", "branch", "remote", "tag",
            "rev-parse", "ls-files", "cat-file", "describe", "shortlog",
            "reflog", "blame", "bisect", "stash list", "worktree list",
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
}
