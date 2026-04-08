//! Plan Mode & Worktree Tools — unified action-based tools.
//!
//! plan_mode: Enter/Exit via one tool with action parameter.
//! worktree: Create/Switch/Remove/List via one tool with action parameter.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::typed_tool::*;
use crate::{ToolContext, ToolError};

// ═══════════════════════════════════════════════════════════════
//  PLAN MODE TOOL
// ═══════════════════════════════════════════════════════════════

/// Plan mode action.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PlanModeInput {
    /// Enter plan mode — read-only, discuss before editing.
    Enter {
        /// Optional initial plan description.
        plan: Option<String>,
    },
    /// Exit plan mode — resume normal editing.
    Exit {
        /// Commit message summarizing the plan.
        summary: Option<String>,
    },
}

/// Unified plan mode tool — replaces Enter/ExitPlanMode.
pub struct PlanModeTool;

#[async_trait]
impl TypedTool for PlanModeTool {
    type Input = PlanModeInput;
    const NAME: &'static str = "plan_mode";
    const CAPABILITIES: CapabilitySet = CapabilitySet::SESSION_WRITE;
    const PURITY: Purity = Purity::Mutating;

    fn describe() -> ToolCard {
        ToolCard {
            name: "plan_mode".into(),
            summary: "Enter or exit plan mode for discussing approach before editing".into(),
            when_to_use: "Use plan mode when you want to discuss the approach with the user before making changes. In plan mode, you should only read files and discuss — no edits.".into(),
            examples: vec![
                ToolExample {
                    description: "Enter plan mode".into(),
                    input: serde_json::json!({
                        "action": "enter",
                        "plan": "Refactor auth module: 1) extract middleware, 2) add token validation"
                    }),
                },
                ToolExample {
                    description: "Exit plan mode".into(),
                    input: serde_json::json!({
                        "action": "exit",
                        "summary": "User approved the 3-step refactoring plan"
                    }),
                },
            ],
            tags: vec!["plan".into(), "mode".into(), "discussion".into(), "strategy".into()],
            purity: Purity::Mutating,
            capabilities: CapabilitySet::SESSION_WRITE.0,
        }
    }

    async fn execute(
        &self,
        input: PlanModeInput,
        _ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<TypedToolResult, ToolError> {
        match input {
            PlanModeInput::Enter { plan } => {
                let msg = if let Some(p) = plan {
                    format!(
                        "Entered plan mode.\n\nPlan:\n{}\n\nI will only read files and discuss approach — no edits until plan mode is exited.",
                        p
                    )
                } else {
                    "Entered plan mode. I will only read files and discuss approach — no edits until plan mode is exited.".into()
                };
                Ok(TypedToolResult::mutating(msg))
            }
            PlanModeInput::Exit { summary } => {
                let msg = if let Some(s) = summary {
                    format!(
                        "Exited plan mode. Summary: {}\n\nResuming normal editing mode.",
                        s
                    )
                } else {
                    "Exited plan mode. Resuming normal editing mode.".into()
                };
                Ok(TypedToolResult::mutating(msg))
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════
//  WORKTREE TOOL
// ═══════════════════════════════════════════════════════════════

/// Worktree action.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum WorktreeInput {
    /// Create a new git worktree for isolated changes.
    Create {
        /// Name for the worktree/branch.
        name: String,
        /// Base ref to branch from (default: HEAD).
        base_ref: Option<String>,
    },
    /// Switch to an existing worktree.
    Switch {
        /// Worktree name to switch to.
        name: String,
    },
    /// Remove a worktree.
    Remove {
        /// Worktree name to remove.
        name: String,
    },
    /// List all worktrees.
    List,
}

/// Unified worktree tool — replaces Enter/ExitWorktree.
pub struct WorktreeTool;

#[async_trait]
impl TypedTool for WorktreeTool {
    type Input = WorktreeInput;
    const NAME: &'static str = "worktree";
    const CAPABILITIES: CapabilitySet =
        CapabilitySet(CapabilitySet::FS_WRITE.0 | CapabilitySet::PROCESS_EXEC.0);
    const PURITY: Purity = Purity::Mutating;

    fn describe() -> ToolCard {
        ToolCard {
            name: "worktree".into(),
            summary: "Manage git worktrees for isolated parallel changes".into(),
            when_to_use: "When you need to work on changes in isolation without affecting the main working directory. Useful for experimental changes, parallel tasks, or subagent isolation.".into(),
            examples: vec![
                ToolExample {
                    description: "Create worktree for feature".into(),
                    input: serde_json::json!({
                        "action": "create",
                        "name": "feature-auth",
                        "base_ref": "main"
                    }),
                },
                ToolExample {
                    description: "List worktrees".into(),
                    input: serde_json::json!({"action": "list"}),
                },
            ],
            tags: vec!["git".into(), "worktree".into(), "branch".into(), "isolation".into()],
            purity: Purity::Mutating,
            capabilities: CapabilitySet::FS_WRITE.0 | CapabilitySet::PROCESS_EXEC.0,
        }
    }

    async fn execute(
        &self,
        input: WorktreeInput,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<TypedToolResult, ToolError> {
        let root = &ctx.project_root;

        match input {
            WorktreeInput::Create { name, base_ref } => {
                let base = base_ref.as_deref().unwrap_or("HEAD");
                let wt_path = root.join(format!("../.pipit-worktrees/{}", name));
                let output = tokio::process::Command::new("git")
                    .args(["worktree", "add", "-b", &name])
                    .arg(&wt_path)
                    .arg(base)
                    .current_dir(root)
                    .output()
                    .await
                    .map_err(|e| {
                        ToolError::ExecutionFailed(format!("git worktree add failed: {e}"))
                    })?;

                if output.status.success() {
                    Ok(TypedToolResult::mutating(format!(
                        "Created worktree '{}' at {} (based on {})",
                        name,
                        wt_path.display(),
                        base
                    )))
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    Err(ToolError::ExecutionFailed(format!(
                        "git worktree add failed: {stderr}"
                    )))
                }
            }

            WorktreeInput::Switch { name } => {
                // Verify the worktree exists in git's records, not just on filesystem
                let check = tokio::process::Command::new("git")
                    .args(["worktree", "list", "--porcelain"])
                    .current_dir(root)
                    .output()
                    .await
                    .map_err(|e| {
                        ToolError::ExecutionFailed(format!("git worktree list failed: {e}"))
                    })?;

                let list_output = String::from_utf8_lossy(&check.stdout);
                let wt_path = root.join(format!("../.pipit-worktrees/{}", name));
                let wt_path_str = wt_path.to_string_lossy();

                // Check both git's record and filesystem
                let in_git = list_output
                    .lines()
                    .any(|l| l.starts_with("worktree ") && l.contains(&*wt_path_str));
                let on_disk = wt_path.exists();

                if in_git || on_disk {
                    // Persist CWD change for session resume
                    let session_dir = root.join(".pipit").join("sessions").join("latest");
                    if session_dir.exists() {
                        let _ = std::fs::write(
                            session_dir.join("cwd"),
                            wt_path.to_string_lossy().as_bytes(),
                        );
                    }
                    ctx.set_cwd(wt_path.clone());
                    Ok(TypedToolResult::mutating(format!(
                        "Switched to worktree '{name}' at {}",
                        wt_path.display()
                    )))
                } else {
                    Err(ToolError::InvalidArgs(format!(
                        "Worktree '{}' not found (checked git records and {})",
                        name,
                        wt_path.display()
                    )))
                }
            }

            WorktreeInput::Remove { name } => {
                let output = tokio::process::Command::new("git")
                    .args(["worktree", "remove", &name])
                    .current_dir(root)
                    .output()
                    .await
                    .map_err(|e| {
                        ToolError::ExecutionFailed(format!("git worktree remove failed: {e}"))
                    })?;

                if output.status.success() {
                    Ok(TypedToolResult::mutating(format!(
                        "Removed worktree '{name}'"
                    )))
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    Err(ToolError::ExecutionFailed(format!("Failed: {stderr}")))
                }
            }

            WorktreeInput::List => {
                let output = tokio::process::Command::new("git")
                    .args(["worktree", "list", "--porcelain"])
                    .current_dir(root)
                    .output()
                    .await
                    .map_err(|e| {
                        ToolError::ExecutionFailed(format!("git worktree list failed: {e}"))
                    })?;

                let stdout = String::from_utf8_lossy(&output.stdout);
                Ok(TypedToolResult::text(if stdout.is_empty() {
                    "No worktrees found.".into()
                } else {
                    stdout.to_string()
                }))
            }
        }
    }
}
