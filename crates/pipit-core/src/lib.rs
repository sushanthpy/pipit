pub mod adaptive_budget;
pub mod adaptive_context;
pub mod adversarial;
pub mod agent;
pub mod blob_store;
pub mod bridge_protocol;
pub mod capability;
pub mod command_registry;
pub mod continuation;
pub mod cost_oracle;
pub mod delegation;
pub mod deliberation;
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
pub mod plan_ir;
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
pub mod scoped_capability;
pub mod sdd_pipeline;
pub mod sdk;
pub mod sdk_compat;
pub mod service_graph;
pub mod session_kernel;
pub mod skill_activation;
pub mod skill_budget;
pub mod skill_kernel;
pub mod skill_runtime;
pub mod skill_signing;
pub mod speculative;
pub mod telemetry;
pub mod telemetry_facade;
pub mod tool_interrupt;
pub mod tool_semantics;
pub mod tool_summary;
pub mod triage;
pub mod turn_kernel;
pub mod two_phase;
pub mod unified_timeline;
pub mod verification_surface;
pub mod verifier;
pub mod verifier_llm;
pub mod worktree;
pub mod worktree_session;

pub use agent::{AgentLoop, AgentLoopConfig, PlanningState, ProofState};
pub use events::{
    AgentEvent, AgentOutcome, ApprovalDecision, ApprovalHandler, AutoApproveHandler,
    DenyAllApprovalHandler, ToolCallOutcome, TurnEndReason,
};
pub use governor::{ActionClass, Governor, RiskReport};
pub use loop_detector::LoopDetector;
pub use pev::{
    AgentMode, CommandOutput, EditSummary, ExecutionBrief, ExecutionResult, Finding,
    FindingSeverity, ModelRole, ModelRouter, PevConfig, PevPhase, PlanSpec, RepairDirective,
    RoleProvider, Verdict, VerificationReport,
};
pub use planner::{
    AdaptiveScorer, CandidatePlan, NullPlanner, PlanSource, PlanStrategy, Planner, StrategyKind,
    VerificationSource, VerifyStrategy, generate_remediation_plan, is_question_task,
};
pub use planner_llm::LlmPlanner;
pub use proof::{
    Assumption, ChangeClaim, ConfidenceReport, EvidenceArtifact, ImplementationTier, Objective,
    PlanPivot, PolicyStage, ProofPacket, RealizedEdit, RollbackCheckpoint, SuccessCriterion,
    VerificationKind, VerificationStep,
};
pub use verifier::{NullVerifier, Verifier};
pub use verifier_llm::LlmVerifier;
pub use worktree::WorktreeManager;

// ── Optional subsystem crates ──
// These dependencies are feature-gated to reduce compile times when not needed.
// Enable the "full" feature to include all subsystems.
#[cfg(feature = "pipit-agents")]
pub use pipit_agents as agents;
#[cfg(feature = "pipit-arch-evolution")]
pub use pipit_arch_evolution as arch_evolution;
#[cfg(feature = "pipit-bridge")]
pub use pipit_bridge as bridge;
#[cfg(feature = "pipit-compliance")]
pub use pipit_compliance as compliance;
#[cfg(feature = "pipit-env")]
pub use pipit_env as env;
#[cfg(feature = "pipit-evolve")]
pub use pipit_evolve as evolve;
#[cfg(feature = "pipit-hw-codesign")]
pub use pipit_hw_codesign as hw_codesign;
#[cfg(feature = "pipit-memory")]
pub use pipit_memory as memory;
#[cfg(feature = "pipit-perf")]
pub use pipit_perf as perf;
#[cfg(feature = "pipit-permissions")]
pub use pipit_permissions as permissions;
#[cfg(feature = "pipit-spec")]
pub use pipit_spec as spec;
#[cfg(feature = "pipit-verify")]
pub use pipit_verify as verify;
