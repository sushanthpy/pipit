use crate::planner::CandidatePlan;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Implementation tier for a subsystem — records provenance of confidence values.
///
/// Every reported confidence/score must carry its tier so post-hoc analysis
/// can separate heuristic from LLM-derived measurements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImplementationTier {
    /// Tier 0: Type defined but not yet implemented.
    TypeOnly,
    /// Tier 1: Rule-based heuristic, no LLM involved.
    Heuristic,
    /// Tier 2: Structured JSON output from an LLM role.
    LlmStructured,
    /// Tier 3: Tier 2 + empirically calibrated confidence scores.
    Validated,
}

impl std::fmt::Display for ImplementationTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImplementationTier::TypeOnly => write!(f, "type-only"),
            ImplementationTier::Heuristic => write!(f, "heuristic"),
            ImplementationTier::LlmStructured => write!(f, "llm-structured"),
            ImplementationTier::Validated => write!(f, "validated"),
        }
    }
}

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
                description:
                    "The provided objective is sufficient to drive the next valid state transition."
                        .to_string(),
                verified: false,
            }],
            verification_plan: vec![
                VerificationStep {
                    description: "Read relevant code and documentation before mutating."
                        .to_string(),
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
    /// Task 9: Subagent execution evidence — first-class in the proof chain.
    SubagentExecution {
        /// Lineage branch ID of the child.
        branch_id: String,
        /// Task that was assigned.
        task: String,
        /// The child's verdict/summary.
        verdict: String,
        /// Confidence from the child's work.
        confidence: f32,
        /// Whether the child succeeded.
        success: bool,
        /// Token usage (input + output).
        tokens_used: u64,
        /// Cost in USD.
        cost_usd: f64,
    },
    /// Task 11: Integration verification passed.
    IntegrationVerified {
        /// Number of cross-artifact checks that were run.
        checks_run: usize,
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
    /// Implementation tier for each subsystem that contributed to this proof.
    /// Keys: "planner", "verifier", "governor".
    #[serde(default)]
    pub tiers: HashMap<String, ImplementationTier>,
    /// Requirement-to-artifact traceability matrix.
    /// Tracks which requirements were satisfied by which artifacts.
    #[serde(default)]
    pub requirement_coverage: RequirementCoverage,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Requirement-to-Artifact Traceability
// ═══════════════════════════════════════════════════════════════════════════

/// A single requirement extracted from the user's objective.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Requirement {
    /// Short identifier (e.g., "REQ-01", "entity:bookmarks", "endpoint:GET /bookmarks").
    pub id: String,
    /// Human-readable requirement description.
    pub description: String,
    /// The kind of requirement.
    pub kind: RequirementKind,
    /// Whether this requirement has been satisfied.
    pub status: RequirementStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RequirementKind {
    /// An entity/table that must exist.
    Entity,
    /// An API endpoint that must be implemented.
    Endpoint,
    /// A business rule or validation constraint.
    BusinessRule,
    /// A test that must pass.
    Test,
    /// A file that must be created.
    Artifact,
    /// A non-functional requirement.
    NonFunctional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequirementStatus {
    /// Not yet addressed.
    Pending,
    /// Partially implemented (some artifacts exist).
    Partial,
    /// Fully satisfied with evidence.
    Satisfied,
    /// Explicitly skipped or out of scope.
    Skipped,
}

/// An implementation artifact that satisfies one or more requirements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceableArtifact {
    /// File path or command output that constitutes evidence.
    pub path: String,
    /// What this artifact is (file, table, route, test).
    pub kind: String,
    /// Which requirement IDs this artifact satisfies.
    pub satisfies: Vec<String>,
}

/// Bipartite graph tracking requirement → artifact coverage.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequirementCoverage {
    pub requirements: Vec<Requirement>,
    pub artifacts: Vec<TraceableArtifact>,
}

impl RequirementCoverage {
    /// Compute coverage ratio: satisfied / total requirements.
    pub fn coverage_ratio(&self) -> f32 {
        if self.requirements.is_empty() {
            return 1.0;
        }
        let satisfied = self
            .requirements
            .iter()
            .filter(|r| matches!(r.status, RequirementStatus::Satisfied))
            .count() as f32;
        satisfied / self.requirements.len() as f32
    }

    /// Get unsatisfied requirements.
    pub fn unsatisfied(&self) -> Vec<&Requirement> {
        self.requirements
            .iter()
            .filter(|r| matches!(r.status, RequirementStatus::Pending | RequirementStatus::Partial))
            .collect()
    }

    /// Mark a requirement as satisfied by an artifact.
    pub fn satisfy(&mut self, requirement_id: &str, artifact_path: &str, artifact_kind: &str) {
        // Update requirement status
        if let Some(req) = self.requirements.iter_mut().find(|r| r.id == requirement_id) {
            req.status = RequirementStatus::Satisfied;
        }
        // Add or update artifact
        if let Some(art) = self.artifacts.iter_mut().find(|a| a.path == artifact_path) {
            if !art.satisfies.contains(&requirement_id.to_string()) {
                art.satisfies.push(requirement_id.to_string());
            }
        } else {
            self.artifacts.push(TraceableArtifact {
                path: artifact_path.to_string(),
                kind: artifact_kind.to_string(),
                satisfies: vec![requirement_id.to_string()],
            });
        }
    }

    /// Populate requirements from a domain architecture IR.
    pub fn from_architecture_ir(ir: &crate::domain_architect::ArchitectureIR) -> Self {
        let mut requirements = Vec::new();

        for entity in &ir.entities {
            requirements.push(Requirement {
                id: format!("entity:{}", entity.name),
                description: format!("Entity '{}' must have a corresponding table/model", entity.name),
                kind: RequirementKind::Entity,
                status: RequirementStatus::Pending,
            });
        }

        for iface in &ir.interfaces {
            requirements.push(Requirement {
                id: format!("endpoint:{} {}", iface.method, iface.path),
                description: format!("{} {} — {}", iface.method, iface.path, iface.description),
                kind: RequirementKind::Endpoint,
                status: RequirementStatus::Pending,
            });
        }

        for inv in &ir.invariants {
            requirements.push(Requirement {
                id: format!("invariant:{}", &inv.description[..inv.description.len().min(40)]),
                description: inv.description.clone(),
                kind: RequirementKind::BusinessRule,
                status: RequirementStatus::Pending,
            });
        }

        Self {
            requirements,
            artifacts: Vec::new(),
        }
    }

    /// Render a coverage summary for display.
    pub fn render_summary(&self) -> String {
        if self.requirements.is_empty() {
            return String::new();
        }
        let total = self.requirements.len();
        let satisfied = self.requirements.iter()
            .filter(|r| matches!(r.status, RequirementStatus::Satisfied))
            .count();
        let partial = self.requirements.iter()
            .filter(|r| matches!(r.status, RequirementStatus::Partial))
            .count();
        let pending = self.requirements.iter()
            .filter(|r| matches!(r.status, RequirementStatus::Pending))
            .count();

        let mut out = format!(
            "## Requirement Coverage: {}/{} ({:.0}%)\n",
            satisfied, total, self.coverage_ratio() * 100.0
        );
        if partial > 0 {
            out.push_str(&format!("  Partial: {}\n", partial));
        }
        if pending > 0 {
            out.push_str(&format!("  Pending: {}\n", pending));
            for req in self.unsatisfied() {
                out.push_str(&format!("  - [{}] {}\n", req.id, req.description));
            }
        }
        out
    }
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
            self.hypothesis, expected_effects, verification_plan, assumptions,
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
