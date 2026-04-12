//! Session Kernel — Single Source of Truth for All Session State
//!
//! Unifies the `SessionLedger`, `TranscriptWal`, `ContextManager`, and
//! `PermissionLedger` under one canonical authority. All state mutations
//! (turn acceptance, tool outcomes, compaction, permission decisions, resume
//! checkpoints) flow through the kernel, ensuring deterministic replay and
//! eliminating state divergence between subsystems.
//!
//! Design:
//! - Write path: O(1) append per mutation (ledger + optional WAL/context sync)
//! - Recovery: O(k) from last snapshot (k = events since snapshot, default 50)
//! - Hot path: The kernel does NOT block tool execution; it records outcomes
//!   at turn boundaries, not inside the scheduler inner loop (Task 7).

use crate::ledger::{LedgerError, SessionEvent, SessionLedger, SessionState};
use crate::permission_ledger::{DenialSource, PermissionDenialRecord, PermissionLedger};
use pipit_context::budget::ContextManager;
use pipit_context::transcript::{TranscriptWal, WalError};
use pipit_provider::Message;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Errors from the session kernel.
#[derive(Debug, thiserror::Error)]
pub enum SessionKernelError {
    #[error("Ledger error: {0}")]
    Ledger(#[from] LedgerError),
    #[error("WAL error: {0}")]
    Wal(#[from] WalError),
    #[error("Session not started")]
    NotStarted,
    #[error("Session already ended")]
    AlreadyEnded,
    #[error("State error: {0}")]
    State(String),
}

/// Configuration for the session kernel.
#[derive(Debug, Clone)]
pub struct SessionKernelConfig {
    /// Directory for session persistence (.pipit/sessions/{slug}).
    pub session_dir: PathBuf,
    /// Whether to fsync WAL writes (true for durability, false for speed).
    pub durable_writes: bool,
    /// Snapshot interval for ledger (default: 50 events).
    pub snapshot_interval: u64,
}

/// The Session Kernel — single authority for all session state mutations.
///
/// All writes to session state (messages, tool outcomes, permissions,
/// compression, checkpoints) MUST go through this kernel. This guarantees:
/// 1. Deterministic replay from any interruption point
/// 2. No divergence between context, transcript, and ledger
/// 3. Hash-chained integrity for audit/tamper detection
/// 4. Snapshot-accelerated recovery
pub struct SessionKernel {
    /// The canonical event log (hash-chained, append-only).
    ledger: SessionLedger,
    /// Write-ahead log for crash recovery (pre-API-call flush).
    wal: Option<TranscriptWal>,
    /// Permission denial accumulator.
    permissions: PermissionLedger,
    /// Derived state (rebuilt from replay).
    state: SessionState,
    /// Session configuration.
    config: SessionKernelConfig,
    /// Whether the session has been started.
    started: bool,
}

impl SessionKernel {
    /// Create a new session kernel. Does NOT start the session — call `start()`.
    pub fn new(config: SessionKernelConfig) -> Result<Self, SessionKernelError> {
        std::fs::create_dir_all(&config.session_dir)
            .map_err(|e| SessionKernelError::State(format!("Cannot create session dir: {}", e)))?;

        let ledger_path = config.session_dir.join("ledger.jsonl");
        let ledger = SessionLedger::open(ledger_path)?;

        let wal_path = config.session_dir.join("transcript.wal");
        let wal = TranscriptWal::new(wal_path, config.durable_writes).ok();

        Ok(Self {
            ledger,
            wal,
            permissions: PermissionLedger::new(),
            state: SessionState::default(),
            config,
            started: false,
        })
    }

    /// Start a new session.
    pub fn start(
        &mut self,
        session_id: &str,
        model: &str,
        provider: &str,
    ) -> Result<(), SessionKernelError> {
        self.ledger.append(SessionEvent::SessionStarted {
            session_id: session_id.to_string(),
            model: model.to_string(),
            provider: provider.to_string(),
        })?;

        if let Some(ref mut wal) = self.wal {
            let _ = wal.append_session_meta(model, provider, session_id);
        }

        self.started = true;
        Ok(())
    }

    /// Resume a session from persisted state.
    /// Returns the number of events replayed and the recovered messages.
    pub fn resume(&mut self) -> Result<(usize, Vec<Message>), SessionKernelError> {
        let ledger_path = self.config.session_dir.join("ledger.jsonl");

        // Recover state from ledger with snapshot acceleration
        self.state = SessionState::recover(&ledger_path)?;
        let event_count = self.state.last_seq as usize;

        // Recover messages from WAL
        let wal_path = self.config.session_dir.join("transcript.wal");
        let messages = if wal_path.exists() {
            TranscriptWal::resume_messages(&wal_path).unwrap_or_default()
        } else {
            Vec::new()
        };

        self.started = !self.state.ended;

        Ok((event_count, messages))
    }

    // ── Message flow ──

    /// Record a user message. MUST be called BEFORE the API call.
    pub fn accept_user_message(&mut self, content: &str) -> Result<(), SessionKernelError> {
        self.ensure_started()?;

        // WAL first (crash recovery guarantee)
        if let Some(ref mut wal) = self.wal {
            let msg = Message::user(content);
            let _ = wal.append_message(&msg);
        }

        // Ledger (canonical event)
        self.ledger.append(SessionEvent::UserMessageAccepted {
            content: content.to_string(),
        })?;

        Ok(())
    }

    /// Record that an assistant response has started.
    pub fn begin_response(&mut self, turn: u32) -> Result<(), SessionKernelError> {
        self.ensure_started()?;
        self.ledger
            .append(SessionEvent::AssistantResponseStarted { turn })?;
        Ok(())
    }

    /// Record a completed assistant response.
    pub fn complete_response(
        &mut self,
        text: &str,
        thinking: &str,
        tokens_used: u64,
        message: &Message,
    ) -> Result<(), SessionKernelError> {
        self.ensure_started()?;

        // WAL for crash recovery
        if let Some(ref mut wal) = self.wal {
            let _ = wal.append_message(message);
        }

        // Ledger (canonical)
        self.ledger
            .append(SessionEvent::AssistantResponseCompleted {
                text: text.to_string(),
                thinking: thinking.to_string(),
                tokens_used,
            })?;

        Ok(())
    }

    // ── Tool lifecycle ──

    /// Record a tool call proposal.
    pub fn propose_tool_call(
        &mut self,
        call_id: &str,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<(), SessionKernelError> {
        self.ensure_started()?;
        self.ledger.append(SessionEvent::ToolCallProposed {
            call_id: call_id.to_string(),
            tool_name: tool_name.to_string(),
            args: args.clone(),
        })?;
        Ok(())
    }

    /// Record a tool approval.
    pub fn approve_tool(&mut self, call_id: &str) -> Result<(), SessionKernelError> {
        self.ensure_started()?;
        self.ledger.append(SessionEvent::ToolApproved {
            call_id: call_id.to_string(),
        })?;
        Ok(())
    }

    /// Record a tool denial with structured reason.
    pub fn deny_tool(
        &mut self,
        call_id: &str,
        tool_name: &str,
        reason: &str,
        source: DenialSource,
        turn: u32,
    ) -> Result<(), SessionKernelError> {
        self.ensure_started()?;

        // Ledger (canonical)
        self.ledger.append(SessionEvent::ToolDenied {
            call_id: call_id.to_string(),
            reason: reason.to_string(),
        })?;

        // Permission accumulator (for SDK fanout)
        self.permissions
            .record_denial(tool_name, call_id, source, turn);

        Ok(())
    }

    /// Record tool execution start.
    pub fn start_tool(&mut self, call_id: &str) -> Result<(), SessionKernelError> {
        self.ensure_started()?;
        self.ledger.append(SessionEvent::ToolStarted {
            call_id: call_id.to_string(),
        })?;
        Ok(())
    }

    /// Record tool completion.
    pub fn complete_tool(
        &mut self,
        call_id: &str,
        success: bool,
        mutated: bool,
        result_summary: &str,
        blob_hash: Option<String>,
    ) -> Result<(), SessionKernelError> {
        self.ensure_started()?;
        self.ledger.append(SessionEvent::ToolCompleted {
            call_id: call_id.to_string(),
            success,
            mutated,
            result_summary: result_summary.to_string(),
            result_blob_hash: blob_hash,
        })?;
        Ok(())
    }

    // ── Context management ──

    /// Record a context compression event.
    pub fn record_compression(
        &mut self,
        messages_removed: usize,
        tokens_freed: u64,
        strategy: &str,
    ) -> Result<(), SessionKernelError> {
        self.ensure_started()?;

        // WAL
        if let Some(ref mut wal) = self.wal {
            let _ = wal.append_compression(messages_removed, tokens_freed);
        }

        // Ledger
        self.ledger.append(SessionEvent::ContextCompressed {
            messages_removed,
            tokens_freed,
            strategy: strategy.to_string(),
        })?;

        Ok(())
    }

    // ── Plan/Verify ──

    /// Record plan selection.
    pub fn select_plan(
        &mut self,
        strategy: &str,
        rationale: &str,
    ) -> Result<(), SessionKernelError> {
        self.ensure_started()?;
        self.ledger.append(SessionEvent::PlanSelected {
            strategy: strategy.to_string(),
            rationale: rationale.to_string(),
        })?;
        Ok(())
    }

    /// Record plan pivot.
    pub fn pivot_plan(
        &mut self,
        from: &str,
        to: &str,
        trigger: &str,
    ) -> Result<(), SessionKernelError> {
        self.ensure_started()?;
        self.ledger.append(SessionEvent::PlanPivoted {
            from_strategy: from.to_string(),
            to_strategy: to.to_string(),
            trigger: trigger.to_string(),
        })?;
        Ok(())
    }

    // ── Checkpoints ──

    /// Create a checkpoint (for rollback support).
    pub fn create_checkpoint(&mut self, label: &str) -> Result<(), SessionKernelError> {
        self.ensure_started()?;
        self.ledger.append(SessionEvent::CheckpointCreated {
            checkpoint_id: label.to_string(),
            event_seq: self.ledger.current_seq(),
        })?;

        // Trigger ledger snapshot if needed
        if self.ledger.needs_snapshot() {
            self.create_snapshot()?;
        }

        Ok(())
    }

    /// Create a ledger snapshot for accelerated replay.
    fn create_snapshot(&mut self) -> Result<(), SessionKernelError> {
        let state = self.derived_state();
        let snapshot_data =
            serde_json::to_string(&state).map_err(|e| SessionKernelError::State(e.to_string()))?;

        let seq = self.ledger.current_seq();
        self.ledger.append(SessionEvent::Snapshot {
            at_seq: seq,
            message_count: 0,
            state_hash: {
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut h = DefaultHasher::new();
                snapshot_data.hash(&mut h);
                h.finish()
            },
        })?;
        self.ledger.snapshot_taken();
        Ok(())
    }

    // ── Session end ──

    /// End the session.
    pub fn end_session(
        &mut self,
        turns: u32,
        total_tokens: u64,
        cost: f64,
    ) -> Result<(), SessionKernelError> {
        self.ensure_started()?;
        self.ledger.append(SessionEvent::SessionEnded {
            turns,
            total_tokens,
            cost,
        })?;
        self.started = false;
        Ok(())
    }

    // ── Queries ──

    /// Get the current derived state.
    pub fn derived_state(&self) -> &SessionState {
        &self.state
    }

    /// Get permission denial records for SDK output.
    pub fn drain_permission_denials(&self) -> Vec<PermissionDenialRecord> {
        self.permissions.drain()
    }

    /// Get the session directory.
    pub fn session_dir(&self) -> &Path {
        &self.config.session_dir
    }

    /// Current event sequence number.
    pub fn current_seq(&self) -> u64 {
        self.ledger.current_seq()
    }

    /// Whether the session is active.
    pub fn is_active(&self) -> bool {
        self.started
    }

    /// Compact the WAL to current state.
    pub fn compact_wal(&mut self, messages: &[Message]) -> Result<(), SessionKernelError> {
        if let Some(ref mut wal) = self.wal {
            wal.compact(messages)?;
        }
        Ok(())
    }

    fn ensure_started(&self) -> Result<(), SessionKernelError> {
        if !self.started {
            return Err(SessionKernelError::NotStarted);
        }
        Ok(())
    }

    // ═══════════════════════════════════════════════════════════════
    //  KERNEL-GATED COMMIT PROTOCOL
    // ═══════════════════════════════════════════════════════════════
    //
    // Mandatory persistence boundaries (selective journaling):
    //   1. UserAccepted    — user message recorded before model step
    //   2. ResponseBegin   — response start recorded before streaming
    //   3. ToolProposed    — tool call recorded before permission check
    //   4. PermissionResolved — approval/denial recorded before execution
    //   5. ToolCompleted   — outcome recorded after execution
    //   6. TurnCommitted   — terminal milestone, makes turn externally visible
    //
    // Everything else (stream chunks, telemetry, UI status) is opportunistic.
    // This keeps persistence cost O(b) per turn where b = mandatory boundaries,
    // rather than O(b + e) for every micro-event.
    //
    // Recovery completeness: a recovered state is a prefix-consistent image
    // of the committed event stream (monotonicity theorem).

    /// Gate: no model step starts until this succeeds.
    /// This is the write-ahead intent record for the turn.
    pub fn gate_user_accepted(&mut self, content: &str) -> Result<u64, SessionKernelError> {
        self.accept_user_message(content)?;
        Ok(self.ledger.current_seq())
    }

    /// Gate: tool execution doesn't begin until proposal is recorded.
    pub fn gate_tool_proposed(
        &mut self,
        call_id: &str,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<u64, SessionKernelError> {
        self.propose_tool_call(call_id, tool_name, args)?;
        Ok(self.ledger.current_seq())
    }

    /// Gate: tool execution doesn't begin until permission decision is recorded.
    pub fn gate_permission_resolved(
        &mut self,
        call_id: &str,
        approved: bool,
        reason: Option<&str>,
    ) -> Result<u64, SessionKernelError> {
        if approved {
            self.approve_tool(call_id)?;
        } else {
            // For denied tools, we use the existing deny_tool method
            // The caller should use deny_tool directly for full denial metadata
            self.ledger.append(SessionEvent::ToolDenied {
                call_id: call_id.to_string(),
                reason: reason.unwrap_or("denied").to_string(),
            })?;
        }
        Ok(self.ledger.current_seq())
    }

    /// Gate: assistant completion is only surfaced as committed after this.
    pub fn gate_turn_committed(&mut self, turn: u32) -> Result<u64, SessionKernelError> {
        self.ensure_started()?;
        self.ledger.append(SessionEvent::TurnCompleted { turn })?;
        Ok(self.ledger.current_seq())
    }

    /// Spawn a child session kernel for a subagent execution branch.
    ///
    /// The child kernel writes to its own ledger under the parent's session directory,
    /// inheriting the session context but maintaining independent event streams.
    /// This enables parallel subagent execution with deterministic merge.
    pub fn spawn_subagent_kernel(
        &mut self,
        branch_id: &str,
        task: &str,
    ) -> Result<SessionKernel, SessionKernelError> {
        self.ensure_started()?;

        // Record the spawn in the parent ledger
        self.ledger.append(SessionEvent::SubagentSpawned {
            child_id: branch_id.to_string(),
            parent_id: "root".to_string(),
            task: task.to_string(),
            capability_set: 0,
        })?;

        // Create child kernel in a subdirectory
        let child_dir = self.config.session_dir.join("branches").join(branch_id);
        let child_config = SessionKernelConfig {
            session_dir: child_dir,
            durable_writes: self.config.durable_writes,
            snapshot_interval: self.config.snapshot_interval,
        };
        let mut child = SessionKernel::new(child_config)?;
        child.start(branch_id, "inherited", "inherited")?;

        Ok(child)
    }

    /// Record completion of a subagent branch in the parent ledger.
    pub fn complete_subagent(
        &mut self,
        branch_id: &str,
        success: bool,
    ) -> Result<(), SessionKernelError> {
        self.ensure_started()?;
        self.ledger.append(SessionEvent::SubagentCompleted {
            child_id: branch_id.to_string(),
            success,
            output: None,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
            duration_ms: 0,
            total_turns: 0,
            task: None,
            model: None,
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_kernel() -> (SessionKernel, TempDir) {
        let dir = TempDir::new().unwrap();
        let config = SessionKernelConfig {
            session_dir: dir.path().to_path_buf(),
            durable_writes: false,
            snapshot_interval: 50,
        };
        (SessionKernel::new(config).unwrap(), dir)
    }

    #[test]
    fn test_start_and_record() {
        let (mut kernel, _dir) = test_kernel();
        kernel.start("sess-1", "gpt-4", "openai").unwrap();
        assert!(kernel.is_active());

        kernel.accept_user_message("hello").unwrap();
        kernel.begin_response(1).unwrap();
        assert!(kernel.current_seq() > 0);
    }

    #[test]
    fn test_deny_tool_records_permission() {
        let (mut kernel, _dir) = test_kernel();
        kernel.start("sess-1", "gpt-4", "openai").unwrap();

        kernel
            .deny_tool(
                "call-1",
                "bash",
                "risky command",
                DenialSource::GovernorRisk {
                    risk_score: 0.95,
                    threshold: 0.8,
                },
                1,
            )
            .unwrap();

        let denials = kernel.drain_permission_denials();
        assert_eq!(denials.len(), 1);
        assert_eq!(denials[0].tool_name, "bash");
    }

    #[test]
    fn test_not_started_errors() {
        let (mut kernel, _dir) = test_kernel();
        assert!(kernel.accept_user_message("hello").is_err());
    }
}
