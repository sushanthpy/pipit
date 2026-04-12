use crate::planner::CandidatePlan;
use crate::proof::ProofPacket;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

/// The user's response to an approval prompt.
#[derive(Debug, Clone)]
pub enum ApprovalDecision {
    /// Approve — execute the tool as proposed.
    Approve,
    /// Deny — skip this tool call and tell the agent it was rejected.
    Deny,
    /// Approve with scoped constraints — execute only within the grant boundary.
    /// This replaces binary approval with fine-grained, machine-enforceable grants.
    ScopedGrant(CapabilityGrant),
}

/// A typed, time-bounded, constraint-carrying authorization artifact.
///
/// This is the bridge between the approval system and the executor:
/// instead of "yes/no," operators can express "approve only this file path,"
/// "approve only for 60 seconds," or "approve only these commands."
///
/// Validation cost: O(1) for expiry + nonce, O(k) for constraint checks.
#[derive(Debug, Clone)]
pub struct CapabilityGrant {
    /// Which tool this grant applies to (exact match or "*" for any).
    pub tool_pattern: String,
    /// Constraints that must be satisfied for execution to proceed.
    pub constraints: Vec<GrantConstraint>,
    /// When this grant was issued (unix timestamp ms).
    pub issued_at: u64,
    /// When this grant expires (unix timestamp ms). 0 = no expiry.
    pub expires_at: u64,
    /// Unique nonce to prevent replay.
    pub nonce: String,
    /// Task ID this grant is scoped to.
    pub subject_task_id: Option<String>,
}

/// Individual constraint within a capability grant.
#[derive(Debug, Clone)]
pub enum GrantConstraint {
    /// Only allow operations on paths with this prefix.
    PathPrefix(String),
    /// Maximum bytes that may be written in a single operation.
    MaxBytesWritten(u64),
    /// Only allow these specific binaries in shell execution.
    AllowedBinaries(Vec<String>),
    /// Only allow network access to these hosts.
    AllowedHosts(Vec<String>),
    /// Only allow this many invocations before the grant is exhausted.
    MaxInvocations(u32),
    /// Custom predicate (key = constraint name, value = constraint parameter).
    Custom { key: String, value: String },
}

impl CapabilityGrant {
    /// Check whether this grant has expired.
    pub fn is_expired(&self) -> bool {
        if self.expires_at == 0 {
            return false;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        now > self.expires_at
    }

    /// Validate tool args against all constraints. Returns Ok(()) if all pass.
    pub fn validate_constraints(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<(), String> {
        // Check tool pattern
        if self.tool_pattern != "*" && self.tool_pattern != tool_name {
            return Err(format!(
                "grant is for tool '{}', not '{}'",
                self.tool_pattern, tool_name
            ));
        }

        // Check expiry
        if self.is_expired() {
            return Err("grant has expired".to_string());
        }

        // Check each constraint
        for constraint in &self.constraints {
            match constraint {
                GrantConstraint::PathPrefix(prefix) => {
                    if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                        if !path.starts_with(prefix.as_str()) {
                            return Err(format!(
                                "path '{}' does not match required prefix '{}'",
                                path, prefix
                            ));
                        }
                    }
                }
                GrantConstraint::MaxBytesWritten(max) => {
                    if let Some(content) = args.get("content").and_then(|v| v.as_str()) {
                        if content.len() as u64 > *max {
                            return Err(format!(
                                "content size {} exceeds max_bytes_written {}",
                                content.len(),
                                max
                            ));
                        }
                    }
                }
                GrantConstraint::AllowedBinaries(allowed) => {
                    if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                        let first_token = cmd.split_whitespace().next().unwrap_or("");
                        let binary = first_token.rsplit('/').next().unwrap_or(first_token);
                        if !allowed.iter().any(|b| b == binary) {
                            return Err(format!(
                                "binary '{}' is not in the allowed list: {:?}",
                                binary, allowed
                            ));
                        }
                    }
                }
                GrantConstraint::AllowedHosts(allowed) => {
                    if let Some(url) = args.get("url").and_then(|v| v.as_str()) {
                        let host_matches = allowed.iter().any(|h| url.contains(h));
                        if !host_matches {
                            return Err(format!("host not in allowed list: {:?}", allowed));
                        }
                    }
                }
                GrantConstraint::MaxInvocations(_) => {
                    // Invocation counting is handled externally
                }
                GrantConstraint::Custom { key, value } => {
                    tracing::debug!(
                        constraint_key = key,
                        constraint_value = value,
                        "custom constraint not validated at grant level"
                    );
                }
            }
        }

        Ok(())
    }
}

/// Trait for handling approval prompts. The CLI provides an implementation
/// that renders the approval card and blocks on stdin.
#[async_trait]
pub trait ApprovalHandler: Send + Sync {
    /// Called when a tool requires approval. Must block until the user responds.
    async fn request_approval(
        &self,
        call_id: &str,
        tool_name: &str,
        args: &Value,
    ) -> ApprovalDecision;
}

/// No-op approval handler that denies everything (for non-interactive builds).
pub struct DenyAllApprovalHandler;

#[async_trait]
impl ApprovalHandler for DenyAllApprovalHandler {
    async fn request_approval(
        &self,
        _call_id: &str,
        _tool_name: &str,
        _args: &Value,
    ) -> ApprovalDecision {
        ApprovalDecision::Deny
    }
}

/// Auto-approve handler for FullAuto mode (should never be called, but safe fallback).
pub struct AutoApproveHandler;

#[async_trait]
impl ApprovalHandler for AutoApproveHandler {
    async fn request_approval(
        &self,
        _call_id: &str,
        _tool_name: &str,
        _args: &Value,
    ) -> ApprovalDecision {
        ApprovalDecision::Approve
    }
}

/// Events emitted by the agent loop. Every subscriber sees every event.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    // --- Lifecycle ---
    TurnStart {
        turn_number: u32,
    },
    TurnEnd {
        turn_number: u32,
        reason: TurnEndReason,
    },

    // --- Streaming ---
    ContentDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    ContentComplete {
        full_text: String,
    },

    // --- Tool calls ---
    ToolCallStart {
        call_id: String,
        name: String,
        args: Value,
    },
    ToolCallEnd {
        call_id: String,
        name: String,
        result: ToolCallOutcome,
        /// Wall-clock milliseconds the tool took to execute.
        duration_ms: u64,
    },
    ToolApprovalNeeded {
        call_id: String,
        name: String,
        args: Value,
    },

    // --- Context ---
    CompressionStart,
    CompressionEnd {
        messages_removed: usize,
        tokens_freed: u64,
    },
    TokenUsageUpdate {
        used: u64,
        limit: u64,
        cost: f64,
    },
    PlanSelected {
        strategy: String,
        rationale: String,
        pivoted: bool,
        candidate_plans: Vec<CandidatePlan>,
    },

    // --- Errors ---
    ProviderError {
        error: String,
        will_retry: bool,
    },
    ToolError {
        call_id: String,
        error: String,
    },
    LoopDetected {
        tool_name: String,
        count: u32,
    },

    // --- Steering ---
    SteeringMessageInjected {
        text: String,
    },

    // --- Status ---
    /// Agent is busy with a phase that doesn't stream tokens (planning, verifying, etc.)
    Waiting {
        label: String,
    },
    /// Adaptive turn budget was extended — UI should update the displayed limit.
    BudgetExtended {
        new_approved: u32,
    },

    // --- PEV phase transitions ---
    /// Phase changed in the PEV orchestrator.
    PhaseTransition {
        from: String,
        to: String,
        mode: String,
    },
    /// Verifier produced a verdict.
    VerifierVerdict {
        verdict: String,
        confidence: f32,
        findings_summary: String,
    },
    /// Repair loop started.
    RepairStarted {
        attempt: u32,
        reason: String,
    },

    // --- Turn State Machine (canonical FSM events) ---
    /// A canonical turn phase was entered. Derived from TurnKernel.
    /// Single-source: UI, telemetry, logs, and replay all derive from this.
    TurnPhaseEntered {
        turn: u32,
        phase: String,
        detail: Option<String>,
        timestamp_ms: u64,
    },
}

#[derive(Debug, Clone)]
pub enum TurnEndReason {
    Complete,
    ToolsExecuted,
    MaxTurns,
    Error,
    Cancelled,
}

#[derive(Debug, Clone)]
pub enum ToolCallOutcome {
    Success {
        content: String,
        mutated: bool,
        /// Evidence artifacts from typed tools.
        artifacts: Vec<pipit_tools::typed_tool::ArtifactKind>,
        /// Realized file edits from typed tools.
        edits: Vec<pipit_tools::typed_tool::RealizedEdit>,
    },
    PolicyBlocked {
        message: String,
        stage: crate::proof::PolicyStage,
        mutated: bool,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone)]
pub enum AgentOutcome {
    Completed {
        turns: u32,
        total_tokens: u64,
        cost: f64,
        proof: ProofPacket,
    },
    MaxTurnsReached(u32),
    BudgetExhausted {
        turns: u32,
        cost: f64,
        budget: f64,
    },
    Cancelled,
    Error(String),
}

// ═══════════════════════════════════════════════════════════════════════
//  Reactive Runtime Event Bus
// ═══════════════════════════════════════════════════════════════════════

/// Global monotonic sequence counter for event ordering.
static GLOBAL_SEQ: AtomicU64 = AtomicU64::new(1);

/// Canonical runtime event — wraps AgentEvent with a monotonic sequence
/// number and timestamp for replayable, ordered multi-consumer fanout.
///
/// This is the single source of truth for all surfaces (CLI, TUI, SDK, daemon).
/// Event append is O(1); fanout per subscriber is O(1) amortized.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeEvent {
    /// Monotonically increasing sequence number (global across the process).
    pub seq: u64,
    /// Unix timestamp in milliseconds.
    pub timestamp_ms: u64,
    /// The event payload.
    pub kind: RuntimeEventKind,
}

/// Typed runtime event kinds — the canonical wire format for all consumers.
/// This collapses the distinction between internal richness and external thinness.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RuntimeEventKind {
    // ── Turn lifecycle ──
    TurnStart {
        turn_number: u32,
    },
    TurnEnd {
        turn_number: u32,
        reason: String,
    },

    // ── Content streaming ──
    ContentDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    ContentComplete {
        full_text: String,
    },

    // ── Tool lifecycle ──
    ToolCallStart {
        call_id: String,
        name: String,
        args: Value,
    },
    ToolCallEnd {
        call_id: String,
        name: String,
        success: bool,
        mutated: bool,
        summary: String,
    },
    ToolApprovalNeeded {
        call_id: String,
        name: String,
        args: Value,
    },

    // ── Planning & verification ──
    PlanSelected {
        strategy: String,
        rationale: String,
        pivoted: bool,
    },
    VerifierVerdict {
        verdict: String,
        confidence: f32,
        summary: String,
    },
    RepairStarted {
        attempt: u32,
        reason: String,
    },
    PhaseTransition {
        from: String,
        to: String,
    },

    // ── Context management ──
    CompressionStart,
    CompressionEnd {
        messages_removed: usize,
        tokens_freed: u64,
    },
    TokenUsage {
        used: u64,
        limit: u64,
        cost: f64,
    },

    // ── Status & control ──
    Waiting {
        label: String,
    },
    SteeringInjected {
        text: String,
    },
    LoopDetected {
        tool_name: String,
        count: u32,
    },

    // ── Errors ──
    ProviderError {
        error: String,
        will_retry: bool,
    },

    // ── Canonical turn FSM ──
    TurnPhaseEntered {
        turn: u32,
        phase: String,
        detail: Option<String>,
        timestamp_ms: u64,
    },

    // ── Termination ──
    SessionEnded {
        outcome: String,
    },
}

impl RuntimeEvent {
    /// Create a new runtime event with auto-incrementing sequence number.
    pub fn new(kind: RuntimeEventKind) -> Self {
        Self {
            seq: GLOBAL_SEQ.fetch_add(1, Ordering::Relaxed),
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            kind,
        }
    }

    /// Convert from an AgentEvent to a RuntimeEvent.
    pub fn from_agent_event(event: &AgentEvent) -> Option<Self> {
        let kind = match event {
            AgentEvent::TurnStart { turn_number } => RuntimeEventKind::TurnStart {
                turn_number: *turn_number,
            },
            AgentEvent::TurnEnd {
                turn_number,
                reason,
            } => RuntimeEventKind::TurnEnd {
                turn_number: *turn_number,
                reason: format!("{:?}", reason),
            },
            AgentEvent::ContentDelta { text } => {
                RuntimeEventKind::ContentDelta { text: text.clone() }
            }
            AgentEvent::ThinkingDelta { text } => {
                RuntimeEventKind::ThinkingDelta { text: text.clone() }
            }
            AgentEvent::ContentComplete { full_text } => RuntimeEventKind::ContentComplete {
                full_text: full_text.clone(),
            },
            AgentEvent::ToolCallStart {
                call_id,
                name,
                args,
            } => RuntimeEventKind::ToolCallStart {
                call_id: call_id.clone(),
                name: name.clone(),
                args: args.clone(),
            },
            AgentEvent::ToolCallEnd {
                call_id,
                name,
                result,
                ..
            } => {
                let (success, mutated, summary) = match result {
                    ToolCallOutcome::Success {
                        content, mutated, ..
                    } => (true, *mutated, content.chars().take(200).collect()),
                    ToolCallOutcome::PolicyBlocked { message, .. } => {
                        (false, false, message.clone())
                    }
                    ToolCallOutcome::Error { message } => (false, false, message.clone()),
                };
                RuntimeEventKind::ToolCallEnd {
                    call_id: call_id.clone(),
                    name: name.clone(),
                    success,
                    mutated,
                    summary,
                }
            }
            AgentEvent::ToolApprovalNeeded {
                call_id,
                name,
                args,
            } => RuntimeEventKind::ToolApprovalNeeded {
                call_id: call_id.clone(),
                name: name.clone(),
                args: args.clone(),
            },
            AgentEvent::PlanSelected {
                strategy,
                rationale,
                pivoted,
                ..
            } => RuntimeEventKind::PlanSelected {
                strategy: strategy.clone(),
                rationale: rationale.clone(),
                pivoted: *pivoted,
            },
            AgentEvent::VerifierVerdict {
                verdict,
                confidence,
                findings_summary,
            } => RuntimeEventKind::VerifierVerdict {
                verdict: verdict.clone(),
                confidence: *confidence,
                summary: findings_summary.clone(),
            },
            AgentEvent::RepairStarted { attempt, reason } => RuntimeEventKind::RepairStarted {
                attempt: *attempt,
                reason: reason.clone(),
            },
            AgentEvent::PhaseTransition { from, to, .. } => RuntimeEventKind::PhaseTransition {
                from: from.clone(),
                to: to.clone(),
            },
            AgentEvent::CompressionStart => RuntimeEventKind::CompressionStart,
            AgentEvent::CompressionEnd {
                messages_removed,
                tokens_freed,
            } => RuntimeEventKind::CompressionEnd {
                messages_removed: *messages_removed,
                tokens_freed: *tokens_freed,
            },
            AgentEvent::TokenUsageUpdate { used, limit, cost } => RuntimeEventKind::TokenUsage {
                used: *used,
                limit: *limit,
                cost: *cost,
            },
            AgentEvent::Waiting { label } => RuntimeEventKind::Waiting {
                label: label.clone(),
            },
            AgentEvent::SteeringMessageInjected { text } => {
                RuntimeEventKind::SteeringInjected { text: text.clone() }
            }
            AgentEvent::LoopDetected { tool_name, count } => RuntimeEventKind::LoopDetected {
                tool_name: tool_name.clone(),
                count: *count,
            },
            AgentEvent::ProviderError { error, will_retry } => RuntimeEventKind::ProviderError {
                error: error.clone(),
                will_retry: *will_retry,
            },
            AgentEvent::ToolError { .. } => return None,
            AgentEvent::TurnPhaseEntered {
                turn,
                phase,
                detail,
                timestamp_ms,
            } => RuntimeEventKind::TurnPhaseEntered {
                turn: *turn,
                phase: phase.clone(),
                detail: detail.clone(),
                timestamp_ms: *timestamp_ms,
            },
            AgentEvent::BudgetExtended { .. } => return None,
        };
        Some(Self::new(kind))
    }
}

/// A replay buffer for runtime events. Consumers can replay from any
/// sequence number to catch up after reconnection or lag.
pub struct RuntimeEventBuffer {
    events: std::collections::VecDeque<RuntimeEvent>,
    max_size: usize,
}

impl RuntimeEventBuffer {
    pub fn new(max_size: usize) -> Self {
        Self {
            events: std::collections::VecDeque::with_capacity(max_size),
            max_size,
        }
    }

    /// Append an event to the buffer.
    pub fn push(&mut self, event: RuntimeEvent) {
        if self.events.len() >= self.max_size {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    /// Replay events from a given sequence number (exclusive).
    /// Returns events with seq > from_seq.
    pub fn replay_from(&self, from_seq: u64) -> Vec<&RuntimeEvent> {
        self.events.iter().filter(|e| e.seq > from_seq).collect()
    }

    /// Get the latest sequence number.
    pub fn latest_seq(&self) -> u64 {
        self.events.back().map(|e| e.seq).unwrap_or(0)
    }

    /// Number of buffered events.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}
