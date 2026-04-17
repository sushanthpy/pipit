//! Task #9: Compile rules into plan-gate constraints.
//!
//! Mandates with `forbidden_paths` compile to `PlanConstraint::PathForbidden`.
//! Procedures with `required_sequence` compile to `PlanConstraint::SequenceRequired`.
//! The plan gate evaluates these before any tool call fires.

use crate::registry::RuleRegistry;
use crate::rule::{RuleId, RuleKind};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

/// A constraint compiled from a rule for plan-gate evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlanConstraint {
    /// No plan step may reference a path matching any of these patterns.
    PathForbidden {
        rule_id: RuleId,
        rule_name: String,
        patterns: Vec<String>,
    },
    /// A tool must appear before another tool in the plan.
    SequenceRequired {
        rule_id: RuleId,
        rule_name: String,
        /// Ordered list: `sequence[i]` must appear before `sequence[i+1]`.
        sequence: Vec<String>,
    },
}

impl PlanConstraint {
    /// Human-readable description of this constraint.
    pub fn description(&self) -> String {
        match self {
            Self::PathForbidden {
                rule_name,
                patterns,
                ..
            } => {
                format!(
                    "Rule '{}' forbids paths matching: {}",
                    rule_name,
                    patterns.join(", ")
                )
            }
            Self::SequenceRequired {
                rule_name,
                sequence,
                ..
            } => {
                format!(
                    "Rule '{}' requires tool sequence: {}",
                    rule_name,
                    sequence.join(" → ")
                )
            }
        }
    }
}

/// Result of checking a plan step against compiled constraints.
#[derive(Debug, Clone)]
pub struct ConstraintViolation {
    pub constraint: PlanConstraint,
    pub step_index: usize,
    pub detail: String,
}

/// Compile active rules into plan-gate constraints.
/// O(M) where M = rules with plan constraints.
pub fn compile_constraints(registry: &RuleRegistry) -> Vec<PlanConstraint> {
    let mut constraints = Vec::new();

    for rule in registry.plan_constraining_rules() {
        if !rule.forbidden_paths.is_empty() {
            constraints.push(PlanConstraint::PathForbidden {
                rule_id: rule.id.clone(),
                rule_name: rule.name.clone(),
                patterns: rule.forbidden_paths.clone(),
            });
        }

        if !rule.required_sequence.is_empty()
            && matches!(rule.kind, RuleKind::Procedure)
        {
            constraints.push(PlanConstraint::SequenceRequired {
                rule_id: rule.id.clone(),
                rule_name: rule.name.clone(),
                sequence: rule.required_sequence.clone(),
            });
        }
    }

    constraints
}

/// Check a list of plan step paths against PathForbidden constraints.
/// Returns violations found.
pub fn check_path_constraints(
    constraints: &[PlanConstraint],
    step_paths: &[(usize, &str)], // (step_index, path)
) -> Vec<ConstraintViolation> {
    let mut violations = Vec::new();

    for constraint in constraints {
        if let PlanConstraint::PathForbidden {
            patterns,
            rule_name,
            ..
        } = constraint
        {
            let mut builder = GlobSetBuilder::new();
            for pat in patterns {
                if let Ok(g) = Glob::new(pat) {
                    builder.add(g);
                }
            }
            if let Ok(globset) = builder.build() {
                for (idx, path) in step_paths {
                    if globset.is_match(path) {
                        violations.push(ConstraintViolation {
                            constraint: constraint.clone(),
                            step_index: *idx,
                            detail: format!(
                                "Step {} references path '{}' forbidden by rule '{}'",
                                idx, path, rule_name
                            ),
                        });
                    }
                }
            }
        }
    }

    violations
}

/// Check a tool execution sequence against SequenceRequired constraints.
/// Returns violations found.
pub fn check_sequence_constraints(
    constraints: &[PlanConstraint],
    tool_sequence: &[(usize, &str)], // (step_index, tool_name)
) -> Vec<ConstraintViolation> {
    let mut violations = Vec::new();

    for constraint in constraints {
        if let PlanConstraint::SequenceRequired {
            sequence,
            rule_name,
            ..
        } = constraint
        {
            // Check that each item in the required sequence appears
            // before the next item.
            for window in sequence.windows(2) {
                let before = &window[0];
                let after = &window[1];

                let before_pos = tool_sequence
                    .iter()
                    .position(|(_, t)| t == before);
                let after_pos = tool_sequence
                    .iter()
                    .position(|(_, t)| t == after);

                match (before_pos, after_pos) {
                    (Some(b), Some(a)) if b > a => {
                        violations.push(ConstraintViolation {
                            constraint: constraint.clone(),
                            step_index: tool_sequence[a].0,
                            detail: format!(
                                "Rule '{}' requires '{}' before '{}', but order is reversed",
                                rule_name, before, after
                            ),
                        });
                    }
                    (None, Some(a_idx)) => {
                        violations.push(ConstraintViolation {
                            constraint: constraint.clone(),
                            step_index: tool_sequence[a_idx].0,
                            detail: format!(
                                "Rule '{}' requires '{}' before '{}', but '{}' is missing",
                                rule_name, before, after, before
                            ),
                        });
                    }
                    _ => {}
                }
            }
        }
    }

    violations
}
