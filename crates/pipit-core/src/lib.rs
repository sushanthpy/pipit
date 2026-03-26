pub mod agent;
pub mod events;
pub mod governor;
pub mod loop_detector;
pub mod planner;
pub mod proof;
pub mod verifier;
pub mod worktree;

pub use agent::{AgentLoop, AgentLoopConfig, PlanningState};
pub use events::{
    AgentEvent, TurnEndReason, ToolCallOutcome, AgentOutcome,
    ApprovalDecision, ApprovalHandler, AutoApproveHandler, DenyAllApprovalHandler,
};
pub use governor::{ActionClass, Governor, RiskReport};
pub use loop_detector::LoopDetector;
pub use planner::{CandidatePlan, Planner, StrategyKind};
pub use proof::{
	Assumption, ChangeClaim, ConfidenceReport, EvidenceArtifact, Objective, ProofPacket,
	PlanPivot, PolicyStage, RealizedEdit, RollbackCheckpoint, SuccessCriterion, VerificationStep,
};
pub use verifier::Verifier;
pub use worktree::WorktreeManager;
