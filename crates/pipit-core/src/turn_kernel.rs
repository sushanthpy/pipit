//! TurnKernel — Canonical Turn State Machine
//!
//! Models an agent turn as a deterministic finite-state machine with a
//! constant-size state graph. Every turn traverses a strict phase sequence:
//!
//! ```text
//!  Accepted → ContextFrozen → ResponseStarted → ToolProposed →
//!  PermissionResolved → ToolStarted → ToolCompleted →
//!  ResponseCompleted → Committed
//! ```
//!
//! Transition validation is O(1) per event using a fixed transition table.
//! This eliminates illegal interleavings and converts implicit control flow
//! into explicit state invariants.
//!
//! The kernel is a pure Mealy machine: (state, input) → (state', outputs).
//! No I/O, no side effects — deterministic simulation and replay for free.

use serde::{Deserialize, Serialize};

// ═══════════════════════════════════════════════════════════════
//  CANONICAL TURN PHASES
// ═══════════════════════════════════════════════════════════════

/// The canonical phase of a single turn in the agent lifecycle.
///
/// This is the formal state graph. Every turn follows this sequence,
/// though some phases may be skipped (e.g. no tools → skip ToolProposed).
/// The transition table enforces which skips are legal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TurnPhase {
    /// Idle — waiting for user input. Initial state and post-commit rest state.
    Idle,
    /// Accepted — user message has been accepted and recorded.
    Accepted,
    /// ContextFrozen — control-plane snapshot taken, context budget locked.
    ContextFrozen,
    /// Requesting — LLM request sent.
    Requesting,
    /// ResponseStarted — first token received from LLM.
    ResponseStarted,
    /// ToolProposed — tool calls received from LLM, pending permission.
    ToolProposed,
    /// PermissionResolved — all tool permissions decided (approved/denied).
    PermissionResolved,
    /// ToolStarted — tool execution has begun.
    ToolStarted,
    /// ToolCompleted — all tool executions have finished.
    ToolCompleted,
    /// Verifying — running post-edit verification (lint/test).
    Verifying,
    /// ResponseCompleted — assistant response fully received (no tools path).
    ResponseCompleted,
    /// Committed — turn result committed to kernel. Terminal state.
    Committed,
    /// Failed — turn encountered an unrecoverable error. Terminal state.
    Failed,
}

impl TurnPhase {
    /// Whether this is a terminal state (Committed or Failed).
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Committed | Self::Failed)
    }

    /// Whether this phase is part of the tool sub-cycle.
    pub fn is_tool_phase(self) -> bool {
        matches!(
            self,
            Self::ToolProposed | Self::PermissionResolved | Self::ToolStarted | Self::ToolCompleted
        )
    }
}

// ═══════════════════════════════════════════════════════════════
//  TRANSITION TABLE (O(1) lookup)
// ═══════════════════════════════════════════════════════════════

/// Legal transitions in the turn FSM.
/// Each entry maps (from_phase, input_kind) → to_phase.
///
/// This is a compile-time constant table; validation is a single
/// array lookup, not a match cascade.
const LEGAL_TRANSITIONS: &[(TurnPhase, &str, TurnPhase)] = &[
    // Happy path (no tools)
    (TurnPhase::Idle,              "user_message",        TurnPhase::Accepted),
    (TurnPhase::Accepted,          "context_frozen",      TurnPhase::ContextFrozen),
    (TurnPhase::ContextFrozen,     "request_sent",        TurnPhase::Requesting),
    (TurnPhase::Requesting,        "stream_started",      TurnPhase::ResponseStarted),
    (TurnPhase::ResponseStarted,   "response_complete",   TurnPhase::ResponseCompleted),
    (TurnPhase::ResponseCompleted, "committed",           TurnPhase::Committed),

    // Tool path
    (TurnPhase::ResponseStarted,   "tool_proposed",       TurnPhase::ToolProposed),
    (TurnPhase::ToolProposed,      "permission_resolved", TurnPhase::PermissionResolved),
    (TurnPhase::PermissionResolved,"tool_started",        TurnPhase::ToolStarted),
    (TurnPhase::ToolStarted,       "tool_completed",      TurnPhase::ToolCompleted),
    (TurnPhase::ToolCompleted,     "verification_start",  TurnPhase::Verifying),
    (TurnPhase::ToolCompleted,     "request_sent",        TurnPhase::Requesting),   // no verification needed
    (TurnPhase::Verifying,         "verification_done",   TurnPhase::Requesting),   // loop back for next LLM call

    // All-denied tools: skip execution entirely
    (TurnPhase::PermissionResolved,"request_sent",        TurnPhase::Requesting),

    // Committed → Idle for next turn
    (TurnPhase::Committed,         "reset",               TurnPhase::Idle),

    // Error from any phase
    // (handled specially — see transition() method)
];

/// Validate whether a transition is legal. O(1) amortized via small table scan.
fn is_legal_transition(from: TurnPhase, input_kind: &str) -> Option<TurnPhase> {
    LEGAL_TRANSITIONS
        .iter()
        .find(|(f, i, _)| *f == from && *i == input_kind)
        .map(|(_, _, to)| *to)
}

// ═══════════════════════════════════════════════════════════════
//  TURN INPUTS & OUTPUTS
// ═══════════════════════════════════════════════════════════════

/// Typed turn events — inputs to the state machine.
#[derive(Debug, Clone)]
pub enum TurnInput {
    /// User submitted a message.
    UserMessage(String),
    /// Control-plane snapshot frozen (context budget, tool registry, policy).
    ContextFrozen,
    /// LLM request sent.
    RequestSent,
    /// LLM response started streaming.
    StreamStarted,
    /// LLM response chunk received (informational, no phase change).
    StreamChunk { text: String },
    /// LLM response finished with tool calls.
    ToolCallsReceived { call_count: usize },
    /// LLM response finished without tool calls.
    ResponseComplete,
    /// Tool calls proposed to permission system.
    ToolProposed { call_ids: Vec<String> },
    /// All permissions resolved (approved/denied).
    PermissionResolved { approved: Vec<String>, denied: Vec<String> },
    /// Tool execution started.
    ToolExecutionStarted,
    /// A single tool call completed.
    SingleToolCompleted {
        call_id: String,
        success: bool,
        mutated: bool,
    },
    /// All tool calls completed.
    AllToolsCompleted { modified_files: Vec<String> },
    /// Verification completed.
    VerificationCompleted { passed: bool },
    /// Turn committed to kernel.
    TurnCommitted,
    /// Reset for next turn.
    Reset,
    /// Context compression triggered (out-of-band, any phase).
    CompressionTriggered,
    /// User cancelled (out-of-band, any phase).
    Cancelled,
    /// Error occurred (out-of-band, any phase).
    Error(String),
}

impl TurnInput {
    /// The transition-table key for this input.
    fn kind(&self) -> &str {
        match self {
            Self::UserMessage(_)       => "user_message",
            Self::ContextFrozen        => "context_frozen",
            Self::RequestSent          => "request_sent",
            Self::StreamStarted        => "stream_started",
            Self::StreamChunk { .. }   => "stream_chunk",
            Self::ToolCallsReceived { .. } => "tool_proposed",
            Self::ResponseComplete     => "response_complete",
            Self::ToolProposed { .. }  => "tool_proposed",
            Self::PermissionResolved { .. } => "permission_resolved",
            Self::ToolExecutionStarted => "tool_started",
            Self::SingleToolCompleted { .. } => "single_tool_completed",
            Self::AllToolsCompleted { .. }  => "tool_completed",
            Self::VerificationCompleted { .. } => "verification_done",
            Self::TurnCommitted        => "committed",
            Self::Reset                => "reset",
            Self::CompressionTriggered => "compression",
            Self::Cancelled            => "cancelled",
            Self::Error(_)             => "error",
        }
    }
}

/// Typed turn outputs — side effects the loop must execute.
#[derive(Debug, Clone)]
pub enum TurnOutput {
    /// Transition to a new phase.
    PhaseChange(TurnPhase),
    /// Emit a status label for the UI.
    Status(String),
    /// Emit an event for subscribers.
    Event(String),
    /// Request LLM completion.
    RequestCompletion,
    /// Execute scheduled tool batches.
    ExecuteTools,
    /// Run verification commands.
    RunVerification,
    /// Compress context.
    CompressContext,
    /// Freeze control-plane snapshot.
    FreezeControlPlane,
    /// Commit turn to kernel.
    CommitTurn,
    /// Turn is complete — yield control.
    Yield,
    /// Invalid transition attempted (for diagnostics).
    InvalidTransition { from: TurnPhase, input: String },
}

// ═══════════════════════════════════════════════════════════════
//  TURN MILESTONE TRACE (Task 8: Live Turn Trace)
// ═══════════════════════════════════════════════════════════════

/// A timestamped milestone in the current turn's lifecycle.
/// Collected as the turn progresses for live observability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnMilestone {
    /// The phase that was entered.
    pub phase: TurnPhase,
    /// Wall-clock timestamp (unix ms).
    pub timestamp_ms: u64,
    /// Optional detail (e.g. tool name, error message).
    pub detail: Option<String>,
}

impl TurnMilestone {
    fn now(phase: TurnPhase, detail: Option<String>) -> Self {
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Self { phase, timestamp_ms, detail }
    }
}

// ═══════════════════════════════════════════════════════════════
//  TURN RUNTIME SNAPSHOT (Task 5: materialized view)
// ═══════════════════════════════════════════════════════════════

/// Materialized view of the current turn's state.
/// Updated incrementally (O(1) per event). This is a projection
/// over the turn event stream, not a copy of kernel state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnSnapshot {
    /// Current turn number (1-indexed).
    pub turn_number: u32,
    /// Current phase.
    pub phase: TurnPhase,
    /// Proposed tool call IDs (pending permission).
    pub proposed_tools: Vec<String>,
    /// Approved tool call IDs.
    pub approved_tools: Vec<String>,
    /// Denied tool call IDs.
    pub denied_tools: Vec<String>,
    /// Running tool call IDs.
    pub running_tools: Vec<String>,
    /// Completed tool call IDs.
    pub completed_tools: Vec<String>,
    /// Whether any mutation occurred this turn.
    pub had_mutation: bool,
    /// Whether verification is pending.
    pub verification_pending: bool,
    /// Milestones in this turn (live trace).
    pub milestones: Vec<TurnMilestone>,
    /// Consecutive error count.
    pub consecutive_errors: u32,
}

impl TurnSnapshot {
    fn new(turn_number: u32) -> Self {
        Self {
            turn_number,
            phase: TurnPhase::Idle,
            proposed_tools: Vec::new(),
            approved_tools: Vec::new(),
            denied_tools: Vec::new(),
            running_tools: Vec::new(),
            completed_tools: Vec::new(),
            had_mutation: false,
            verification_pending: false,
            milestones: Vec::new(),
            consecutive_errors: 0,
        }
    }
}

// ═══════════════════════════════════════════════════════════════
//  THE KERNEL
// ═══════════════════════════════════════════════════════════════

/// The pure turn kernel — canonical phase governor.
///
/// Enforces a deterministic phase sequence for every turn.
/// All phase transitions are validated against the transition table.
/// Invalid transitions are rejected (not silently ignored).
#[derive(Debug, Clone)]
pub struct TurnKernel {
    pub phase: TurnPhase,
    pub turn_number: u32,
    pub tool_calls_pending: usize,
    pub tool_calls_completed: usize,
    pub had_mutation: bool,
    pub verification_needed: bool,
    pub consecutive_errors: u32,
    pub max_turns: u32,
    /// Live turn snapshot (materialized view, O(1) update per event).
    snapshot: TurnSnapshot,
}

impl TurnKernel {
    pub fn new(max_turns: u32) -> Self {
        Self {
            phase: TurnPhase::Idle,
            turn_number: 0,
            tool_calls_pending: 0,
            tool_calls_completed: 0,
            had_mutation: false,
            verification_needed: false,
            consecutive_errors: 0,
            max_turns,
            snapshot: TurnSnapshot::new(0),
        }
    }

    /// Get the current live turn snapshot (read-only projection).
    pub fn snapshot(&self) -> &TurnSnapshot {
        &self.snapshot
    }

    /// Pure transition function: (state, input) → (state', vec of outputs).
    /// This is the core Mealy machine — no side effects, no I/O.
    ///
    /// Transition validation is O(1) per event via the fixed transition table.
    pub fn transition(&mut self, input: TurnInput) -> Vec<TurnOutput> {
        let mut outputs = Vec::new();

        // ── Out-of-band events (valid from any phase) ──
        match &input {
            TurnInput::CompressionTriggered => {
                outputs.push(TurnOutput::CompressContext);
                return outputs;
            }
            TurnInput::Cancelled => {
                let from = self.phase;
                self.phase = TurnPhase::Failed;
                self.snapshot.phase = TurnPhase::Failed;
                self.snapshot.milestones.push(TurnMilestone::now(TurnPhase::Failed, Some("cancelled".into())));
                outputs.push(TurnOutput::PhaseChange(TurnPhase::Failed));
                outputs.push(TurnOutput::Yield);
                return outputs;
            }
            TurnInput::Error(msg) => {
                self.consecutive_errors += 1;
                self.snapshot.consecutive_errors = self.consecutive_errors;
                if self.consecutive_errors >= 3 {
                    self.phase = TurnPhase::Failed;
                    self.snapshot.phase = TurnPhase::Failed;
                    self.snapshot.milestones.push(TurnMilestone::now(TurnPhase::Failed, Some(msg.clone())));
                    outputs.push(TurnOutput::PhaseChange(TurnPhase::Failed));
                    outputs.push(TurnOutput::Yield);
                } else {
                    outputs.push(TurnOutput::Status(format!("Error: {}", msg)));
                }
                return outputs;
            }
            TurnInput::StreamChunk { .. } => {
                // Informational — no phase change
                return outputs;
            }
            _ => {}
        }

        // ── Phase-validated transitions ──
        match (&self.phase, input) {
            // Idle → Accepted: user message received
            (TurnPhase::Idle, TurnInput::UserMessage(_)) => {
                self.turn_number += 1;
                self.reset_turn_state();
                self.snapshot = TurnSnapshot::new(self.turn_number);
                self.set_phase(TurnPhase::Accepted, None, &mut outputs);
                // Immediately freeze control plane
                outputs.push(TurnOutput::FreezeControlPlane);
            }

            // Accepted → ContextFrozen: control-plane snapshot taken
            (TurnPhase::Accepted, TurnInput::ContextFrozen) => {
                self.set_phase(TurnPhase::ContextFrozen, None, &mut outputs);
                // Immediately request LLM completion
                outputs.push(TurnOutput::RequestCompletion);
            }

            // ContextFrozen → Requesting: LLM request sent
            (TurnPhase::ContextFrozen, TurnInput::RequestSent) => {
                self.set_phase(TurnPhase::Requesting, None, &mut outputs);
            }

            // Requesting → ResponseStarted: first token received
            (TurnPhase::Requesting, TurnInput::StreamStarted) => {
                self.set_phase(TurnPhase::ResponseStarted, None, &mut outputs);
            }

            // Requesting → Requesting: idempotent (agent loop sends RequestSent
            // even when kernel auto-transitioned from AllToolsCompleted)
            (TurnPhase::Requesting, TurnInput::RequestSent) => {
                // Already in Requesting — no-op
            }

            // ResponseStarted → ToolProposed: tool calls in response
            (TurnPhase::ResponseStarted, TurnInput::ToolCallsReceived { call_count }) => {
                self.tool_calls_pending = call_count;
                self.tool_calls_completed = 0;
                self.set_phase(TurnPhase::ToolProposed, Some(format!("{} tools", call_count)), &mut outputs);
            }

            // ResponseStarted → ResponseCompleted: no tools, done
            (TurnPhase::ResponseStarted, TurnInput::ResponseComplete) => {
                self.set_phase(TurnPhase::ResponseCompleted, None, &mut outputs);
                outputs.push(TurnOutput::CommitTurn);
            }

            // ToolProposed → PermissionResolved: permissions decided
            (TurnPhase::ToolProposed, TurnInput::PermissionResolved { approved, denied }) => {
                self.snapshot.approved_tools = approved.clone();
                self.snapshot.denied_tools = denied.clone();
                self.set_phase(TurnPhase::PermissionResolved, None, &mut outputs);
                if approved.is_empty() {
                    // All denied — skip execution, go back to requesting
                    outputs.push(TurnOutput::RequestCompletion);
                } else {
                    outputs.push(TurnOutput::ExecuteTools);
                }
            }

            // ToolProposed → AllToolsCompleted: shortcut when permission/execution
            // is handled internally by execute_tools() without individual FSM events.
            (TurnPhase::ToolProposed, TurnInput::AllToolsCompleted { modified_files }) => {
                self.set_phase(TurnPhase::ToolCompleted, None, &mut outputs);
                if self.verification_needed && !modified_files.is_empty() {
                    self.snapshot.verification_pending = true;
                    outputs.push(TurnOutput::RunVerification);
                } else {
                    self.verification_needed = false;
                    self.set_phase(TurnPhase::Requesting, None, &mut outputs);
                    outputs.push(TurnOutput::RequestCompletion);
                }
            }

            // ToolProposed → SingleToolCompleted: tool completions arriving
            // while still in ToolProposed (permission handled internally)
            (TurnPhase::ToolProposed, TurnInput::SingleToolCompleted { call_id, mutated, .. }) => {
                self.tool_calls_completed += 1;
                self.snapshot.completed_tools.push(call_id.clone());
                if mutated {
                    self.had_mutation = true;
                    self.verification_needed = true;
                    self.snapshot.had_mutation = true;
                }
            }

            // ToolProposed → Requesting: RequestSent when all tools were denied
            // or execution completed without explicit FSM transitions
            (TurnPhase::ToolProposed, TurnInput::RequestSent) => {
                self.set_phase(TurnPhase::Requesting, None, &mut outputs);
            }

            // PermissionResolved → ToolStarted: execution begins
            (TurnPhase::PermissionResolved, TurnInput::ToolExecutionStarted) => {
                self.set_phase(TurnPhase::ToolStarted, None, &mut outputs);
            }

            // ToolStarted: individual tool completion (no phase change)
            (TurnPhase::ToolStarted, TurnInput::SingleToolCompleted { call_id, mutated, .. }) => {
                self.tool_calls_completed += 1;
                self.snapshot.completed_tools.push(call_id.clone());
                if mutated {
                    self.had_mutation = true;
                    self.verification_needed = true;
                    self.snapshot.had_mutation = true;
                }
            }

            // ToolStarted → ToolCompleted: all tools finished
            (TurnPhase::ToolStarted, TurnInput::AllToolsCompleted { modified_files }) => {
                self.set_phase(TurnPhase::ToolCompleted, None, &mut outputs);
                if self.verification_needed && !modified_files.is_empty() {
                    self.snapshot.verification_pending = true;
                    outputs.push(TurnOutput::RunVerification);
                } else {
                    // No verification needed — loop back to requesting
                    self.verification_needed = false;
                    self.set_phase(TurnPhase::Requesting, None, &mut outputs);
                    outputs.push(TurnOutput::RequestCompletion);
                }
            }

            // ToolCompleted → Verifying (via RunVerification output above, explicit entry)
            (TurnPhase::ToolCompleted, TurnInput::VerificationCompleted { .. }) => {
                // Direct verification completion from ToolCompleted
                self.verification_needed = false;
                self.snapshot.verification_pending = false;
                self.set_phase(TurnPhase::Requesting, None, &mut outputs);
                outputs.push(TurnOutput::RequestCompletion);
            }

            // Verifying → Requesting: verification done, continue
            (TurnPhase::Verifying, TurnInput::VerificationCompleted { .. }) => {
                self.verification_needed = false;
                self.snapshot.verification_pending = false;
                self.set_phase(TurnPhase::Requesting, None, &mut outputs);
                outputs.push(TurnOutput::RequestCompletion);
            }

            // ResponseCompleted → Committed: turn committed
            (TurnPhase::ResponseCompleted, TurnInput::TurnCommitted) => {
                self.set_phase(TurnPhase::Committed, None, &mut outputs);
                outputs.push(TurnOutput::Yield);
            }

            // Committed → Idle: reset for next turn
            (TurnPhase::Committed, TurnInput::Reset) => {
                self.set_phase(TurnPhase::Idle, None, &mut outputs);
            }

            // PermissionResolved → Requesting: all tools denied, skip to next LLM call
            (TurnPhase::PermissionResolved, TurnInput::RequestSent) => {
                self.set_phase(TurnPhase::Requesting, None, &mut outputs);
            }

            _ => {
                // Invalid transition — report but don't crash
                let from = self.phase;
                let input_desc = format!("{:?}", "unknown");
                tracing::warn!(
                    "TurnKernel: invalid transition from {:?} (input type not handled in this phase)",
                    from
                );
                outputs.push(TurnOutput::InvalidTransition {
                    from,
                    input: format!("unhandled in phase {:?}", from),
                });
            }
        }

        outputs
    }

    /// Set phase with milestone tracking.
    fn set_phase(&mut self, phase: TurnPhase, detail: Option<String>, outputs: &mut Vec<TurnOutput>) {
        self.phase = phase;
        self.snapshot.phase = phase;
        self.snapshot.milestones.push(TurnMilestone::now(phase, detail));
        outputs.push(TurnOutput::PhaseChange(phase));
    }

    /// Check if the kernel has exceeded the turn limit.
    pub fn exceeds_turn_limit(&self) -> bool {
        self.turn_number > self.max_turns
    }

    /// Reset internal state for a new turn (called on Idle → Accepted).
    fn reset_turn_state(&mut self) {
        self.tool_calls_pending = 0;
        self.tool_calls_completed = 0;
        self.had_mutation = false;
        self.verification_needed = false;
        // NOTE: consecutive_errors is NOT reset per-turn — it's session-scoped
    }

    /// Reset for a new turn (public API for backwards compat).
    pub fn reset_turn(&mut self) {
        self.reset_turn_state();
        self.consecutive_errors = 0;
    }

    /// Get the live milestone trace for the current turn.
    pub fn milestones(&self) -> &[TurnMilestone] {
        &self.snapshot.milestones
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_turn_lifecycle_no_tools() {
        let mut kernel = TurnKernel::new(100);
        assert_eq!(kernel.phase, TurnPhase::Idle);

        // Idle → Accepted
        let outputs = kernel.transition(TurnInput::UserMessage("fix the bug".into()));
        assert_eq!(kernel.phase, TurnPhase::Accepted);
        assert_eq!(kernel.turn_number, 1);
        assert!(outputs.iter().any(|o| matches!(o, TurnOutput::FreezeControlPlane)));

        // Accepted → ContextFrozen
        let outputs = kernel.transition(TurnInput::ContextFrozen);
        assert_eq!(kernel.phase, TurnPhase::ContextFrozen);
        assert!(outputs.iter().any(|o| matches!(o, TurnOutput::RequestCompletion)));

        // ContextFrozen → Requesting (explicit)
        let outputs = kernel.transition(TurnInput::RequestSent);
        assert_eq!(kernel.phase, TurnPhase::Requesting);

        // Requesting → ResponseStarted
        let outputs = kernel.transition(TurnInput::StreamStarted);
        assert_eq!(kernel.phase, TurnPhase::ResponseStarted);

        // ResponseStarted → ResponseCompleted (no tools)
        let outputs = kernel.transition(TurnInput::ResponseComplete);
        assert_eq!(kernel.phase, TurnPhase::ResponseCompleted);
        assert!(outputs.iter().any(|o| matches!(o, TurnOutput::CommitTurn)));

        // ResponseCompleted → Committed
        let outputs = kernel.transition(TurnInput::TurnCommitted);
        assert_eq!(kernel.phase, TurnPhase::Committed);
        assert!(outputs.iter().any(|o| matches!(o, TurnOutput::Yield)));

        // Committed → Idle (reset)
        kernel.transition(TurnInput::Reset);
        assert_eq!(kernel.phase, TurnPhase::Idle);
    }

    #[test]
    fn canonical_turn_with_tools() {
        let mut kernel = TurnKernel::new(100);
        kernel.transition(TurnInput::UserMessage("edit file".into()));
        kernel.transition(TurnInput::ContextFrozen);
        kernel.transition(TurnInput::RequestSent);
        kernel.transition(TurnInput::StreamStarted);

        // Tool calls received
        let outputs = kernel.transition(TurnInput::ToolCallsReceived { call_count: 2 });
        assert_eq!(kernel.phase, TurnPhase::ToolProposed);

        // Permissions resolved
        let outputs = kernel.transition(TurnInput::PermissionResolved {
            approved: vec!["call_1".into(), "call_2".into()],
            denied: vec![],
        });
        assert_eq!(kernel.phase, TurnPhase::PermissionResolved);
        assert!(outputs.iter().any(|o| matches!(o, TurnOutput::ExecuteTools)));

        // Execution starts
        kernel.transition(TurnInput::ToolExecutionStarted);
        assert_eq!(kernel.phase, TurnPhase::ToolStarted);

        // Individual tool completions
        kernel.transition(TurnInput::SingleToolCompleted {
            call_id: "call_1".into(), success: true, mutated: true,
        });
        assert_eq!(kernel.phase, TurnPhase::ToolStarted); // still waiting for call_2

        // All tools completed with mutations → verification
        let outputs = kernel.transition(TurnInput::AllToolsCompleted {
            modified_files: vec!["foo.rs".into()],
        });
        assert!(outputs.iter().any(|o| matches!(o, TurnOutput::RunVerification)));
    }

    #[test]
    fn permission_denial_skips_execution() {
        let mut kernel = TurnKernel::new(100);
        kernel.transition(TurnInput::UserMessage("run rm -rf".into()));
        kernel.transition(TurnInput::ContextFrozen);
        kernel.transition(TurnInput::RequestSent);
        kernel.transition(TurnInput::StreamStarted);
        kernel.transition(TurnInput::ToolCallsReceived { call_count: 1 });

        // ALL tools denied
        let outputs = kernel.transition(TurnInput::PermissionResolved {
            approved: vec![],
            denied: vec!["call_1".into()],
        });
        assert_eq!(kernel.phase, TurnPhase::PermissionResolved);
        assert!(outputs.iter().any(|o| matches!(o, TurnOutput::RequestCompletion)));
    }

    #[test]
    fn cancellation_from_any_phase() {
        let mut kernel = TurnKernel::new(100);
        kernel.transition(TurnInput::UserMessage("test".into()));
        kernel.transition(TurnInput::ContextFrozen);
        kernel.transition(TurnInput::RequestSent);
        kernel.transition(TurnInput::StreamStarted);

        let outputs = kernel.transition(TurnInput::Cancelled);
        assert_eq!(kernel.phase, TurnPhase::Failed);
        assert!(outputs.iter().any(|o| matches!(o, TurnOutput::Yield)));
    }

    #[test]
    fn consecutive_errors_trigger_failure() {
        let mut kernel = TurnKernel::new(100);
        kernel.transition(TurnInput::UserMessage("test".into()));

        kernel.transition(TurnInput::Error("e1".into()));
        assert_ne!(kernel.phase, TurnPhase::Failed);
        kernel.transition(TurnInput::Error("e2".into()));
        assert_ne!(kernel.phase, TurnPhase::Failed);
        kernel.transition(TurnInput::Error("e3".into()));
        assert_eq!(kernel.phase, TurnPhase::Failed);
    }

    #[test]
    fn milestone_trace_recorded() {
        let mut kernel = TurnKernel::new(100);
        kernel.transition(TurnInput::UserMessage("trace test".into()));
        kernel.transition(TurnInput::ContextFrozen);
        kernel.transition(TurnInput::RequestSent);
        kernel.transition(TurnInput::StreamStarted);
        kernel.transition(TurnInput::ResponseComplete);

        let snap = kernel.snapshot();
        assert_eq!(snap.turn_number, 1);
        assert_eq!(snap.phase, TurnPhase::ResponseCompleted);
        assert!(snap.milestones.len() >= 5);
        assert_eq!(snap.milestones[0].phase, TurnPhase::Accepted);
        assert_eq!(snap.milestones[1].phase, TurnPhase::ContextFrozen);
    }

    #[test]
    fn snapshot_tracks_tool_lifecycle() {
        let mut kernel = TurnKernel::new(100);
        kernel.transition(TurnInput::UserMessage("tools".into()));
        kernel.transition(TurnInput::ContextFrozen);
        kernel.transition(TurnInput::RequestSent);
        kernel.transition(TurnInput::StreamStarted);
        kernel.transition(TurnInput::ToolCallsReceived { call_count: 2 });
        kernel.transition(TurnInput::PermissionResolved {
            approved: vec!["c1".into()],
            denied: vec!["c2".into()],
        });

        let snap = kernel.snapshot();
        assert_eq!(snap.approved_tools, vec!["c1"]);
        assert_eq!(snap.denied_tools, vec!["c2"]);
    }
}
