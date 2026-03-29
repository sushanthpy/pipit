//! Consistency Checker — Task 5.1 (part 2)
//!
//! Checks if spec constraints are satisfiable: C₁ ∧ ... ∧ Cₙ has a solution.
//! Uses a simplified Fourier-Motzkin-style bound propagation for QF_LRA.

use crate::spec_lang::{Spec, SpecConstraint, CompareOp, ConstraintValue};
use std::collections::HashMap;

/// Result of consistency checking.
#[derive(Debug, Clone)]
pub struct ConsistencyResult {
    pub is_consistent: bool,
    pub variable_bounds: HashMap<String, (f64, f64)>, // (lower, upper)
    pub conflicts: Vec<String>,
}

/// Check if a spec's invariants and rule preconditions are mutually consistent.
pub fn check_consistency(spec: &Spec) -> ConsistencyResult {
    let mut bounds: HashMap<String, (f64, f64)> = HashMap::new();
    let mut conflicts = Vec::new();

    // Initialize bounds from type definitions
    for (name, ty) in &spec.types {
        match ty {
            crate::spec_lang::SpecType::Integer { min, max } => {
                let lo = min.map(|v| v as f64).unwrap_or(f64::NEG_INFINITY);
                let hi = max.map(|v| v as f64).unwrap_or(f64::INFINITY);
                bounds.insert(name.clone(), (lo, hi));
            }
            crate::spec_lang::SpecType::Float { min, max } => {
                let lo = min.unwrap_or(f64::NEG_INFINITY);
                let hi = max.unwrap_or(f64::INFINITY);
                bounds.insert(name.clone(), (lo, hi));
            }
            _ => {}
        }
    }

    // Propagate constraints from invariants
    for invariant in &spec.invariants {
        propagate_bounds(invariant, &mut bounds, &mut conflicts);
    }

    // Propagate from rule preconditions (each rule must be individually feasible)
    for rule in &spec.rules {
        let mut rule_bounds = bounds.clone();
        let mut rule_conflicts = Vec::new();
        propagate_bounds(&rule.precondition, &mut rule_bounds, &mut rule_conflicts);
        if !rule_conflicts.is_empty() {
            conflicts.push(format!("Rule '{}' has contradictory precondition: {}", rule.name, rule_conflicts.join(", ")));
        }
    }

    // Check for infeasible bounds
    for (var, (lo, hi)) in &bounds {
        if lo > hi {
            conflicts.push(format!("Variable '{}' has empty range: [{}, {}]", var, lo, hi));
        }
    }

    ConsistencyResult {
        is_consistent: conflicts.is_empty(),
        variable_bounds: bounds,
        conflicts,
    }
}

/// Propagate a constraint into variable bounds (Fourier-Motzkin projection).
fn propagate_bounds(
    constraint: &SpecConstraint,
    bounds: &mut HashMap<String, (f64, f64)>,
    conflicts: &mut Vec<String>,
) {
    match constraint {
        SpecConstraint::True => {}
        SpecConstraint::False => conflicts.push("Explicit false constraint".into()),
        SpecConstraint::Compare { var, cmp, value } => {
            let val = match value {
                ConstraintValue::Int(i) => *i as f64,
                ConstraintValue::Float(f) => *f,
                _ => return,
            };
            let entry = bounds.entry(var.clone()).or_insert((f64::NEG_INFINITY, f64::INFINITY));
            match cmp {
                CompareOp::Le => entry.1 = entry.1.min(val),
                CompareOp::Lt => entry.1 = entry.1.min(val - f64::EPSILON),
                CompareOp::Ge => entry.0 = entry.0.max(val),
                CompareOp::Gt => entry.0 = entry.0.max(val + f64::EPSILON),
                CompareOp::Eq => {
                    entry.0 = entry.0.max(val);
                    entry.1 = entry.1.min(val);
                }
                CompareOp::Ne => {} // Can't tighten bounds from !=
            }
        }
        SpecConstraint::And { clauses } => {
            for clause in clauses {
                propagate_bounds(clause, bounds, conflicts);
            }
        }
        SpecConstraint::Or { .. } => {
            // For OR, we can't tighten bounds (any branch could be taken)
        }
        SpecConstraint::Not { .. } => {
            // Negation requires full complement, skip for simple propagation
        }
        SpecConstraint::Linear { terms, bound, comparison } => {
            // Single-variable linear: ax ≤ b → x ≤ b/a (if a > 0)
            if terms.len() == 1 {
                let (coeff, var) = &terms[0];
                if coeff.abs() > f64::EPSILON {
                    let entry = bounds.entry(var.clone()).or_insert((f64::NEG_INFINITY, f64::INFINITY));
                    let adjusted = bound / coeff;
                    if *coeff > 0.0 {
                        match comparison {
                            CompareOp::Le => entry.1 = entry.1.min(adjusted),
                            CompareOp::Lt => entry.1 = entry.1.min(adjusted - f64::EPSILON),
                            CompareOp::Ge => entry.0 = entry.0.max(adjusted),
                            CompareOp::Gt => entry.0 = entry.0.max(adjusted + f64::EPSILON),
                            _ => {}
                        }
                    } else {
                        // Negative coefficient flips the inequality
                        match comparison {
                            CompareOp::Le => entry.0 = entry.0.max(adjusted),
                            CompareOp::Ge => entry.1 = entry.1.min(adjusted),
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec_lang::*;

    #[test]
    fn test_consistent_spec() {
        let mut spec = Spec::new("test");
        spec.types.insert("x".into(), SpecType::Integer { min: Some(0), max: Some(100) });
        spec.invariants.push(SpecConstraint::Compare {
            var: "x".into(), cmp: CompareOp::Ge, value: ConstraintValue::Int(10),
        });

        let result = check_consistency(&spec);
        assert!(result.is_consistent, "Should be consistent: {:?}", result.conflicts);
        let (lo, hi) = result.variable_bounds["x"];
        assert!(lo >= 10.0 && hi <= 100.0, "Bounds: [{}, {}]", lo, hi);
    }

    #[test]
    fn test_inconsistent_spec() {
        let mut spec = Spec::new("test");
        spec.types.insert("x".into(), SpecType::Integer { min: Some(0), max: Some(5) });
        spec.invariants.push(SpecConstraint::Compare {
            var: "x".into(), cmp: CompareOp::Ge, value: ConstraintValue::Int(10),
        });

        let result = check_consistency(&spec);
        assert!(!result.is_consistent, "x ∈ [0,5] ∧ x≥10 should be inconsistent");
    }
}
