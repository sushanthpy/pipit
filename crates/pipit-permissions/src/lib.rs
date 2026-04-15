//! Pipit Permissions — Deep Permission Engine (Task 1)
//!
//! Architecture: Lattice-based classifier composition.
//!
//!   Allow < Ask < Deny < Escalate
//!
//! Each classifier f_i: ToolCall → Decision. Final decision = ⊔{f_1..f_n}.
//! TOML rules evaluated as priority-ordered list (first match wins).
//! Shadowed-rule detection: R_j is shadowed iff ∃R_i (i<j) s.t. L(R_i) ⊇ L(R_j).
//!
//! Complexity: O(C·R) per decision, C classifiers × R rules.

pub mod classifiers;
pub mod denial_tracker;
pub mod escape_gates;
pub mod production_classifiers;
pub mod rules;
pub mod shadow_detector;

pub use escape_gates::{DangerousWriteSet, WriteCheck};

use classifiers::*;
use denial_tracker::DenialTracker;
use rules::{PermissionRuleSet, RuleSource};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ═══════════════════════════════════════════════════════════════════════════
//  Permission Modes — 5-level approval model
// ═══════════════════════════════════════════════════════════════════════════

/// The 5 permission modes, ordered from most restrictive to least.
///
///   Default < Plan < Auto < Yolo < Bypass
///
/// Each mode determines which classifiers are active and what the default
/// decision is for unclassified tool calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Default: All mutating tools require approval. Read-only tools auto-allowed.
    Default,
    /// Plan: Read-only tools + plan-mode tools auto-allowed. Writes require approval.
    Plan,
    /// Auto: Trusted tools auto-allowed based on rules. Unknown tools ask.
    Auto,
    /// Yolo: Everything auto-allowed except dangerous patterns.
    Yolo,
    /// Bypass: Everything allowed. No classifiers run. FOR TESTING ONLY.
    Bypass,
}

impl Default for PermissionMode {
    fn default() -> Self {
        Self::Default
    }
}

impl std::fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Default => write!(f, "default"),
            Self::Plan => write!(f, "plan"),
            Self::Auto => write!(f, "auto"),
            Self::Yolo => write!(f, "yolo"),
            Self::Bypass => write!(f, "bypass"),
        }
    }
}

impl std::str::FromStr for PermissionMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "default" => Ok(Self::Default),
            "plan" => Ok(Self::Plan),
            "auto" | "auto-edit" => Ok(Self::Auto),
            "yolo" | "full-auto" => Ok(Self::Yolo),
            "bypass" => Ok(Self::Bypass),
            _ => Err(format!("Unknown permission mode: {s}")),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Decision lattice: Allow < Ask < Deny < Escalate
// ═══════════════════════════════════════════════════════════════════════════

/// A single classifier's decision for a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Decision {
    /// Tool is safe to execute without user interaction.
    Allow,
    /// Tool should prompt the user for confirmation.
    Ask,
    /// Tool is denied (user can override with explicit confirmation).
    Deny,
    /// Tool is absolutely forbidden (no override possible).
    Escalate,
}

impl Decision {
    /// Lattice join: returns the higher (more restrictive) decision.
    pub fn join(self, other: Self) -> Self {
        std::cmp::max(self, other)
    }
}

/// Detailed result of permission evaluation.
#[derive(Debug, Clone, Serialize)]
pub struct PermissionResult {
    pub decision: Decision,
    pub mode: PermissionMode,
    /// Which classifiers contributed to this decision (name → individual decision).
    pub classifier_verdicts: HashMap<String, Decision>,
    /// Which rule matched (if any).
    pub matched_rule: Option<String>,
    /// Human-readable explanation.
    pub explanation: String,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool call descriptor — the input to classifiers
// ═══════════════════════════════════════════════════════════════════════════

/// A tool call to be evaluated for permission.
#[derive(Debug, Clone)]
pub struct ToolCallDescriptor {
    pub tool_name: String,
    pub args: serde_json::Value,
    /// Extracted paths from arguments (for path-based classifiers).
    pub paths: Vec<PathBuf>,
    /// Extracted command string (for bash/powershell classifiers).
    pub command: Option<String>,
    /// Whether the tool is classified as mutating by the semantic type system.
    pub is_mutating: bool,
    /// The project root (for path containment checks).
    pub project_root: PathBuf,
}

impl ToolCallDescriptor {
    /// Build a descriptor from raw tool call data.
    pub fn from_tool_call(
        tool_name: &str,
        args: &serde_json::Value,
        is_mutating: bool,
        project_root: &Path,
    ) -> Self {
        let paths = extract_paths(args);
        let command = extract_command(tool_name, args);
        Self {
            tool_name: tool_name.to_string(),
            args: args.clone(),
            paths,
            command,
            is_mutating,
            project_root: project_root.to_path_buf(),
        }
    }
}

fn extract_paths(args: &serde_json::Value) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
        paths.push(PathBuf::from(p));
    }
    if let Some(p) = args.get("file_path").and_then(|v| v.as_str()) {
        paths.push(PathBuf::from(p));
    }
    if let Some(p) = args.get("target").and_then(|v| v.as_str()) {
        paths.push(PathBuf::from(p));
    }
    paths
}

fn extract_command(tool_name: &str, args: &serde_json::Value) -> Option<String> {
    match tool_name {
        "bash" | "powershell" => args
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        _ => None,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Permission Engine — the central evaluator
// ═══════════════════════════════════════════════════════════════════════════

/// The permission engine. Owns classifiers, rules, and denial state.
///
/// Thread-safe: classifiers are stateless, denial tracker uses DashMap.
pub struct PermissionEngine {
    mode: PermissionMode,
    classifiers: Vec<Box<dyn Classifier>>,
    rules: PermissionRuleSet,
    denial_tracker: DenialTracker,
}

impl PermissionEngine {
    /// Create an engine with default classifiers for the given mode.
    pub fn new(mode: PermissionMode) -> Self {
        Self {
            mode,
            classifiers: default_classifiers(),
            rules: PermissionRuleSet::empty(),
            denial_tracker: DenialTracker::new(),
        }
    }

    /// Create with TOML rules loaded from a file or directory.
    pub fn with_rules(mode: PermissionMode, rule_paths: &[PathBuf]) -> Self {
        let rules = PermissionRuleSet::load(rule_paths);
        let shadows = shadow_detector::detect_shadows(&rules);
        if !shadows.is_empty() {
            for shadow in &shadows {
                tracing::warn!(
                    "Shadowed permission rule: rule '{}' at line {} is masked by rule '{}' at line {}",
                    shadow.shadowed_rule,
                    shadow.shadowed_line,
                    shadow.masking_rule,
                    shadow.masking_line,
                );
            }
        }
        Self {
            mode,
            classifiers: default_classifiers(),
            rules,
            denial_tracker: DenialTracker::new(),
        }
    }

    /// Set the permission mode (e.g., from /mode command).
    pub fn set_mode(&mut self, mode: PermissionMode) {
        self.mode = mode;
    }

    pub fn mode(&self) -> PermissionMode {
        self.mode
    }

    /// Evaluate a tool call. Returns the decision and full audit trail.
    ///
    /// Complexity: O(C·R) where C = |classifiers|, R = |rules|.
    pub fn evaluate(&self, descriptor: &ToolCallDescriptor) -> PermissionResult {
        // Bypass mode: allow everything, no classifiers
        if self.mode == PermissionMode::Bypass {
            return PermissionResult {
                decision: Decision::Allow,
                mode: self.mode,
                classifier_verdicts: HashMap::new(),
                matched_rule: Some("bypass-mode".to_string()),
                explanation: "Bypass mode: all tools allowed".to_string(),
            };
        }

        // Check denial tracker first — if this exact call was recently denied
        // and the user hasn't explicitly re-approved, escalate backoff.
        if let Some(backoff_decision) = self.denial_tracker.check(descriptor) {
            return backoff_decision;
        }

        // Phase 1: Check TOML rules (highest priority, user-defined)
        if let Some(rule_result) = self.rules.evaluate(descriptor, self.mode) {
            return rule_result;
        }

        // Phase 2: Run all classifiers and join decisions
        let mut verdicts = HashMap::new();
        let mut final_decision = Decision::Allow;

        for classifier in &self.classifiers {
            if !classifier.active_in_mode(self.mode) {
                continue;
            }
            let verdict = classifier.classify(descriptor);
            final_decision = final_decision.join(verdict);
            verdicts.insert(classifier.name().to_string(), verdict);
        }

        // Phase 3: Mode-based default adjustment
        let adjusted = match self.mode {
            PermissionMode::Default => {
                if descriptor.is_mutating && final_decision == Decision::Allow {
                    Decision::Ask // Default mode: all mutating tools need confirmation
                } else {
                    final_decision
                }
            }
            PermissionMode::Plan => {
                if descriptor.is_mutating && final_decision == Decision::Allow {
                    Decision::Ask
                } else {
                    final_decision
                }
            }
            PermissionMode::Auto => final_decision,
            PermissionMode::Yolo => {
                // Yolo: only respect Deny and Escalate from classifiers
                if final_decision >= Decision::Deny {
                    final_decision
                } else {
                    Decision::Allow
                }
            }
            PermissionMode::Bypass => unreachable!(),
        };

        let explanation = build_explanation(&verdicts, adjusted, self.mode, descriptor);

        PermissionResult {
            decision: adjusted,
            mode: self.mode,
            classifier_verdicts: verdicts,
            matched_rule: None,
            explanation,
        }
    }

    /// Record a user denial for backoff tracking.
    pub fn record_denial(&self, descriptor: &ToolCallDescriptor) {
        self.denial_tracker.record(descriptor);
    }

    /// Record a user approval (resets backoff for this call pattern).
    pub fn record_approval(&self, descriptor: &ToolCallDescriptor) {
        self.denial_tracker.clear(descriptor);
    }
}

fn build_explanation(
    verdicts: &HashMap<String, Decision>,
    final_decision: Decision,
    mode: PermissionMode,
    descriptor: &ToolCallDescriptor,
) -> String {
    let reasons: Vec<String> = verdicts
        .iter()
        .filter(|(_, d)| **d > Decision::Allow)
        .map(|(name, d)| format!("{name}: {d:?}"))
        .collect();

    if reasons.is_empty() {
        format!("Tool '{}' allowed in {} mode", descriptor.tool_name, mode)
    } else {
        format!(
            "Tool '{}' {:?} in {} mode. Classifiers: {}",
            descriptor.tool_name,
            final_decision,
            mode,
            reasons.join(", ")
        )
    }
}

fn default_classifiers() -> Vec<Box<dyn Classifier>> {
    vec![
        Box::new(ReadOnlyClassifier),
        Box::new(DangerousCommandClassifier),
        Box::new(PathEscapeClassifier),
        Box::new(SedMutationClassifier),
        Box::new(NetworkExposureClassifier),
        Box::new(GitDestructiveClassifier),
        Box::new(PrivilegeEscalationClassifier),
        Box::new(EnvironmentMutationClassifier),
        Box::new(RecursiveDeleteClassifier),
        Box::new(PipeToShellClassifier),
        Box::new(SensitiveFileClassifier),
        Box::new(LargeWriteClassifier),
    ]
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn test_descriptor(tool: &str, args: serde_json::Value, mutating: bool) -> ToolCallDescriptor {
        ToolCallDescriptor::from_tool_call(tool, &args, mutating, Path::new("/tmp/project"))
    }

    #[test]
    fn bypass_mode_allows_everything() {
        let engine = PermissionEngine::new(PermissionMode::Bypass);
        let desc = test_descriptor("bash", serde_json::json!({"command": "rm -rf /"}), true);
        let result = engine.evaluate(&desc);
        assert_eq!(result.decision, Decision::Allow);
    }

    #[test]
    fn default_mode_asks_for_mutating() {
        let engine = PermissionEngine::new(PermissionMode::Default);
        let desc = test_descriptor("write_file", serde_json::json!({"path": "test.txt"}), true);
        let result = engine.evaluate(&desc);
        assert!(result.decision >= Decision::Ask);
    }

    #[test]
    fn default_mode_allows_readonly() {
        let engine = PermissionEngine::new(PermissionMode::Default);
        let desc = test_descriptor("read_file", serde_json::json!({"path": "test.txt"}), false);
        let result = engine.evaluate(&desc);
        assert_eq!(result.decision, Decision::Allow);
    }

    #[test]
    fn dangerous_command_denied_in_yolo() {
        let engine = PermissionEngine::new(PermissionMode::Yolo);
        let desc = test_descriptor("bash", serde_json::json!({"command": "rm -rf /"}), true);
        let result = engine.evaluate(&desc);
        assert!(result.decision >= Decision::Deny);
    }

    #[test]
    fn path_escape_detected() {
        let engine = PermissionEngine::new(PermissionMode::Auto);
        let desc = test_descriptor(
            "write_file",
            serde_json::json!({"path": "../../../etc/passwd"}),
            true,
        );
        let result = engine.evaluate(&desc);
        assert!(result.decision >= Decision::Deny);
    }

    #[test]
    fn sed_inline_edit_classified() {
        let engine = PermissionEngine::new(PermissionMode::Default);
        let desc = test_descriptor(
            "bash",
            serde_json::json!({"command": "sed -i 's/foo/bar/g' important.conf"}),
            true,
        );
        let result = engine.evaluate(&desc);
        assert!(result.decision >= Decision::Ask);
    }

    #[test]
    fn decision_lattice_join() {
        assert_eq!(Decision::Allow.join(Decision::Ask), Decision::Ask);
        assert_eq!(Decision::Ask.join(Decision::Deny), Decision::Deny);
        assert_eq!(Decision::Deny.join(Decision::Escalate), Decision::Escalate);
        assert_eq!(Decision::Allow.join(Decision::Allow), Decision::Allow);
    }
}
