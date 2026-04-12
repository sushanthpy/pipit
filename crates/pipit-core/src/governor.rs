use crate::proof::{ConfidenceReport, RollbackCheckpoint};
use crate::tool_semantics::{
    Purity, SemanticClass, ToolCategory, builtin_semantics, classify_semantically,
};
use pipit_provider::ToolCall;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

// --- Risk factor weights for tool-action classification ---

/// Blast radius for read-only operations (no side effects).
const BLAST_RADIUS_READ_ONLY: f32 = 0.1;
/// Blast radius for local file edits (scoped to one file).
const BLAST_RADIUS_LOCAL_EDIT: f32 = 0.4;
/// Blast radius for shell execution / high-risk actions (broad impact).
const BLAST_RADIUS_HIGH: f32 = 0.7;

/// Irreversibility: read-only actions are fully reversible.
const IRREVERSIBILITY_READ_ONLY: f32 = 0.0;
/// Irreversibility: local edits can be undone via git/backups.
const IRREVERSIBILITY_LOCAL_EDIT: f32 = 0.2;
/// Irreversibility: shell commands may be hard to undo.
const IRREVERSIBILITY_HIGH: f32 = 0.6;

/// Privilege level for shell execution (can affect the whole system).
const PRIVILEGE_SHELL: f32 = 0.8;
/// Privilege level for local file edits.
const PRIVILEGE_LOCAL_EDIT: f32 = 0.4;
/// Privilege level for read-only / low-risk operations.
const PRIVILEGE_LOW: f32 = 0.1;

/// Floor applied to each risk factor before multiplication so that a single
/// zero factor doesn't collapse the entire score.
const RISK_FACTOR_FLOOR: f32 = 0.1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActionClass {
    ReadOnly,
    LocalEdit,
    ShellExecution,
    ProjectConfigChange,
    HighRisk,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskReport {
    pub blast_radius: f32,
    pub irreversibility: f32,
    pub uncertainty: f32,
    pub privilege_level: f32,
    pub score: f32,
    pub action_class: ActionClass,
}

impl Default for RiskReport {
    fn default() -> Self {
        Self {
            blast_radius: 0.1,
            irreversibility: 0.1,
            uncertainty: 0.5,
            privilege_level: 0.1,
            score: 0.0005,
            action_class: ActionClass::ReadOnly,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Governor;

impl Governor {
    pub fn assess_tool_call(&self, call: &ToolCall, confidence: &ConfidenceReport) -> RiskReport {
        let action_class = classify_tool(call);
        let blast_radius: f32 = match action_class {
            ActionClass::ReadOnly => BLAST_RADIUS_READ_ONLY,
            ActionClass::LocalEdit => BLAST_RADIUS_LOCAL_EDIT,
            _ => BLAST_RADIUS_HIGH,
        };
        let irreversibility: f32 = match action_class {
            ActionClass::ReadOnly => IRREVERSIBILITY_READ_ONLY,
            ActionClass::LocalEdit => IRREVERSIBILITY_LOCAL_EDIT,
            _ => IRREVERSIBILITY_HIGH,
        };
        let privilege_level: f32 = match action_class {
            ActionClass::ShellExecution => PRIVILEGE_SHELL,
            ActionClass::LocalEdit => PRIVILEGE_LOCAL_EDIT,
            _ => PRIVILEGE_LOW,
        };
        let uncertainty = 1.0 - confidence.overall().clamp(0.0, 1.0);
        let score = blast_radius
            * irreversibility.max(RISK_FACTOR_FLOOR)
            * uncertainty.max(RISK_FACTOR_FLOOR)
            * privilege_level.max(RISK_FACTOR_FLOOR);

        RiskReport {
            blast_radius,
            irreversibility,
            uncertainty,
            privilege_level,
            score,
            action_class,
        }
    }

    pub fn create_rollback_checkpoint(
        &self,
        project_root: &Path,
        modified_files: &[String],
    ) -> RollbackCheckpoint {
        if modified_files.is_empty() {
            return RollbackCheckpoint {
                checkpoint_id: None,
                strategy: "No files were modified; rollback is a no-op".to_string(),
                reversible: true,
            };
        }

        if project_root.join(".git").exists() {
            let output = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(project_root)
                .output();

            if let Ok(output) = output {
                if output.status.success() {
                    let head = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    return RollbackCheckpoint {
                        checkpoint_id: Some(head.clone()),
                        strategy: format!(
                            "Reset modified files to git revision {} or checkout those paths from that revision.",
                            head
                        ),
                        reversible: true,
                    };
                }
            }
        }

        RollbackCheckpoint {
            checkpoint_id: None,
            strategy: format!(
                "Manually restore the modified files: {}",
                modified_files.join(", ")
            ),
            reversible: true,
        }
    }
}

/// Classify a tool call based on its canonical semantic descriptor.
/// ActionClass is derived from SemanticClass — the same type that evidence
/// and scheduling use — ensuring consistent classification across the system.
fn classify_tool(call: &ToolCall) -> ActionClass {
    let semantic_class = classify_semantically(&call.tool_name, &call.args);
    match semantic_class {
        SemanticClass::Read { .. } | SemanticClass::Search { .. } | SemanticClass::Pure => {
            ActionClass::ReadOnly
        }
        SemanticClass::Edit { .. } => ActionClass::LocalEdit,
        SemanticClass::Exec { .. } => ActionClass::ShellExecution,
        SemanticClass::Delegate { .. } | SemanticClass::External { .. } => ActionClass::HighRisk,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Task 6: Verification Nudge Predicate
// ═══════════════════════════════════════════════════════════════════════════

/// Determine whether a verification nudge should be fired at todo close-out.
///
/// Fires when:
///   - All todos are completed
///   - There are ≥3 items
///   - None of them contain "verif" or "test" (case-insensitive)
///   - The caller is the coordinator (not a subagent)
///
/// Complexity: O(n) in list size, runs once per TodoWrite call.
pub fn should_nudge_verification(
    todos: &[crate::ledger::TodoItem],
    is_coordinator: bool,
) -> bool {
    if !is_coordinator || todos.len() < 3 {
        return false;
    }

    let all_done = todos
        .iter()
        .all(|t| matches!(t.status, crate::ledger::TodoStatus::Completed));

    if !all_done {
        return false;
    }

    let has_verification = todos.iter().any(|t| {
        let lower = t.content.to_ascii_lowercase();
        lower.contains("verif") || lower.contains("test")
    });

    !has_verification
}
