//! Ports-and-Adapters Runtime Kernel (Architecture Task 8)
//!
//! Defines the minimal `AgentKernel` trait boundary around the core agent
//! subsystems. Everything else (CLI, TUI, SDK, daemon, browser, mesh,
//! voice, MCP) is an adapter that injects behavior through the kernel interfaces.
//!
//! The kernel owns: prompt build, policy evaluation, scheduler, context graph,
//! verifier, and artifact store. All approval, logging, persistence, progress,
//! and rendering are injected interfaces.

use crate::capability::{CapabilityRequest, ExecutionLineage, PolicyDecision};
use crate::events::AgentEvent;
use crate::governor::RiskReport;
use crate::proof::{ConfidenceReport, EvidenceArtifact, ProofPacket, RealizedEdit};
use crate::scheduler::ExecutionBatch;
use crate::tool_semantics::{SemanticClass, classify_semantically};
use pipit_provider::{CompletionRequest, ToolCall, ToolDeclaration};
use std::path::PathBuf;
use std::sync::Arc;

// ─── Kernel Ports (injected interfaces) ─────────────────────────────────

/// Port: How the kernel reports progress and events.
#[async_trait::async_trait]
pub trait ProgressPort: Send + Sync {
    /// Report an agent event to the surface (TUI, SDK, bridge, etc.).
    async fn report(&self, event: AgentEvent);
    /// Report a status label (e.g., "Sending to model…").
    async fn status(&self, label: &str);
}

/// Port: How the kernel requests user approval.
#[async_trait::async_trait]
pub trait ApprovalPort: Send + Sync {
    /// Request approval for a tool call. Blocks until decision.
    async fn request_approval(
        &self,
        call_id: &str,
        tool_name: &str,
        args: &serde_json::Value,
        policy_decision: &PolicyDecision,
    ) -> bool;
}

/// Port: How the kernel persists session state.
#[async_trait::async_trait]
pub trait PersistencePort: Send + Sync {
    /// Write an event to the session ledger.
    async fn write_event(&self, event: &crate::ledger::SessionEvent) -> Result<(), String>;
    /// Checkpoint current state.
    async fn checkpoint(&self) -> Result<String, String>;
    /// Rollback to a checkpoint.
    async fn rollback(&self, checkpoint_id: &str) -> Result<(), String>;
}

/// Port: How the kernel gets extension hooks.
#[async_trait::async_trait]
pub trait ExtensionPort: Send + Sync {
    /// Pre-process user input.
    async fn on_input(&self, text: &str) -> Result<Option<String>, String>;
    /// Transform system prompt before request.
    async fn on_before_request(&self, system: &str) -> Result<Option<String>, String>;
    /// Hook into content streaming.
    async fn on_content_delta(&self, text: &str) -> Result<(), String>;
    /// Pre-tool hook: may modify args.
    async fn on_before_tool(
        &self,
        name: &str,
        args: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>, String>;
    /// Post-tool hook.
    async fn on_after_tool(&self, name: &str, result: &str) -> Result<Option<String>, String>;
}

/// Port: How the kernel interacts with the VCS (git).
pub trait VcsPort: Send + Sync {
    /// Get current HEAD ref.
    fn head_ref(&self) -> Option<String>;
    /// Create a checkpoint (commit or stash).
    fn create_checkpoint(&self, message: &str) -> Result<String, String>;
    /// Rollback to a ref.
    fn rollback_to(&self, ref_str: &str) -> Result<(), String>;
    /// Get diff of modified files.
    fn diff(&self, files: &[String]) -> Result<String, String>;
}

// ─── Kernel Configuration ───────────────────────────────────────────────

/// Minimal configuration the kernel needs to operate.
#[derive(Debug, Clone)]
pub struct KernelConfig {
    /// Project root path.
    pub project_root: PathBuf,
    /// Maximum turns per run.
    pub max_turns: u32,
    /// Tool execution timeout (seconds).
    pub tool_timeout_secs: u64,
    /// Maximum concurrent read tools.
    pub max_concurrent_reads: usize,
    /// Whether to enable steering messages.
    pub enable_steering: bool,
}

impl Default for KernelConfig {
    fn default() -> Self {
        Self {
            project_root: PathBuf::from("."),
            max_turns: 100,
            tool_timeout_secs: 300,
            max_concurrent_reads: 8,
            enable_steering: true,
        }
    }
}

// ─── Kernel Trait ───────────────────────────────────────────────────────

/// The minimal kernel interface. All surfaces (CLI, TUI, SDK, daemon)
/// interact with the agent through this trait.
///
/// This is the trusted core — everything inside is security-relevant.
/// Everything outside is an adapter.
#[async_trait::async_trait]
pub trait AgentKernel: Send + Sync {
    /// Submit a user message and run the agent loop.
    async fn submit(
        &mut self,
        message: String,
        cancel: tokio_util::sync::CancellationToken,
    ) -> KernelOutcome;

    /// Get available tool declarations for the LLM.
    fn tool_declarations(&self) -> Vec<ToolDeclaration>;

    /// Evaluate a policy decision for a capability request.
    fn evaluate_policy(
        &mut self,
        tool_name: &str,
        request: &CapabilityRequest,
        lineage: &ExecutionLineage,
    ) -> PolicyDecision;

    /// Schedule a batch of tool calls for execution.
    fn schedule_tools(&self, calls: &[ToolCall]) -> Vec<ExecutionBatch>;

    /// Get current token usage.
    fn token_usage(&self) -> TokenUsageSummary;

    /// Inject a steering message.
    fn inject_steering(&mut self, message: String);

    /// Clear the conversation context.
    fn clear_context(&mut self);
}

/// Outcome of a kernel submission.
#[derive(Debug, Clone)]
pub enum KernelOutcome {
    Completed {
        turns: u32,
        total_tokens: u64,
        cost: f64,
        proof: ProofPacket,
    },
    MaxTurnsReached(u32),
    Cancelled,
    Error(String),
}

/// Token usage summary.
#[derive(Debug, Clone, Default)]
pub struct TokenUsageSummary {
    pub used: u64,
    pub limit: u64,
    pub cost: f64,
}

// ─── No-op Adapter Implementations ─────────────────────────────────────

/// No-op progress port (for headless/testing).
pub struct NullProgressPort;

#[async_trait::async_trait]
impl ProgressPort for NullProgressPort {
    async fn report(&self, _event: AgentEvent) {}
    async fn status(&self, _label: &str) {}
}

/// No-op persistence port (for testing).
pub struct NullPersistencePort;

#[async_trait::async_trait]
impl PersistencePort for NullPersistencePort {
    async fn write_event(&self, _event: &crate::ledger::SessionEvent) -> Result<(), String> {
        Ok(())
    }
    async fn checkpoint(&self) -> Result<String, String> {
        Ok("null-checkpoint".to_string())
    }
    async fn rollback(&self, _id: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Auto-approve port (for FullAuto mode).
pub struct AutoApprovalPort;

#[async_trait::async_trait]
impl ApprovalPort for AutoApprovalPort {
    async fn request_approval(
        &self,
        _call_id: &str,
        _tool_name: &str,
        _args: &serde_json::Value,
        _decision: &PolicyDecision,
    ) -> bool {
        true
    }
}

/// No-op extension port.
pub struct NullExtensionPort;

#[async_trait::async_trait]
impl ExtensionPort for NullExtensionPort {
    async fn on_input(&self, _text: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
    async fn on_before_request(&self, _s: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
    async fn on_content_delta(&self, _text: &str) -> Result<(), String> {
        Ok(())
    }
    async fn on_before_tool(
        &self,
        _n: &str,
        _a: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>, String> {
        Ok(None)
    }
    async fn on_after_tool(&self, _n: &str, _r: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
}

/// No-op VCS port (for non-git projects).
pub struct NullVcsPort;

impl VcsPort for NullVcsPort {
    fn head_ref(&self) -> Option<String> {
        None
    }
    fn create_checkpoint(&self, _msg: &str) -> Result<String, String> {
        Ok("null".to_string())
    }
    fn rollback_to(&self, _r: &str) -> Result<(), String> {
        Ok(())
    }
    fn diff(&self, _files: &[String]) -> Result<String, String> {
        Ok(String::new())
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Kernel Tool Lifecycle Funnel
// ═══════════════════════════════════════════════════════════════════════

/// The result of a single tool call through the kernel funnel.
/// All transitions (policy, schedule, execute, evidence, persist) pass through
/// a single pipeline — no adapter may bypass.
#[derive(Debug, Clone)]
pub struct ToolLifecycleResult {
    pub call_id: String,
    pub tool_name: String,
    pub semantic_class: SemanticClassLabel,
    pub policy_decision: PolicyDecisionLabel,
    pub outcome: ToolOutcome,
    pub evidence: Option<EvidenceArtifact>,
    pub risk: Option<RiskReport>,
    pub mutated_files: Vec<String>,
}

/// Serializable label for the semantic class (avoids lifetime issues).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum SemanticClassLabel {
    Read,
    Search,
    Edit,
    Exec,
    Delegate,
    External,
    Pure,
}

impl From<&SemanticClass> for SemanticClassLabel {
    fn from(sc: &SemanticClass) -> Self {
        match sc {
            SemanticClass::Read { .. } => SemanticClassLabel::Read,
            SemanticClass::Search { .. } => SemanticClassLabel::Search,
            SemanticClass::Edit { .. } => SemanticClassLabel::Edit,
            SemanticClass::Exec { .. } => SemanticClassLabel::Exec,
            SemanticClass::Delegate { .. } => SemanticClassLabel::Delegate,
            SemanticClass::External { .. } => SemanticClassLabel::External,
            SemanticClass::Pure => SemanticClassLabel::Pure,
        }
    }
}

/// Serializable label for the policy decision.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PolicyDecisionLabel {
    Allowed,
    Asked,
    Denied,
    Sandboxed,
}

impl From<&PolicyDecision> for PolicyDecisionLabel {
    fn from(pd: &PolicyDecision) -> Self {
        match pd {
            PolicyDecision::Allow => PolicyDecisionLabel::Allowed,
            PolicyDecision::Ask { .. } => PolicyDecisionLabel::Asked,
            PolicyDecision::Deny { .. } => PolicyDecisionLabel::Denied,
            PolicyDecision::Sandbox { .. } => PolicyDecisionLabel::Sandboxed,
        }
    }
}

/// Outcome of tool execution through the kernel funnel.
#[derive(Debug, Clone)]
pub enum ToolOutcome {
    Success { content: String, mutated: bool },
    PolicyBlocked { reason: String },
    UserDenied,
    Error { message: String },
}

/// The complete result of processing a batch of tool calls through the kernel.
#[derive(Debug, Clone)]
pub struct BatchLifecycleResult {
    pub results: Vec<ToolLifecycleResult>,
    pub total_evidence: Vec<EvidenceArtifact>,
    pub modified_files: Vec<String>,
    pub realized_edits: Vec<RealizedEdit>,
    pub highest_risk: RiskReport,
    pub batches_executed: usize,
}

/// Classify a tool call for the lifecycle funnel.
/// This is the kernel's single entry point for semantic classification.
pub fn classify_for_lifecycle(
    tool_name: &str,
    args: &serde_json::Value,
) -> (SemanticClass, SemanticClassLabel) {
    let sc = classify_semantically(tool_name, args);
    let label = SemanticClassLabel::from(&sc);
    (sc, label)
}

/// Build a capability request from a semantic classification and tool arguments.
/// This is the kernel's single entry point for capability request construction.
pub fn build_capability_request(
    tool_name: &str,
    args: &serde_json::Value,
) -> crate::capability::CapabilityRequest {
    let semantics = crate::tool_semantics::builtin_semantics(tool_name);
    let mut resource_scopes = Vec::new();
    if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
        resource_scopes.push(crate::capability::ResourceScope::Path(PathBuf::from(path)));
    }
    if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
        resource_scopes.push(crate::capability::ResourceScope::Command(cmd.to_string()));
    }
    crate::capability::CapabilityRequest {
        required: semantics.required_capabilities,
        resource_scopes,
        justification: Some(format!("Tool '{}' invocation", tool_name)),
    }
}
