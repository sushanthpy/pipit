//! Specification Language — Task 5.1
//!
//! A lightweight DSL capturing invariants, pre/post-conditions, and behavioral
//! constraints. Fragment of first-order logic with linear arithmetic (QF_LRA).
//!
//! Constraints: conjunctions/disjunctions of linear arithmetic over typed vars.
//! Consistency: C₁ ∧ C₂ ∧ ... ∧ Cₙ satisfiable? (Simplex-based).
//! Completeness: ∃x: valid_input(x) ∧ ¬(rule₁(x) ∨ ... ∨ ruleₖ(x))?

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A complete specification document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Spec {
    pub name: String,
    pub description: String,
    pub types: HashMap<String, SpecType>,
    pub rules: Vec<SpecRule>,
    pub invariants: Vec<SpecConstraint>,
}

/// Type definitions in the spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SpecType {
    Integer { min: Option<i64>, max: Option<i64> },
    Float { min: Option<f64>, max: Option<f64> },
    String { max_length: Option<usize>, pattern: Option<String> },
    Boolean,
    Enum { variants: Vec<String> },
    Struct { fields: HashMap<String, String> }, // field_name → type_name
    Array { element_type: String, max_length: Option<usize> },
}

/// A behavioral rule: if precondition → then postcondition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecRule {
    pub name: String,
    pub description: String,
    pub precondition: SpecConstraint,
    pub postcondition: SpecConstraint,
    /// Priority for ambiguity resolution (higher = more specific).
    pub priority: u32,
}

/// A constraint in the linear arithmetic fragment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SpecConstraint {
    /// Always true
    True,
    /// Always false
    False,
    /// Variable comparison: var <op> value
    Compare {
        var: String,
        cmp: CompareOp,
        value: ConstraintValue,
    },
    /// Conjunction: all must hold
    And { clauses: Vec<SpecConstraint> },
    /// Disjunction: at least one must hold
    Or { clauses: Vec<SpecConstraint> },
    /// Negation
    Not { inner: Box<SpecConstraint> },
    /// Linear inequality: Σ aᵢxᵢ ≤ b
    Linear {
        terms: Vec<(f64, String)>, // (coefficient, variable)
        bound: f64,
        comparison: CompareOp,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConstraintValue {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
}

impl Spec {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            description: String::new(),
            types: HashMap::new(),
            rules: Vec::new(),
            invariants: Vec::new(),
        }
    }

    /// Parse a spec from JSON.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Serialize to JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Check coverage: are there inputs where no rule applies?
    pub fn uncovered_inputs(&self) -> Vec<String> {
        // For each type, check if all valid values are covered by at least one rule
        let mut gaps = Vec::new();
        for (type_name, type_def) in &self.types {
            let covering_rules: Vec<_> = self.rules.iter()
                .filter(|r| constraint_references_var(&r.precondition, type_name))
                .collect();

            if covering_rules.is_empty() {
                gaps.push(format!("Type '{}' has no rules covering it", type_name));
            }

            // Check for boundary gaps in integer types
            if let SpecType::Integer { min: Some(lo), max: Some(hi) } = type_def {
                let boundary_values = vec![*lo, *lo + 1, (*lo + *hi) / 2, *hi - 1, *hi];
                for val in boundary_values {
                    let covered = covering_rules.iter().any(|r| {
                        constraint_admits_value(&r.precondition, type_name, val as f64)
                    });
                    if !covered {
                        gaps.push(format!("Value {}={} not covered by any rule", type_name, val));
                    }
                }
            }
        }
        gaps
    }
}

/// Check if a constraint references a given variable name.
fn constraint_references_var(c: &SpecConstraint, var: &str) -> bool {
    match c {
        SpecConstraint::True | SpecConstraint::False => false,
        SpecConstraint::Compare { var: v, .. } => v == var,
        SpecConstraint::And { clauses } | SpecConstraint::Or { clauses } => {
            clauses.iter().any(|cl| constraint_references_var(cl, var))
        }
        SpecConstraint::Not { inner } => constraint_references_var(inner, var),
        SpecConstraint::Linear { terms, .. } => terms.iter().any(|(_, v)| v == var),
    }
}

/// Check if a constraint admits a specific numeric value for a variable.
fn constraint_admits_value(c: &SpecConstraint, var: &str, val: f64) -> bool {
    match c {
        SpecConstraint::True => true,
        SpecConstraint::False => false,
        SpecConstraint::Compare { var: v, cmp, value } if v == var => {
            let target = match value {
                ConstraintValue::Int(i) => *i as f64,
                ConstraintValue::Float(f) => *f,
                _ => return false,
            };
            match cmp {
                CompareOp::Eq => (val - target).abs() < f64::EPSILON,
                CompareOp::Ne => (val - target).abs() >= f64::EPSILON,
                CompareOp::Lt => val < target,
                CompareOp::Le => val <= target,
                CompareOp::Gt => val > target,
                CompareOp::Ge => val >= target,
            }
        }
        SpecConstraint::Compare { .. } => true, // Different var, doesn't constrain
        SpecConstraint::And { clauses } => clauses.iter().all(|cl| constraint_admits_value(cl, var, val)),
        SpecConstraint::Or { clauses } => clauses.iter().any(|cl| constraint_admits_value(cl, var, val)),
        SpecConstraint::Not { inner } => !constraint_admits_value(inner, var, val),
        SpecConstraint::Linear { terms, bound, comparison } => {
            // Partial evaluation: substitute known var, leave others as 0
            let partial_sum: f64 = terms.iter()
                .map(|(coeff, v)| if v == var { coeff * val } else { 0.0 })
                .sum();
            match comparison {
                CompareOp::Le => partial_sum <= *bound,
                CompareOp::Lt => partial_sum < *bound,
                CompareOp::Ge => partial_sum >= *bound,
                CompareOp::Gt => partial_sum > *bound,
                CompareOp::Eq => (partial_sum - bound).abs() < f64::EPSILON,
                CompareOp::Ne => (partial_sum - bound).abs() >= f64::EPSILON,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spec_serialization_roundtrip() {
        let mut spec = Spec::new("pricing");
        spec.types.insert("price".into(), SpecType::Float { min: Some(0.0), max: Some(10000.0) });
        spec.rules.push(SpecRule {
            name: "free_tier".into(),
            description: "Free tier for low usage".into(),
            precondition: SpecConstraint::Compare {
                var: "usage".into(),
                cmp: CompareOp::Le,
                value: ConstraintValue::Int(100),
            },
            postcondition: SpecConstraint::Compare {
                var: "price".into(),
                cmp: CompareOp::Eq,
                value: ConstraintValue::Float(0.0),
            },
            priority: 1,
        });

        let json = spec.to_json().unwrap();
        let parsed = Spec::from_json(&json).unwrap();
        assert_eq!(parsed.name, "pricing");
        assert_eq!(parsed.rules.len(), 1);
    }

    #[test]
    fn test_constraint_admits_value() {
        let c = SpecConstraint::Compare {
            var: "x".into(),
            cmp: CompareOp::Le,
            value: ConstraintValue::Int(10),
        };
        assert!(constraint_admits_value(&c, "x", 5.0));
        assert!(constraint_admits_value(&c, "x", 10.0));
        assert!(!constraint_admits_value(&c, "x", 11.0));
    }
}
