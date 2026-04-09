// ─────────────────────────────────────────────────────────────────────────────
//  pev.rs — Plan / Execute / Verify orchestration mode
// ─────────────────────────────────────────────────────────────────────────────
//
//  PEV is a typed phase machine that routes inference through role-specific
//  model configurations:
//
//    Planner  → produces PlanSpec (structured JSON)
//    Executor → drives tools to realize the plan
//    Verifier → produces VerificationReport (structured JSON)
//
//  Phase transitions:
//
//    Idle → Plan → Execute → Verify → Complete
//                                   → Repair → Execute → Verify → ...
//                                   → Replan → Plan → Execute → ...
//                                   → Escalate
//
//  The orchestrator owns truth. Models do not.
//
// ─────────────────────────────────────────────────────────────────────────────

use pipit_provider::LlmProvider;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ═══════════════════════════════════════════════════════════════════════════
//  Agent mode — the public UX surface
// ═══════════════════════════════════════════════════════════════════════════

/// User-facing agent mode. Controls orchestration policy.
///
/// The architecture is multipart, but the UX is monolithic:
/// - `fast` — single model, no verification overhead
/// - `balanced` — plans before acting, heuristic verification
/// - `guarded` — full PEV: structured plan, execute, LLM-based verify
/// - `custom` — guarded + user-specified role models
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentMode {
    /// Direct execution, no orchestration overhead.
    /// Best for simple tasks, Q&A, small edits.
    Fast,
    /// Plans before acting, heuristic verification after mutations.
    /// Good default for medium-complexity work.
    Balanced,
    /// Full PEV: structured plan → execute → LLM verify → repair loop.
    /// Higher confidence, higher cost, slower. For risky changes.
    Guarded,
    /// Like Guarded, but with user-specified role models.
    Custom,
}

impl AgentMode {
    /// Convert to PEV config. Returns None for Fast (no orchestration).
    pub fn to_pev_config(&self) -> Option<PevConfig> {
        match self {
            AgentMode::Fast => None,
            AgentMode::Balanced => Some(PevConfig {
                max_repairs: 1,
                allow_replan: false,
                require_verifier_pass: false,
                verify_only_on_mutation: true,
                executor_max_turns: 50,
            }),
            AgentMode::Guarded | AgentMode::Custom => Some(PevConfig {
                max_repairs: 2,
                allow_replan: true,
                require_verifier_pass: true,
                verify_only_on_mutation: false,
                executor_max_turns: 30,
            }),
        }
    }

    /// Human-readable description for status display.
    pub fn description(&self) -> &'static str {
        match self {
            AgentMode::Fast => "direct execution, no verification",
            AgentMode::Balanced => "plans before acting, heuristic verification",
            AgentMode::Guarded => "full plan/execute/verify with repair loops",
            AgentMode::Custom => "guarded mode with custom role models",
        }
    }
}

impl std::fmt::Display for AgentMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentMode::Fast => write!(f, "fast"),
            AgentMode::Balanced => write!(f, "balanced"),
            AgentMode::Guarded => write!(f, "guarded"),
            AgentMode::Custom => write!(f, "custom"),
        }
    }
}

impl std::str::FromStr for AgentMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "fast" | "f" => Ok(AgentMode::Fast),
            "balanced" | "b" | "default" => Ok(AgentMode::Balanced),
            "guarded" | "g" | "strict" => Ok(AgentMode::Guarded),
            "custom" | "c" => Ok(AgentMode::Custom),
            // Context mode aliases (Claude Code / ECC compatibility):
            //   dev      → balanced (plan + execute, standard dev workflow)
            //   review   → guarded  (thorough, verify-heavy, read-first)
            //   research → fast     (exploration, Q&A, no mutation overhead)
            "dev" | "development" => Ok(AgentMode::Balanced),
            "review" | "code-review" | "pr" => Ok(AgentMode::Guarded),
            "research" | "explore" | "read" => Ok(AgentMode::Fast),
            _ => Err(format!(
                "Unknown mode: '{}'. Available: fast, balanced, guarded, custom, dev, review, research",
                s
            )),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Model routing
// ═══════════════════════════════════════════════════════════════════════════

/// Which cognitive role a model is serving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelRole {
    /// Sparse, high-leverage: decomposes objectives into structured plans.
    Planner,
    /// Dense, tool-heavy: executes plans via tool calls and patch generation.
    Executor,
    /// Adversarial: reviews diffs and evidence, produces machine-readable verdicts.
    Verifier,
}

impl std::fmt::Display for ModelRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelRole::Planner => write!(f, "planner"),
            ModelRole::Executor => write!(f, "executor"),
            ModelRole::Verifier => write!(f, "verifier"),
        }
    }
}

/// A provider + model pair bound to a specific role.
#[derive(Clone)]
pub struct RoleProvider {
    pub provider: Arc<dyn LlmProvider>,
    pub model_id: String,
    pub role: ModelRole,
}

impl std::fmt::Debug for RoleProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoleProvider")
            .field("model_id", &self.model_id)
            .field("role", &self.role)
            .finish()
    }
}

/// Routes inference requests to role-specific model configurations.
///
/// When PEV mode is active, each phase of the orchestration uses a different
/// provider+model combination. When PEV is not active, all roles fall back
/// to the single executor provider.
#[derive(Clone)]
pub struct ModelRouter {
    planner: RoleProvider,
    executor: RoleProvider,
    verifier: RoleProvider,
}

impl ModelRouter {
    /// Create a router with explicit role assignments.
    pub fn new(planner: RoleProvider, executor: RoleProvider, verifier: RoleProvider) -> Self {
        Self {
            planner,
            executor,
            verifier,
        }
    }

    /// Create a router where all roles use the same provider (non-PEV mode).
    pub fn single(provider: Arc<dyn LlmProvider>, model_id: String) -> Self {
        let role_provider = |role: ModelRole| RoleProvider {
            provider: provider.clone(),
            model_id: model_id.clone(),
            role,
        };
        Self {
            planner: role_provider(ModelRole::Planner),
            executor: role_provider(ModelRole::Executor),
            verifier: role_provider(ModelRole::Verifier),
        }
    }

    /// Get the provider for a specific role.
    pub fn for_role(&self, role: ModelRole) -> &RoleProvider {
        match role {
            ModelRole::Planner => &self.planner,
            ModelRole::Executor => &self.executor,
            ModelRole::Verifier => &self.verifier,
        }
    }

    /// Whether this router has distinct models for different roles.
    pub fn is_multi_model(&self) -> bool {
        self.planner.model_id != self.executor.model_id
            || self.executor.model_id != self.verifier.model_id
    }
}

impl std::fmt::Debug for ModelRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelRouter")
            .field("planner", &self.planner.model_id)
            .field("executor", &self.executor.model_id)
            .field("verifier", &self.verifier.model_id)
            .finish()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  PEV typed artifacts — the inter-phase coordination protocol
// ═══════════════════════════════════════════════════════════════════════════

/// Structured plan produced by the Planner phase.
/// This is the contract between Planner and Executor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanSpec {
    /// The original user objective, as understood by the planner.
    pub objective: String,
    /// High-level strategy description.
    pub strategy: String,
    /// Why this strategy was chosen over alternatives.
    pub rationale: String,
    /// Files the executor should read for context.
    pub files_to_read: Vec<String>,
    /// Files the executor is expected to modify or create.
    pub files_to_modify: Vec<String>,
    /// Conditions that must hold before AND after execution.
    pub invariants: Vec<String>,
    /// Known risks and how to mitigate them.
    pub risks: Vec<String>,
    /// Steps the verifier should check after execution.
    pub verification_steps: Vec<String>,
    /// Conditions under which the executor should stop early.
    pub stop_conditions: Vec<String>,
}

impl Default for PlanSpec {
    fn default() -> Self {
        Self {
            objective: String::new(),
            strategy: String::new(),
            rationale: String::new(),
            files_to_read: Vec::new(),
            files_to_modify: Vec::new(),
            invariants: Vec::new(),
            risks: Vec::new(),
            verification_steps: Vec::new(),
            stop_conditions: Vec::new(),
        }
    }
}

/// Brief handed to the Executor phase.
/// Contains the plan plus execution constraints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionBrief {
    pub plan: PlanSpec,
    pub max_turns: u32,
    pub allowed_tools: Vec<String>,
}

/// Result produced by the Executor phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    /// Files that were actually modified.
    pub modified_files: Vec<String>,
    /// Summary of each edit applied.
    pub realized_edits: Vec<EditSummary>,
    /// Commands that were run and their outcomes.
    pub command_outputs: Vec<CommandOutput>,
    /// Combined diff across all modified files.
    pub diff_summary: String,
    /// Number of turns the executor used.
    pub turns_used: u32,
    /// Whether the executor believes it completed the task.
    pub self_reported_complete: bool,
}

/// A single file edit summary for the verifier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditSummary {
    pub path: String,
    pub description: String,
    pub lines_added: u32,
    pub lines_removed: u32,
}

/// A command execution result for the verifier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandOutput {
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub success: bool,
}

/// Machine-readable verdict from the Verifier phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationReport {
    /// The verifier's decision.
    pub verdict: Verdict,
    /// Confidence in the verdict (0.0 - 1.0).
    pub confidence: f32,
    /// Specific findings, each mapped to an objective/invariant/test.
    pub findings: Vec<Finding>,
    /// Evidence the verifier wanted but could not find.
    pub missing_evidence: Vec<String>,
    /// Specific repair instructions if verdict is Repairable.
    pub suggested_repairs: Vec<String>,
    /// Whether a full replan is needed vs a local repair.
    pub needs_replan: bool,
}

/// The verifier's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// All objectives met, invariants hold, tests pass.
    Pass,
    /// Issues found but fixable without replanning.
    Repairable,
    /// Fundamental failure — wrong approach, need to replan.
    Fail,
    /// Cannot determine correctness — need more evidence.
    Inconclusive,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Verdict::Pass => write!(f, "PASS"),
            Verdict::Repairable => write!(f, "REPAIRABLE"),
            Verdict::Fail => write!(f, "FAIL"),
            Verdict::Inconclusive => write!(f, "INCONCLUSIVE"),
        }
    }
}

/// A specific finding from the verifier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    /// What was checked.
    pub check: String,
    /// Whether it passed.
    pub passed: bool,
    /// What objective, invariant, or test this maps to.
    pub relates_to: String,
    /// Details about the finding.
    pub detail: String,
    /// Severity if it failed.
    pub severity: FindingSeverity,
}

/// Severity of a verifier finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FindingSeverity {
    /// Blocking — must fix before completion.
    Critical,
    /// Should fix, but not necessarily a blocker.
    Warning,
    /// Informational observation.
    Info,
}

/// A repair directive sent back to the executor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairDirective {
    /// What specifically needs to be fixed.
    pub instructions: Vec<String>,
    /// Files to focus on.
    pub target_files: Vec<String>,
    /// What the verifier found wrong.
    pub findings: Vec<Finding>,
    /// How many repair attempts have been made so far.
    pub attempt_number: u32,
}

// ═══════════════════════════════════════════════════════════════════════════
//  PEV phase machine
// ═══════════════════════════════════════════════════════════════════════════

/// Current phase of the PEV state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PevPhase {
    Idle,
    Planning,
    Executing,
    Verifying,
    Repairing,
    Complete,
    Escalated,
}

impl std::fmt::Display for PevPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PevPhase::Idle => write!(f, "idle"),
            PevPhase::Planning => write!(f, "planning"),
            PevPhase::Executing => write!(f, "executing"),
            PevPhase::Verifying => write!(f, "verifying"),
            PevPhase::Repairing => write!(f, "repairing"),
            PevPhase::Complete => write!(f, "complete"),
            PevPhase::Escalated => write!(f, "escalated"),
        }
    }
}

/// Configuration for PEV orchestration mode.
/// Derived from AgentMode policy — users set the mode, not these fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PevConfig {
    /// Maximum number of repair loops before escalating.
    pub max_repairs: u32,
    /// Whether the verifier can trigger a full replan.
    pub allow_replan: bool,
    /// Whether verifier must pass for the task to complete.
    pub require_verifier_pass: bool,
    /// Only run verifier after file mutations (not on read-only turns).
    pub verify_only_on_mutation: bool,
    /// Maximum turns the executor gets per phase.
    pub executor_max_turns: u32,
}

impl Default for PevConfig {
    fn default() -> Self {
        Self {
            max_repairs: 2,
            allow_replan: true,
            require_verifier_pass: true,
            verify_only_on_mutation: false,
            executor_max_turns: 30,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Prompt templates for each role
// ═══════════════════════════════════════════════════════════════════════════

/// Build the system prompt for the Planner role.
pub fn planner_system_prompt(repo_summary: &str, repo_map: Option<&str>) -> String {
    let repo_map_section = match repo_map {
        Some(map) if !map.is_empty() => format!(
            "\n## Repository File Map\nUse these REAL paths in files_to_read and files_to_modify:\n{}\n",
            map
        ),
        _ => String::new(),
    };
    format!(
        r#"You are a planning agent. Your job is to analyze a task and produce a structured execution plan.

## Repository Context
{repo_summary}{repo_map_section}

## Output Format
You MUST respond with ONLY a JSON object matching this exact schema:
```json
{{
  "objective": "what the user wants achieved",
  "strategy": "MinimalPatch | RootCauseRepair | CharacterizationFirst | ArchitecturalRepair | DiagnosticOnly | Greenfield",
  "rationale": "why this approach over alternatives",
  "files_to_read": ["paths to read for context"],
  "files_to_modify": ["paths expected to change"],
  "invariants": ["conditions that must hold before and after"],
  "risks": ["known risks and mitigations"],
  "verification_steps": ["how to verify correctness after execution"],
  "stop_conditions": ["when to stop early"]
}}
```

## Strategy Definitions
- **MinimalPatch**: Surgical, focused change — fewest files, smallest diff.
- **RootCauseRepair**: Investigate the underlying bug, then fix it.
- **CharacterizationFirst**: Write tests to capture current behavior, then change code.
- **ArchitecturalRepair**: Refactor/restructure that touches multiple modules.
- **DiagnosticOnly**: Read-only investigation — gather information, report findings.
- **Greenfield**: Create new files/modules from scratch.

Choose EXACTLY ONE of the six strategy names above. Do not paraphrase.

## Rules
- Do NOT execute any tools. You are a planner only.
- Be specific about file paths. Use actual paths from the repository.
- Invariants must be testable assertions, not vague goals.
- Verification steps should be concrete commands or checks.
- If the task is unclear, state assumptions in the rationale."#
    )
}

/// Build the system prompt for the Executor role.
pub fn executor_system_prompt(plan: &PlanSpec) -> String {
    let plan_json = serde_json::to_string_pretty(plan).unwrap_or_else(|_| format!("{:?}", plan));
    format!(
        r#"You are an execution agent. You must implement the following plan using the available tools.

## Execution Plan
{plan_json}

## Rules
- Follow the plan's strategy. Do not deviate without good reason.
- Read the files listed in `files_to_read` before making changes.
- Only modify files listed in `files_to_modify` unless the plan is incomplete.
- Minimize edits. Prefer surgical changes over rewrites.
- After making changes, verify each invariant if possible.
- If you hit a stop condition, stop immediately and report status.
- Produce evidence for each verification step (run commands, read outputs).
- Do not explain at length. Act, verify, report."#
    )
}

/// Build the system prompt for the Verifier role.
pub fn verifier_system_prompt(plan: &PlanSpec) -> String {
    let verification_steps = plan
        .verification_steps
        .iter()
        .enumerate()
        .map(|(i, s)| format!("  {}. {}", i + 1, s))
        .collect::<Vec<_>>()
        .join("\n");
    let invariants = plan
        .invariants
        .iter()
        .enumerate()
        .map(|(i, s)| format!("  {}. {}", i + 1, s))
        .collect::<Vec<_>>()
        .join("\n");
    let files_to_modify = if plan.files_to_modify.is_empty() {
        "  (none specified)".to_string()
    } else {
        plan.files_to_modify
            .iter()
            .map(|f| format!("  - {}", f))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let risks = if plan.risks.is_empty() {
        "  (none identified)".to_string()
    } else {
        plan.risks
            .iter()
            .enumerate()
            .map(|(i, r)| format!("  {}. {}", i + 1, r))
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        r#"You are a verification agent. Your job is to review execution results and produce a machine-readable verdict.

## Original Plan Objective
{objective}

## Strategy
{strategy}

## Rationale
{rationale}

## Expected Files to Modify
{files_to_modify}

## Known Risks
{risks}

## Invariants (must all hold)
{invariants}

## Verification Steps (check each)
{verification_steps}

## Output Format
You MUST respond with ONLY a JSON object matching this exact schema:
```json
{{
  "verdict": "Pass" | "Repairable" | "Fail" | "Inconclusive",
  "confidence": 0.0-1.0,
  "findings": [
    {{
      "check": "what was checked",
      "passed": true/false,
      "relates_to": "which objective/invariant/step",
      "detail": "specifics",
      "severity": "Critical" | "Warning" | "Info"
    }}
  ],
  "missing_evidence": ["evidence wanted but not available"],
  "suggested_repairs": ["specific fix instructions if Repairable"],
  "needs_replan": false
}}
```

## Rules
- Assume the executor may be wrong. Be skeptical.
- Map every finding to a specific objective, invariant, or verification step.
- Do NOT suggest stylistic improvements — only check correctness.
- "Repairable" means the approach is sound but execution has fixable bugs.
- "Fail" means the approach is wrong and needs replanning.
- "Inconclusive" means you need more evidence to decide.
- Cross-check actual modified files against the expected list above.
- If strategy-specific risks materialized, note them in findings."#,
        objective = plan.objective,
        strategy = plan.strategy,
        rationale = plan.rationale
    )
}

/// Build the user prompt for the Verifier with execution evidence.
pub fn verifier_evidence_prompt(exec_result: &ExecutionResult, diff: &str) -> String {
    let edits = exec_result
        .realized_edits
        .iter()
        .map(|e| {
            format!(
                "  - {}: {} (+{} -{} lines)",
                e.path, e.description, e.lines_added, e.lines_removed
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let commands = exec_result
        .command_outputs
        .iter()
        .map(|c| {
            format!(
                "  $ {} → exit {}\n    stdout: {}\n    stderr: {}",
                c.command,
                c.exit_code,
                truncate_str(&c.stdout, 200),
                truncate_str(&c.stderr, 200)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"## Execution Results

### Files Modified
{edits}

### Commands Run
{commands}

### Diff
```diff
{diff}
```

### Executor Self-Assessment
Completed: {complete}

Review the above evidence and produce your VerificationReport JSON."#,
        complete = exec_result.self_reported_complete
    )
}

/// Build a repair prompt for the executor based on verifier findings.
pub fn repair_prompt(directive: &RepairDirective) -> String {
    let instructions = directive
        .instructions
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{}. {}", i + 1, s))
        .collect::<Vec<_>>()
        .join("\n");
    let findings = directive
        .findings
        .iter()
        .filter(|f| !f.passed)
        .map(|f| format!("  - [{}] {}: {}", f.severity_str(), f.check, f.detail))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"## Repair Required (attempt {attempt})

The verifier found issues with the previous execution. Fix them.

### Failed Checks
{findings}

### Repair Instructions
{instructions}

### Files to Focus On
{files}

Fix the issues and verify your changes."#,
        attempt = directive.attempt_number,
        files = directive.target_files.join(", ")
    )
}

impl Finding {
    fn severity_str(&self) -> &str {
        match self.severity {
            FindingSeverity::Critical => "CRITICAL",
            FindingSeverity::Warning => "WARNING",
            FindingSeverity::Info => "INFO",
        }
    }
}

fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        // Find a valid char boundary at or before max
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}
