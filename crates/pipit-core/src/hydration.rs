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
    StageStarted { stage: HydrationStage },
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
    events.push(HydrationEvent::StageStarted {
        stage: HydrationStage::LedgerReplay,
    });
    let stage_start = std::time::Instant::now();

    let (event_count, messages) = kernel.resume()?;

    events.push(HydrationEvent::StageCompleted {
        stage: HydrationStage::LedgerReplay,
        duration_ms: stage_start.elapsed().as_millis() as u64,
    });

    // ── Stage 2: Context Restore ──
    events.push(HydrationEvent::StageStarted {
        stage: HydrationStage::ContextRestore,
    });
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
    events.push(HydrationEvent::StageStarted {
        stage: HydrationStage::WorktreeRestore,
    });
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
    events.push(HydrationEvent::StageStarted {
        stage: HydrationStage::PermissionRestore,
    });
    let stage_start = std::time::Instant::now();
    // Permissions are rebuilt from ledger replay (ToolApproved/ToolDenied events
    // are already reduced into SessionState by the kernel). No separate action needed.
    events.push(HydrationEvent::StageCompleted {
        stage: HydrationStage::PermissionRestore,
        duration_ms: stage_start.elapsed().as_millis() as u64,
    });

    // ── Stage 5: UI Reattach (no-op here — handled by caller) ──
    events.push(HydrationEvent::StageStarted {
        stage: HydrationStage::UiReattach,
    });
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

// ═══════════════════════════════════════════════════════════════
//  DEPENDENCY DAG (Task 3: DAG-aligned execution & hydration)
// ═══════════════════════════════════════════════════════════════
//
// Both live execution and hydration traverse the same dependency DAG
// in dependency order (topological sort). The DAG is:
//
//   Ledger ← Context ← Worktree ← Permissions ← UI
//
// Live execution order (forward path):
//   Input → Ledger Intent → Context Freeze → Permission Gate → Tool/Model → UI Publish
//
// Hydration order (restore path):
//   Ledger Replay → Context Restore → Worktree Restore → Permission Restore → UI Reattach
//
// Both are the SAME topological ordering of the same DAG.
// The invariant: resume(execute(S)) == S modulo in-flight transient state.

/// Subsystem node in the dependency DAG.
/// Execution and hydration both traverse in dependency order (not reverse).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Subsystem {
    /// Event-sourced ledger — foundation of all state.
    Ledger = 0,
    /// Context manager — depends on ledger for message replay.
    Context = 1,
    /// Worktree/filesystem — depends on context for cwd information.
    Worktree = 2,
    /// Permission state — derived from ledger replay.
    Permissions = 3,
    /// UI/bridge — depends on everything else.
    Ui = 4,
}

/// The dependency edges in the subsystem DAG.
/// Each pair (A, B) means B depends on A.
pub const DEPENDENCY_EDGES: &[(Subsystem, Subsystem)] = &[
    (Subsystem::Ledger, Subsystem::Context),
    (Subsystem::Ledger, Subsystem::Permissions),
    (Subsystem::Context, Subsystem::Worktree),
    (Subsystem::Worktree, Subsystem::Ui),
    (Subsystem::Permissions, Subsystem::Ui),
];

/// Topological order of subsystems (same for execution and hydration).
pub const SUBSYSTEM_ORDER: &[Subsystem] = &[
    Subsystem::Ledger,
    Subsystem::Context,
    Subsystem::Worktree,
    Subsystem::Permissions,
    Subsystem::Ui,
];

/// Verify that a given execution order respects the dependency DAG.
/// Returns Ok(()) if valid, Err with the violating pair if not.
pub fn verify_order(order: &[Subsystem]) -> Result<(), (Subsystem, Subsystem)> {
    for &(dep, dependant) in DEPENDENCY_EDGES {
        let dep_pos = order.iter().position(|s| *s == dep);
        let dependant_pos = order.iter().position(|s| *s == dependant);
        if let (Some(d), Some(n)) = (dep_pos, dependant_pos) {
            if d >= n {
                return Err((dep, dependant));
            }
        }
    }
    Ok(())
}

/// Maps a HydrationStage to its corresponding Subsystem.
/// This ensures hydration and execution share the same DAG.
pub fn stage_to_subsystem(stage: HydrationStage) -> Option<Subsystem> {
    match stage {
        HydrationStage::LedgerReplay => Some(Subsystem::Ledger),
        HydrationStage::ContextRestore => Some(Subsystem::Context),
        HydrationStage::WorktreeRestore => Some(Subsystem::Worktree),
        HydrationStage::PermissionRestore => Some(Subsystem::Permissions),
        HydrationStage::UiReattach => Some(Subsystem::Ui),
        HydrationStage::Complete => None,
    }
}

// ═══════════════════════════════════════════════════════════════
//  MANDATORY PERSISTENCE BOUNDARIES
// ═══════════════════════════════════════════════════════════════

/// The mandatory persistence boundaries per turn.
/// These are the causal cut points that MUST be journaled.
/// Everything else is opportunistic.
///
/// Persistence cost: O(b) per turn where b = |MANDATORY_BOUNDARIES|.
/// Recovery completeness: any recovered state is a prefix-consistent
/// image of the committed event stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MandatoryBoundary {
    /// User message must be persisted before model step.
    UserAccepted,
    /// Response start must be persisted before streaming.
    ResponseBegin,
    /// Tool proposal must be persisted before permission check.
    ToolProposed,
    /// Permission decision must be persisted before execution.
    PermissionResolved,
    /// Tool outcome must be persisted after execution.
    ToolCompleted,
    /// Turn commit — makes turn externally visible.
    TurnCommitted,
}

/// All mandatory boundaries in execution order.
pub const MANDATORY_BOUNDARIES: &[MandatoryBoundary] = &[
    MandatoryBoundary::UserAccepted,
    MandatoryBoundary::ResponseBegin,
    MandatoryBoundary::ToolProposed,
    MandatoryBoundary::PermissionResolved,
    MandatoryBoundary::ToolCompleted,
    MandatoryBoundary::TurnCommitted,
];

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

    #[test]
    fn test_subsystem_order_respects_dag() {
        assert!(verify_order(SUBSYSTEM_ORDER).is_ok());
    }

    #[test]
    fn test_invalid_order_detected() {
        let bad_order = [
            Subsystem::Context, // before Ledger — violation!
            Subsystem::Ledger,
            Subsystem::Worktree,
            Subsystem::Permissions,
            Subsystem::Ui,
        ];
        assert!(verify_order(&bad_order).is_err());
    }

    #[test]
    fn test_hydration_stages_map_to_subsystems() {
        // Verify that hydration stage order matches subsystem DAG order
        let stages = [
            HydrationStage::LedgerReplay,
            HydrationStage::ContextRestore,
            HydrationStage::WorktreeRestore,
            HydrationStage::PermissionRestore,
            HydrationStage::UiReattach,
        ];
        let subsystems: Vec<Subsystem> = stages
            .iter()
            .filter_map(|s| stage_to_subsystem(*s))
            .collect();
        assert!(verify_order(&subsystems).is_ok());
    }
}
