use crate::planner::CandidatePlan;
use crate::proof::ProofPacket;
use async_trait::async_trait;
use serde_json::Value;

/// The user's response to an approval prompt.
#[derive(Debug, Clone)]
pub enum ApprovalDecision {
    /// Approve — execute the tool as proposed.
    Approve,
    /// Deny — skip this tool call and tell the agent it was rejected.
    Deny,
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
    TurnStart { turn_number: u32 },
    TurnEnd { turn_number: u32, reason: TurnEndReason },

    // --- Streaming ---
    ContentDelta { text: String },
    ThinkingDelta { text: String },
    ContentComplete { full_text: String },

    // --- Tool calls ---
    ToolCallStart { call_id: String, name: String, args: Value },
    ToolCallEnd { call_id: String, name: String, result: ToolCallOutcome },
    ToolApprovalNeeded { call_id: String, name: String, args: Value },

    // --- Context ---
    CompressionStart,
    CompressionEnd { messages_removed: usize, tokens_freed: u64 },
    TokenUsageUpdate { used: u64, limit: u64, cost: f64 },
    PlanSelected {
        strategy: String,
        rationale: String,
        pivoted: bool,
        candidate_plans: Vec<CandidatePlan>,
    },

    // --- Errors ---
    ProviderError { error: String, will_retry: bool },
    ToolError { call_id: String, error: String },
    LoopDetected { tool_name: String, count: u32 },

    // --- Steering ---
    SteeringMessageInjected { text: String },
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
    Success { content: String, mutated: bool },
    PolicyBlocked {
        message: String,
        stage: crate::proof::PolicyStage,
        mutated: bool,
    },
    Error { message: String },
}

#[derive(Debug, Clone)]
pub enum AgentOutcome {
    Completed { turns: u32, total_tokens: u64, cost: f64, proof: ProofPacket },
    MaxTurnsReached(u32),
    Cancelled,
    Error(String),
}
