//! Z3 Integration Layer — Task FV-3
//!
//! Translates CSL specs to SMT-LIB2 format and invokes Z3 as subprocess.
//! Subprocess (not linked library): +10ms latency, but crash isolation.
//! Consistency: (check-sat). Completeness: SAT on ¬(coverage).
//! Verification: bounded model checking on code CFG.

use crate::csl::*;
use serde::{Deserialize, Serialize};
use std::process::Command;

/// Result from the SMT solver.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SolverResult {
    Sat { model: String },
    Unsat,
    Unknown { reason: String },
    Error { message: String },
    Z3NotAvailable,
}

/// Translates CSL specs to SMT-LIB2 format.
pub struct SmtTranslator;

impl SmtTranslator {
    /// Translate a CSL spec to SMT-LIB2 string.
    pub fn to_smtlib2(spec: &CslSpec) -> String {
        let mut smt = String::new();
        smt.push_str("; Auto-generated from CSL spec\n");
        smt.push_str("(set-logic QF_LIA)\n\n");

        // Declare variables
        for var in &spec.variables {
            let sort = match &var.var_type {
                CslType::Int => "Int",
                CslType::Bool => "Bool",
                CslType::Enum(_) => "Int", // Encode enum as int
            };
            smt.push_str(&format!(
                "; {}\n(declare-const {} {})\n",
                var.description, var.name, sort
            ));
        }
        smt.push('\n');

        // Declare uninterpreted functions
        for func in &spec.functions {
            let param_sorts: Vec<&str> = func
                .params
                .iter()
                .map(|t| match t {
                    CslType::Int => "Int",
                    CslType::Bool => "Bool",
                    CslType::Enum(_) => "Int",
                })
                .collect();
            let ret_sort = match &func.return_type {
                CslType::Int => "Int",
                CslType::Bool => "Bool",
                CslType::Enum(_) => "Int",
            };
            smt.push_str(&format!(
                "(declare-fun {} ({}) {})\n",
                func.name,
                param_sorts.join(" "),
                ret_sort
            ));
        }

        // Assert constraints
        for (i, constraint) in spec.constraints.iter().enumerate() {
            let expr = Self::constraint_to_smt(constraint);
            smt.push_str(&format!("; Constraint {}\n(assert {})\n", i + 1, expr));
        }

        smt.push_str("\n(check-sat)\n(get-model)\n");
        smt
    }

    fn constraint_to_smt(c: &CslConstraint) -> String {
        match c {
            CslConstraint::Linear {
                terms,
                comparison,
                bound,
            } => {
                let lhs = if terms.len() == 1 {
                    let (coeff, var) = &terms[0];
                    if *coeff == 1 {
                        var.clone()
                    } else {
                        format!("(* {} {})", coeff, var)
                    }
                } else {
                    let parts: Vec<String> = terms
                        .iter()
                        .map(|(coeff, var)| {
                            if *coeff == 1 {
                                var.clone()
                            } else {
                                format!("(* {} {})", coeff, var)
                            }
                        })
                        .collect();
                    format!("(+ {})", parts.join(" "))
                };
                let op = match comparison {
                    CslCmp::Le => "<=",
                    CslCmp::Lt => "<",
                    CslCmp::Ge => ">=",
                    CslCmp::Gt => ">",
                    CslCmp::Eq => "=",
                    CslCmp::Ne => "distinct",
                };
                format!("({} {} {})", op, lhs, bound)
            }
            CslConstraint::BoolConst { var, value } => {
                if *value {
                    var.clone()
                } else {
                    format!("(not {})", var)
                }
            }
            CslConstraint::IntEqual { var, value } => format!("(= {} {})", var, value),
            CslConstraint::And(clauses) => {
                let parts: Vec<String> = clauses.iter().map(Self::constraint_to_smt).collect();
                format!("(and {})", parts.join(" "))
            }
            CslConstraint::Or(clauses) => {
                let parts: Vec<String> = clauses.iter().map(Self::constraint_to_smt).collect();
                format!("(or {})", parts.join(" "))
            }
            CslConstraint::Not(inner) => format!("(not {})", Self::constraint_to_smt(inner)),
            CslConstraint::Implies(p, q) => {
                format!(
                    "(=> {} {})",
                    Self::constraint_to_smt(p),
                    Self::constraint_to_smt(q)
                )
            }
            CslConstraint::FuncApp {
                function,
                args,
                result,
            } => {
                format!("(= ({} {}) {})", function, args.join(" "), result)
            }
            CslConstraint::ReviewRequired { constraint, .. } => {
                Self::constraint_to_smt(constraint) // Translate the inner constraint
            }
        }
    }

    /// Check consistency (satisfiability) via Z3 subprocess.
    pub fn check_consistency(spec: &CslSpec) -> SolverResult {
        let smt = Self::to_smtlib2(spec);
        Self::invoke_z3(&smt)
    }

    /// Check if there are uncovered inputs (completeness).
    pub fn check_completeness(spec: &CslSpec) -> SolverResult {
        let mut smt = String::new();
        smt.push_str("(set-logic QF_LIA)\n");

        for var in &spec.variables {
            let sort = match &var.var_type {
                CslType::Int => "Int",
                CslType::Bool => "Bool",
                CslType::Enum(_) => "Int",
            };
            smt.push_str(&format!("(declare-const {} {})\n", var.name, sort));
        }

        // Assert negation of coverage: ¬(C₁ ∨ C₂ ∨ ... ∨ Cₙ)
        if !spec.constraints.is_empty() {
            let disjunction: Vec<String> = spec
                .constraints
                .iter()
                .map(Self::constraint_to_smt)
                .collect();
            smt.push_str(&format!("(assert (not (or {})))\n", disjunction.join(" ")));
        }

        smt.push_str("(check-sat)\n(get-model)\n");
        Self::invoke_z3(&smt)
    }

    fn invoke_z3(smt_input: &str) -> SolverResult {
        // Check if z3 is available
        let z3_check = Command::new("z3").arg("--version").output();
        if z3_check.is_err() || !z3_check.as_ref().unwrap().status.success() {
            return SolverResult::Z3NotAvailable;
        }

        // Write to temp file and invoke
        let tmp = std::env::temp_dir().join("pipit-verify.smt2");
        if std::fs::write(&tmp, smt_input).is_err() {
            return SolverResult::Error {
                message: "Failed to write SMT file".into(),
            };
        }

        match Command::new("z3").arg("-smt2").arg(&tmp).output() {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let first_line = stdout.lines().next().unwrap_or("");

                match first_line.trim() {
                    "sat" => SolverResult::Sat {
                        model: stdout.to_string(),
                    },
                    "unsat" => SolverResult::Unsat,
                    "unknown" => SolverResult::Unknown {
                        reason: stdout.to_string(),
                    },
                    _ => SolverResult::Error {
                        message: stdout.to_string(),
                    },
                }
            }
            Err(e) => SolverResult::Error {
                message: e.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_smtlib_translation() {
        let mut spec = CslSpec::new("test");
        spec.add_variable("x", CslType::Int, "variable x");
        spec.add_variable("y", CslType::Int, "variable y");
        spec.add_constraint(CslConstraint::Linear {
            terms: vec![(1, "x".into()), (1, "y".into())],
            comparison: CslCmp::Le,
            bound: 100,
        });
        spec.add_constraint(CslConstraint::Linear {
            terms: vec![(1, "x".into())],
            comparison: CslCmp::Ge,
            bound: 0,
        });

        let smt = SmtTranslator::to_smtlib2(&spec);
        assert!(smt.contains("(declare-const x Int)"));
        assert!(smt.contains("(declare-const y Int)"));
        assert!(smt.contains("(<= (+ x y) 100)"));
        assert!(smt.contains("(>= x 0)"));
        assert!(smt.contains("(check-sat)"));
    }

    #[test]
    fn test_implies_translation() {
        let c = CslConstraint::Implies(
            Box::new(CslConstraint::Linear {
                terms: vec![(1, "x".into())],
                comparison: CslCmp::Gt,
                bound: 0,
            }),
            Box::new(CslConstraint::Linear {
                terms: vec![(1, "y".into())],
                comparison: CslCmp::Gt,
                bound: 0,
            }),
        );
        let smt = SmtTranslator::constraint_to_smt(&c);
        assert!(smt.contains("=>"), "Should contain implication: {}", smt);
    }
}
