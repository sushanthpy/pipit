//! Deterministic Resume & Hydration Protocol
//!
//! Defines a strict restoration order for session resume that respects
//! subsystem dependencies. Without a deterministic hydrate order, resumes
//! are vulnerable to Heisenbugs where logically valid state still produces
//! inconsistent behavior because subsystems are reattached in the wrong
//! sequence.
//!
//! Dependency order: Ledger → Context → Worktree → Permissions → UI
//! Restoration cost: O(V + E) over the dependency DAG (constant-sized in practice).

use crate::session_kernel::{SessionKernel, SessionKernelConfig, SessionKernelError};
use pipit_context::budget::ContextManager;
use pipit_provider::Message;
use std::path::{Path, PathBuf};

/// Stages of the hydration protocol (executed in dependency order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HydrationStage {
    /// Stage 1: Load snapshot + replay ledger tail.
    LedgerReplay,
    /// Stage 2: Restore context messages from WAL.
    ContextRestore,
    /// Stage 3: Restore worktree/session working directory.
    WorktreeRestore,
    /// Stage 4: Restore pending approval/permission state.
    PermissionRestore,
    /// Stage 5: Reattach streaming UI / bridge.
    UiReattach,
    /// Hydration complete.
    Complete,
}

/// Event emitted during hydration for observability.
#[derive(Debug, Clone)]
pub enum HydrationEvent {
    /// A hydration stage has started.
    StageStarted {
        stage: HydrationStage,
    },
    /// A hydration stage completed successfully.
    StageCompleted {
        stage: HydrationStage,
        duration_ms: u64,
    },
    /// A hydration stage failed (non-fatal — continue with defaults).
    StageWarning {
        stage: HydrationStage,
        message: String,
    },
    /// Hydration complete.
    Complete {
        events_replayed: usize,
        messages_restored: usize,
        total_duration_ms: u64,
    },
}

/// Result of a successful hydration.
#[derive(Debug)]
pub struct HydrationResult {
    /// Number of ledger events replayed.
    pub events_replayed: usize,
    /// Messages restored into context.
    pub messages_restored: Vec<Message>,
    /// Whether a worktree was restored.
    pub worktree_restored: bool,
    /// Working directory after restoration.
    pub restored_cwd: Option<PathBuf>,
    /// Hydration events for telemetry.
    pub events: Vec<HydrationEvent>,
}

/// Hydrate a session from persisted state with strict dependency ordering.
///
/// The hydration protocol executes stages in topological order:
/// ```text
/// Ledger → Context → Worktree → Permissions → UI
/// ```
/// Each stage depends only on prior stages. If a stage fails, subsequent
/// stages use safe defaults rather than stale state.
pub fn hydrate_session(
    kernel: &mut SessionKernel,
    context: &mut ContextManager,
    session_dir: &Path,
) -> Result<HydrationResult, SessionKernelError> {
    let start = std::time::Instant::now();
    let mut events = Vec::new();
    let mut worktree_restored = false;
    let mut restored_cwd = None;

    // ── Stage 1: Ledger Replay ──
    events.push(HydrationEvent::StageStarted { stage: HydrationStage::LedgerReplay });
    let stage_start = std::time::Instant::now();

    let (event_count, messages) = kernel.resume()?;

    events.push(HydrationEvent::StageCompleted {
        stage: HydrationStage::LedgerReplay,
        duration_ms: stage_start.elapsed().as_millis() as u64,
    });

    // ── Stage 2: Context Restore ──
    events.push(HydrationEvent::StageStarted { stage: HydrationStage::ContextRestore });
    let stage_start = std::time::Instant::now();

    for msg in &messages {
        context.push_message(msg.clone());
    }
    let msg_count = messages.len();

    events.push(HydrationEvent::StageCompleted {
        stage: HydrationStage::ContextRestore,
        duration_ms: stage_start.elapsed().as_millis() as u64,
    });

    // ── Stage 3: Worktree Restore ──
    events.push(HydrationEvent::StageStarted { stage: HydrationStage::WorktreeRestore });
    let stage_start = std::time::Instant::now();

    // Check if a worktree session directory exists with cwd state
    let cwd_file = session_dir.join("cwd");
    if cwd_file.exists() {
        match std::fs::read_to_string(&cwd_file) {
            Ok(cwd_str) => {
                let cwd = PathBuf::from(cwd_str.trim());
                if cwd.exists() {
                    restored_cwd = Some(cwd);
                    worktree_restored = true;
                }
            }
            Err(e) => {
                events.push(HydrationEvent::StageWarning {
                    stage: HydrationStage::WorktreeRestore,
                    message: format!("Failed to read cwd: {}", e),
                });
            }
        }
    }

    events.push(HydrationEvent::StageCompleted {
        stage: HydrationStage::WorktreeRestore,
        duration_ms: stage_start.elapsed().as_millis() as u64,
    });

    // ── Stage 4: Permission Restore ──
    events.push(HydrationEvent::StageStarted { stage: HydrationStage::PermissionRestore });
    let stage_start = std::time::Instant::now();
    // Permissions are rebuilt from ledger replay (ToolApproved/ToolDenied events
    // are already reduced into SessionState by the kernel). No separate action needed.
    events.push(HydrationEvent::StageCompleted {
        stage: HydrationStage::PermissionRestore,
        duration_ms: stage_start.elapsed().as_millis() as u64,
    });

    // ── Stage 5: UI Reattach (no-op here — handled by caller) ──
    events.push(HydrationEvent::StageStarted { stage: HydrationStage::UiReattach });
    events.push(HydrationEvent::StageCompleted {
        stage: HydrationStage::UiReattach,
        duration_ms: 0,
    });

    // ── Complete ──
    events.push(HydrationEvent::Complete {
        events_replayed: event_count,
        messages_restored: msg_count,
        total_duration_ms: start.elapsed().as_millis() as u64,
    });

    Ok(HydrationResult {
        events_replayed: event_count,
        messages_restored: messages,
        worktree_restored,
        restored_cwd,
        events,
    })
}

/// Persist the current working directory for future restoration.
pub fn persist_cwd(session_dir: &Path, cwd: &Path) -> Result<(), std::io::Error> {
    let cwd_file = session_dir.join("cwd");
    std::fs::write(&cwd_file, cwd.to_string_lossy().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hydration_stages_order() {
        // Verify the dependency ordering is correct
        let stages = [
            HydrationStage::LedgerReplay,
            HydrationStage::ContextRestore,
            HydrationStage::WorktreeRestore,
            HydrationStage::PermissionRestore,
            HydrationStage::UiReattach,
            HydrationStage::Complete,
        ];

        // Each stage must come after its dependency
        for (i, stage) in stages.iter().enumerate() {
            assert_eq!(*stage as usize, i);
        }
    }
}
