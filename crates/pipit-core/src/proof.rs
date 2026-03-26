use crate::planner::CandidatePlan;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Objective {
    pub statement: String,
    pub success_criteria: Vec<SuccessCriterion>,
    pub constraints: Vec<String>,
}

impl Objective {
    pub fn from_prompt(prompt: &str) -> Self {
        Self {
            statement: prompt.trim().to_string(),
            success_criteria: vec![SuccessCriterion {
                description: "Produce a codebase state that better satisfies the stated objective and verify it with available evidence.".to_string(),
            }],
            constraints: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuccessCriterion {
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeClaim {
    pub objective: Objective,
    pub hypothesis: String,
    pub expected_effects: Vec<String>,
    pub assumptions: Vec<Assumption>,
    pub verification_plan: Vec<VerificationStep>,
    pub confidence: ConfidenceReport,
}

impl ChangeClaim {
    pub fn from_objective(objective: Objective) -> Self {
        Self {
            hypothesis: format!(
                "If Pipit applies changes guided by the objective, the repository should move to a more correct and validated state: {}",
                objective.statement
            ),
            expected_effects: vec![
                "Requested behavior or output should be closer to the objective.".to_string(),
                "Evidence should exist for any claimed improvement.".to_string(),
            ],
            assumptions: vec![Assumption {
                description: "The provided objective is sufficient to drive the next valid state transition.".to_string(),
                verified: false,
            }],
            verification_plan: vec![
                VerificationStep {
                    description: "Read relevant code and documentation before mutating.".to_string(),
                },
                VerificationStep {
                    description: "Run deterministic checks or commands when available.".to_string(),
                },
            ],
            confidence: ConfidenceReport::default(),
            objective,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assumption {
    pub description: String,
    pub verified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationStep {
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConfidenceReport {
    pub root_cause: f32,
    pub semantic_understanding: f32,
    pub side_effect_risk: f32,
    pub verification_strength: f32,
    pub environment_certainty: f32,
}

impl ConfidenceReport {
    pub fn overall(&self) -> f32 {
        let total = self.root_cause
            + self.semantic_understanding
            + self.side_effect_risk
            + self.verification_strength
            + self.environment_certainty;
        total / 5.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EvidenceArtifact {
    FileRead {
        path: Option<String>,
        summary: String,
    },
    ToolExecution {
        tool_name: String,
        summary: String,
        success: bool,
    },
    CommandResult {
        kind: VerificationKind,
        command: String,
        output: String,
        success: bool,
    },
    EditApplied {
        path: Option<String>,
        summary: String,
    },
    ApprovalBlocked {
        tool_name: String,
        reason: String,
    },
    PolicyViolation {
        tool_name: String,
        stage: PolicyStage,
        summary: String,
        mutation_applied: bool,
        path: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PolicyStage {
    PreToolUse,
    PostToolUse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VerificationKind {
    Test,
    Build,
    Benchmark,
    RuntimeCheck,
    Shell,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealizedEdit {
    pub path: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackCheckpoint {
    pub checkpoint_id: Option<String>,
    pub strategy: String,
    pub reversible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanPivot {
    pub turn_number: u32,
    pub from: CandidatePlan,
    pub to: CandidatePlan,
    pub trigger: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofPacket {
    pub objective: Objective,
    pub selected_plan: CandidatePlan,
    pub candidate_plans: Vec<CandidatePlan>,
    pub plan_pivots: Vec<PlanPivot>,
    pub claim: ChangeClaim,
    pub evidence: Vec<EvidenceArtifact>,
    pub realized_edits: Vec<RealizedEdit>,
    pub unresolved_assumptions: Vec<Assumption>,
    pub risk: crate::governor::RiskReport,
    pub confidence: ConfidenceReport,
    pub rollback_checkpoint: RollbackCheckpoint,
}

impl ChangeClaim {
    pub fn render_for_prompt(&self) -> String {
        let expected_effects = self
            .expected_effects
            .iter()
            .map(|effect| format!("- {}", effect))
            .collect::<Vec<_>>()
            .join("\n");
        let verification_plan = self
            .verification_plan
            .iter()
            .map(|step| format!("- {}", step.description))
            .collect::<Vec<_>>()
            .join("\n");
        let assumptions = self
            .assumptions
            .iter()
            .map(|assumption| format!("- {}", assumption.description))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            "## Active Change Claim\nHypothesis: {}\n\nExpected Effects:\n{}\n\nVerification Plan:\n{}\n\nAssumptions:\n{}\n",
            self.hypothesis,
            expected_effects,
            verification_plan,
            assumptions,
        )
    }

    pub fn align_with_plan(&mut self, plan: &CandidatePlan) {
        self.verification_plan = plan.verification_plan.clone();
        self.expected_effects
            .retain(|effect| !effect.starts_with("Selected strategy:"));
        self.expected_effects.push(format!(
            "Selected strategy: {:?} because {}",
            plan.strategy, plan.rationale
        ));
    }
}