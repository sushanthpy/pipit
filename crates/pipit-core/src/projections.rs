//! # Projection Library (Architecture Task 3)
//!
//! Pure projection layer that reads from session state, ledger, and
//! workspace stores to emit narrowly scoped, testable state views.
//!
//! Each projection is a pure function over read-only context:
//!   Projection(SessionState, LedgerEntries, WorkspaceState) → ProjectionPacket
//!
//! Projections can be consumed by:
//! - LLM prompts (system prompt injection)
//! - UI surfaces (status bars, context tabs)
//! - Logs and telemetry
//! - Replay tests (regression validation)
//!
//! Cost: O(R + E + W) for initial computation; can be cached with
//! change-indexed invalidation once profiling justifies it.

use crate::ledger::{LedgerEvent, SessionEvent, SessionState};
use serde::{Deserialize, Serialize};

// ═══════════════════════════════════════════════════════════════
//  PROJECTION PACKETS — typed state views
// ═══════════════════════════════════════════════════════════════

/// The user's current objective and progress toward it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrentObjective {
    /// The task as stated by the user.
    pub task: Option<String>,
    /// Current strategy (if planning is active).
    pub strategy: Option<String>,
    /// Number of plan pivots so far.
    pub plan_pivots: u32,
    /// Current turn number.
    pub turn: u32,
    /// Whether the session has ended.
    pub ended: bool,
}

/// Summary of the active workspace state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveWorkspace {
    /// Session identifier.
    pub session_id: Option<String>,
    /// Model in use.
    pub model: Option<String>,
    /// Provider in use.
    pub provider: Option<String>,
    /// Files modified during this session.
    pub modified_files: Vec<String>,
    /// Total tokens consumed.
    pub total_tokens: u64,
    /// Total cost.
    pub total_cost: f64,
    /// Context compressions performed.
    pub compressions: u32,
}

/// Obligations that must be fulfilled before promotion or completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingObligations {
    /// Active subagents that haven't completed.
    pub active_subagents: Vec<String>,
    /// Verification steps not yet performed.
    pub pending_verifications: Vec<String>,
    /// Whether a plan review is pending.
    pub plan_review_pending: bool,
}

/// Recent evidence from tool calls and verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentEvidence {
    /// Tool calls completed in this session.
    pub tool_calls_completed: u32,
    /// Tool calls denied.
    pub tool_calls_denied: u32,
    /// Tool results with mutations.
    pub mutating_tool_calls: u32,
    /// Completed subagent IDs.
    pub completed_subagents: Vec<String>,
    /// Last verification verdict (if any).
    pub last_verification: Option<VerificationSummary>,
}

/// Summary of a verification run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationSummary {
    pub verdict: String,
    pub confidence: f32,
    pub findings_count: usize,
}

/// Cost and resource summary for budget tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceSummary {
    pub total_tokens: u64,
    pub total_cost: f64,
    pub tokens_freed_by_compression: u64,
    pub turns_completed: u32,
    pub user_messages: u32,
    pub assistant_messages: u32,
}

// ═══════════════════════════════════════════════════════════════
//  PROJECTION FUNCTIONS — pure, testable, composable
// ═══════════════════════════════════════════════════════════════

/// Project the current objective from session state.
pub fn project_objective(state: &SessionState) -> CurrentObjective {
    CurrentObjective {
        task: None, // Extracted from first user message if available
        strategy: state.current_strategy.clone(),
        plan_pivots: state.plan_pivots,
        turn: state.current_turn,
        ended: state.ended,
    }
}

/// Project the current objective with task extracted from events.
pub fn project_objective_with_events(
    state: &SessionState,
    events: &[LedgerEvent],
) -> CurrentObjective {
    let task = events.iter().find_map(|e| match &e.payload {
        SessionEvent::UserMessageAccepted { content } => Some(content.clone()),
        _ => None,
    });

    CurrentObjective {
        task,
        strategy: state.current_strategy.clone(),
        plan_pivots: state.plan_pivots,
        turn: state.current_turn,
        ended: state.ended,
    }
}

/// Project the active workspace summary.
pub fn project_workspace(state: &SessionState) -> ActiveWorkspace {
    ActiveWorkspace {
        session_id: state.session_id.clone(),
        model: state.model.clone(),
        provider: state.provider.clone(),
        modified_files: state.modified_files.clone(),
        total_tokens: state.total_tokens,
        total_cost: state.total_cost,
        compressions: state.compressions,
    }
}

/// Project pending obligations.
pub fn project_obligations(state: &SessionState) -> PendingObligations {
    PendingObligations {
        active_subagents: state.active_subagents.clone(),
        pending_verifications: Vec::new(), // Would require cross-store query
        plan_review_pending: state.current_strategy.is_some() && state.current_turn == 0,
    }
}

/// Project recent evidence from events.
pub fn project_evidence(state: &SessionState, events: &[LedgerEvent]) -> RecentEvidence {
    // Count mutating tool calls from events
    let mutating_tool_calls = events
        .iter()
        .filter(|e| {
            matches!(
                &e.payload,
                SessionEvent::ToolCompleted { mutated: true, .. }
            )
        })
        .count() as u32;

    // Find last verification verdict
    let last_verification = events.iter().rev().find_map(|e| match &e.payload {
        SessionEvent::VerificationVerdict {
            verdict,
            confidence,
            findings_count,
        } => Some(VerificationSummary {
            verdict: verdict.clone(),
            confidence: *confidence,
            findings_count: *findings_count,
        }),
        _ => None,
    });

    RecentEvidence {
        tool_calls_completed: state.tool_calls_completed,
        tool_calls_denied: state.tool_calls_denied,
        mutating_tool_calls,
        completed_subagents: state.completed_subagents.clone(),
        last_verification,
    }
}

/// Project resource summary for budget tracking.
pub fn project_resources(state: &SessionState) -> ResourceSummary {
    ResourceSummary {
        total_tokens: state.total_tokens,
        total_cost: state.total_cost,
        tokens_freed_by_compression: state.tokens_freed_by_compression,
        turns_completed: state.current_turn,
        user_messages: state.user_messages,
        assistant_messages: state.assistant_messages,
    }
}

/// Aggregate all projections into a single snapshot.
/// This is the primary interface for LLM prompt injection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectionSnapshot {
    pub objective: CurrentObjective,
    pub workspace: ActiveWorkspace,
    pub obligations: PendingObligations,
    pub evidence: RecentEvidence,
    pub resources: ResourceSummary,
}

/// Build a complete projection snapshot from state and events.
pub fn project_all(state: &SessionState, events: &[LedgerEvent]) -> ProjectionSnapshot {
    ProjectionSnapshot {
        objective: project_objective_with_events(state, events),
        workspace: project_workspace(state),
        obligations: project_obligations(state),
        evidence: project_evidence(state, events),
        resources: project_resources(state),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state() -> SessionState {
        SessionState {
            session_id: Some("test-session".into()),
            model: Some("gpt-4o".into()),
            provider: Some("openai".into()),
            current_turn: 3,
            user_messages: 3,
            assistant_messages: 3,
            tool_calls_completed: 5,
            tool_calls_denied: 0,
            total_tokens: 1500,
            total_cost: 0.05,
            current_strategy: Some("balanced".into()),
            plan_pivots: 0,
            modified_files: vec!["src/main.rs".into()],
            active_subagents: Vec::new(),
            completed_subagents: vec!["sub-1".into()],
            compressions: 1,
            tokens_freed_by_compression: 500,
            checkpoints: Vec::new(),
            last_seq: 20,
            ended: false,
        }
    }

    #[test]
    fn project_objective_extracts_strategy() {
        let state = make_state();
        let obj = project_objective(&state);
        assert_eq!(obj.strategy.as_deref(), Some("balanced"));
        assert_eq!(obj.turn, 3);
        assert!(!obj.ended);
    }

    #[test]
    fn project_workspace_captures_state() {
        let state = make_state();
        let ws = project_workspace(&state);
        assert_eq!(ws.modified_files, vec!["src/main.rs"]);
        assert_eq!(ws.total_tokens, 1500);
    }

    #[test]
    fn project_obligations_shows_pending() {
        let mut state = make_state();
        state.active_subagents = vec!["sub-2".into()];
        let obligations = project_obligations(&state);
        assert_eq!(obligations.active_subagents, vec!["sub-2"]);
    }

    #[test]
    fn project_resources_sums_correctly() {
        let state = make_state();
        let res = project_resources(&state);
        assert_eq!(res.total_tokens, 1500);
        assert_eq!(res.tokens_freed_by_compression, 500);
        assert_eq!(res.turns_completed, 3);
    }

    #[test]
    fn project_all_composes() {
        let state = make_state();
        let snap = project_all(&state, &[]);
        assert_eq!(snap.objective.turn, 3);
        assert_eq!(snap.workspace.total_tokens, 1500);
        assert_eq!(snap.resources.user_messages, 3);
    }
}
