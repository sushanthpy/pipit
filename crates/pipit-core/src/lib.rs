pub mod agent;
pub mod adversarial;
pub mod delegation;
pub mod events;
pub mod governor;
pub mod loop_detector;
pub mod pev;
pub mod planner;
pub mod planner_llm;
pub mod proof;
pub mod sdd_pipeline;
pub mod verifier;
pub mod verifier_llm;
pub mod worktree;

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
