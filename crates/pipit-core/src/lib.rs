pub mod adaptive_context;
pub mod agent;
pub mod adversarial;
pub mod blob_store;
pub mod bridge_protocol;
pub mod capability;
pub mod command_registry;
pub mod continuation;
pub mod delegation;
pub mod dx_surface;
pub mod events;
pub mod governor;
pub mod hydration;
pub mod integration_ports;
pub mod kernel;
pub mod ledger;
pub mod lineage;
pub mod loop_detector;
pub mod permission_ledger;
pub mod pev;
pub mod phase_timeout;
pub mod plan_gate;
pub mod planner;
pub mod planner_llm;
pub mod plugin_registry;
pub mod policy_store;
pub mod profiler;
pub mod proof;
pub mod query_profiler;
pub mod reactive;
pub mod replay;
pub mod replication;
pub mod scheduler;
pub mod scheduler_boundary;
pub mod sdd_pipeline;
pub mod sdk;
pub mod sdk_compat;
pub mod session_kernel;
pub mod skill_activation;
pub mod skill_budget;
pub mod skill_kernel;
pub mod skill_runtime;
pub mod skill_signing;
pub mod streaming_executor;
pub mod telemetry;
pub mod telemetry_facade;
pub mod tool_semantics;
pub mod tool_interrupt;
pub mod tool_summary;
pub mod turn_kernel;
pub mod two_phase;
pub mod unified_timeline;
pub mod verifier;
pub mod verifier_llm;
pub mod verification_surface;
pub mod worktree;
pub mod worktree_session;
pub mod cost_oracle;
pub mod speculative;
pub mod scoped_capability;
pub mod plan_ir;
pub mod deliberation;
pub mod triage;
pub mod service_graph;

pub use agent::{AgentLoop, AgentLoopConfig, PlanningState};
pub use events::{
    AgentEvent, TurnEndReason, ToolCallOutcome, AgentOutcome,
    ApprovalDecision, ApprovalHandler, AutoApproveHandler, DenyAllApprovalHandler,
};
pub use governor::{ActionClass, Governor, RiskReport};
pub use loop_detector::LoopDetector;
pub use pev::{
    AgentMode, ModelRole, ModelRouter, RoleProvider, PevConfig, PevPhase,
    PlanSpec, ExecutionBrief, ExecutionResult, VerificationReport, Verdict,
    Finding, FindingSeverity, RepairDirective, EditSummary, CommandOutput,
};
pub use planner::{
    CandidatePlan, NullPlanner, PlanSource, PlanStrategy, Planner, StrategyKind,
    VerificationSource, VerifyStrategy, is_question_task,
    AdaptiveScorer, generate_remediation_plan,
};
pub use planner_llm::LlmPlanner;
pub use proof::{
	Assumption, ChangeClaim, ConfidenceReport, EvidenceArtifact, ImplementationTier,
	Objective, ProofPacket, PlanPivot, PolicyStage, RealizedEdit, RollbackCheckpoint,
	SuccessCriterion, VerificationKind, VerificationStep,
};
pub use verifier::{NullVerifier, Verifier};
pub use verifier_llm::LlmVerifier;
pub use worktree::WorktreeManager;
