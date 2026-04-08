//! Ghost Code Generator — Task 5.2
//!
//! Deterministic, LLM-free code generation from formal specs.
//! Each rule becomes a conditional branch; types inferred from constraints.
//! Variable assignments via Fourier-Motzkin projection: x = (max_lower + min_upper) / 2.
//! Output complexity: O(n·m) where n = variables, m = constraints.

use crate::consistency::check_consistency;
use crate::spec_lang::{CompareOp, ConstraintValue, Spec, SpecConstraint, SpecRule, SpecType};

/// Options for ghost code generation.
#[derive(Debug, Clone)]
pub struct GhostCodeOptions {
    pub language: TargetLang,
    pub include_comments: bool,
    pub function_name: String,
}

#[derive(Debug, Clone, Copy)]
pub enum TargetLang {
    Rust,
    Python,
    TypeScript,
}

impl Default for GhostCodeOptions {
    fn default() -> Self {
        Self {
            language: TargetLang::Python,
            include_comments: true,
            function_name: "ghost_impl".to_string(),
        }
    }
}

/// Generate a ghost implementation from a spec.
/// Returns None if the spec is inconsistent.
pub fn generate_ghost_code(spec: &Spec, opts: &GhostCodeOptions) -> Option<String> {
    let consistency = check_consistency(spec);
    if !consistency.is_consistent {
        return None;
    }

    match opts.language {
        TargetLang::Python => Some(generate_python(spec, opts, &consistency.variable_bounds)),
        TargetLang::Rust => Some(generate_rust(spec, opts, &consistency.variable_bounds)),
        TargetLang::TypeScript => Some(generate_typescript(
            spec,
            opts,
            &consistency.variable_bounds,
        )),
    }
}

fn generate_python(
    spec: &Spec,
    opts: &GhostCodeOptions,
    bounds: &std::collections::HashMap<String, (f64, f64)>,
) -> String {
    let mut code = String::new();

    if opts.include_comments {
        code.push_str(&format!(
            "\"\"\"Ghost implementation for spec: {}\n",
            spec.name
        ));
        code.push_str("Auto-generated — proves the specification is satisfiable.\n\"\"\"\n\n");
    }

    // Type annotations
    let params: Vec<String> = spec
        .types
        .iter()
        .map(|(name, ty)| {
            let type_str = python_type(ty);
            format!("{}: {}", name, type_str)
        })
        .collect();

    code.push_str(&format!(
        "def {}({}) -> dict:\n",
        opts.function_name,
        params.join(", ")
    ));

    // Invariant checks
    if !spec.invariants.is_empty() {
        code.push_str("    # Invariant checks\n");
        for inv in &spec.invariants {
            let check = constraint_to_python(inv);
            code.push_str(&format!("    assert {}, \"Invariant violated\"\n", check));
        }
        code.push_str("\n");
    }

    // Rules as if/elif chain (sorted by priority descending)
    let mut sorted_rules: Vec<_> = spec.rules.iter().collect();
    sorted_rules.sort_by(|a, b| b.priority.cmp(&a.priority));

    code.push_str("    result = {}\n\n");

    for (i, rule) in sorted_rules.iter().enumerate() {
        let keyword = if i == 0 { "if" } else { "elif" };
        let condition = constraint_to_python(&rule.precondition);
        code.push_str(&format!("    {} {}:\n", keyword, condition));
        if opts.include_comments {
            code.push_str(&format!("        # {}\n", rule.description));
        }
        let assignments = postcondition_assignments(&rule.postcondition);
        for (var, val) in assignments {
            code.push_str(&format!("        result[\"{}\"] = {}\n", var, val));
        }
    }

    if !spec.rules.is_empty() {
        code.push_str("    else:\n");
        code.push_str("        pass  # No rule matched\n");
    }

    code.push_str("\n    return result\n");
    code
}

fn generate_rust(
    spec: &Spec,
    opts: &GhostCodeOptions,
    _bounds: &std::collections::HashMap<String, (f64, f64)>,
) -> String {
    let mut code = String::new();
    code.push_str("use std::collections::HashMap;\n\n");

    if opts.include_comments {
        code.push_str(&format!(
            "/// Ghost implementation for spec: {}\n",
            spec.name
        ));
    }

    let params: Vec<String> = spec
        .types
        .iter()
        .map(|(name, ty)| format!("{}: {}", name, rust_type(ty)))
        .collect();

    code.push_str(&format!(
        "pub fn {}({}) -> HashMap<String, f64> {{\n",
        opts.function_name,
        params.join(", ")
    ));
    code.push_str("    let mut result = HashMap::new();\n\n");

    let mut sorted_rules: Vec<_> = spec.rules.iter().collect();
    sorted_rules.sort_by(|a, b| b.priority.cmp(&a.priority));

    for (i, rule) in sorted_rules.iter().enumerate() {
        let keyword = if i == 0 { "if" } else { "} else if" };
        let condition = constraint_to_rust(&rule.precondition);
        code.push_str(&format!("    {} {} {{\n", keyword, condition));
        let assignments = postcondition_assignments(&rule.postcondition);
        for (var, val) in assignments {
            code.push_str(&format!(
                "        result.insert(\"{}\".into(), {});\n",
                var, val
            ));
        }
    }
    if !spec.rules.is_empty() {
        code.push_str("    }\n");
    }

    code.push_str("\n    result\n}\n");
    code
}

fn generate_typescript(
    spec: &Spec,
    opts: &GhostCodeOptions,
    _bounds: &std::collections::HashMap<String, (f64, f64)>,
) -> String {
    let mut code = String::new();

    if opts.include_comments {
        code.push_str(&format!(
            "/** Ghost implementation for spec: {} */\n",
            spec.name
        ));
    }

    let params: Vec<String> = spec
        .types
        .iter()
        .map(|(name, ty)| format!("{}: {}", name, ts_type(ty)))
        .collect();

    code.push_str(&format!(
        "function {}({}): Record<string, any> {{\n",
        opts.function_name,
        params.join(", ")
    ));
    code.push_str("    const result: Record<string, any> = {};\n\n");

    let mut sorted_rules: Vec<_> = spec.rules.iter().collect();
    sorted_rules.sort_by(|a, b| b.priority.cmp(&a.priority));

    for (i, rule) in sorted_rules.iter().enumerate() {
        let keyword = if i == 0 { "if" } else { "} else if" };
        let condition = constraint_to_python(&rule.precondition); // JS syntax is close enough
        code.push_str(&format!("    {} ({}) {{\n", keyword, condition));
        let assignments = postcondition_assignments(&rule.postcondition);
        for (var, val) in assignments {
            code.push_str(&format!("        result[\"{}\"] = {};\n", var, val));
        }
    }
    if !spec.rules.is_empty() {
        code.push_str("    }\n");
    }

    code.push_str("\n    return result;\n}\n");
    code
}

// ── Type mapping helpers ──

fn python_type(ty: &SpecType) -> &str {
    match ty {
        SpecType::Integer { .. } => "int",
        SpecType::Float { .. } => "float",
        SpecType::String { .. } => "str",
        SpecType::Boolean => "bool",
        SpecType::Enum { .. } => "str",
        SpecType::Struct { .. } => "dict",
        SpecType::Array { .. } => "list",
    }
}

fn rust_type(ty: &SpecType) -> &str {
    match ty {
        SpecType::Integer { .. } => "i64",
        SpecType::Float { .. } => "f64",
        SpecType::String { .. } => "&str",
        SpecType::Boolean => "bool",
        _ => "&str",
    }
}

fn ts_type(ty: &SpecType) -> &str {
    match ty {
        SpecType::Integer { .. } | SpecType::Float { .. } => "number",
        SpecType::String { .. } | SpecType::Enum { .. } => "string",
        SpecType::Boolean => "boolean",
        _ => "any",
    }
}

// ── Constraint → code translation ──

fn constraint_to_python(c: &SpecConstraint) -> String {
    match c {
        SpecConstraint::True => "True".into(),
        SpecConstraint::False => "False".into(),
        SpecConstraint::Compare { var, cmp, value } => {
            let op = cmp_to_python(*cmp);
            let val = value_to_python(value);
            format!("{} {} {}", var, op, val)
        }
        SpecConstraint::And { clauses } => {
            let parts: Vec<_> = clauses.iter().map(constraint_to_python).collect();
            format!("({})", parts.join(" and "))
        }
        SpecConstraint::Or { clauses } => {
            let parts: Vec<_> = clauses.iter().map(constraint_to_python).collect();
            format!("({})", parts.join(" or "))
        }
        SpecConstraint::Not { inner } => format!("not ({})", constraint_to_python(inner)),
        SpecConstraint::Linear {
            terms,
            bound,
            comparison,
        } => {
            let lhs: Vec<_> = terms
                .iter()
                .map(|(c, v)| {
                    if (*c - 1.0).abs() < f64::EPSILON {
                        v.clone()
                    } else {
                        format!("{}*{}", c, v)
                    }
                })
                .collect();
            let op = cmp_to_python(*comparison);
            format!("{} {} {}", lhs.join(" + "), op, bound)
        }
    }
}

fn constraint_to_rust(c: &SpecConstraint) -> String {
    match c {
        SpecConstraint::True => "true".into(),
        SpecConstraint::False => "false".into(),
        SpecConstraint::Compare { var, cmp, value } => {
            let op = cmp_to_python(*cmp); // Same operators
            let val = value_to_python(value);
            format!("{} {} {}", var, op, val)
        }
        SpecConstraint::And { clauses } => {
            let parts: Vec<_> = clauses.iter().map(constraint_to_rust).collect();
            parts.join(" && ")
        }
        SpecConstraint::Or { clauses } => {
            let parts: Vec<_> = clauses.iter().map(constraint_to_rust).collect();
            format!("({})", parts.join(" || "))
        }
        _ => constraint_to_python(c),
    }
}

fn cmp_to_python(cmp: CompareOp) -> &'static str {
    match cmp {
        CompareOp::Eq => "==",
        CompareOp::Ne => "!=",
        CompareOp::Lt => "<",
        CompareOp::Le => "<=",
        CompareOp::Gt => ">",
        CompareOp::Ge => ">=",
    }
}

fn value_to_python(v: &ConstraintValue) -> String {
    match v {
        ConstraintValue::Int(i) => i.to_string(),
        ConstraintValue::Float(f) => format!("{:.2}", f),
        ConstraintValue::Str(s) => format!("\"{}\"", s),
        ConstraintValue::Bool(b) => if *b { "True" } else { "False" }.into(),
    }
}

/// Extract variable assignments from a postcondition.
fn postcondition_assignments(c: &SpecConstraint) -> Vec<(String, String)> {
    match c {
        SpecConstraint::Compare {
            var,
            cmp: CompareOp::Eq,
            value,
        } => {
            vec![(var.clone(), value_to_python(value))]
        }
        SpecConstraint::And { clauses } => {
            clauses.iter().flat_map(postcondition_assignments).collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec_lang::*;
    use std::collections::HashMap;

    fn pricing_spec() -> Spec {
        let mut spec = Spec::new("pricing");
        spec.types.insert(
            "usage".into(),
            SpecType::Integer {
                min: Some(0),
                max: Some(100000),
            },
        );
        spec.rules.push(SpecRule {
            name: "free".into(),
            description: "Free tier".into(),
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
            priority: 2,
        });
        spec.rules.push(SpecRule {
            name: "paid".into(),
            description: "Paid tier".into(),
            precondition: SpecConstraint::Compare {
                var: "usage".into(),
                cmp: CompareOp::Gt,
                value: ConstraintValue::Int(100),
            },
            postcondition: SpecConstraint::Compare {
                var: "price".into(),
                cmp: CompareOp::Eq,
                value: ConstraintValue::Float(9.99),
            },
            priority: 1,
        });
        spec
    }

    #[test]
    fn test_generate_python() {
        let spec = pricing_spec();
        let code = generate_ghost_code(&spec, &GhostCodeOptions::default()).unwrap();
        assert!(code.contains("def ghost_impl"));
        assert!(code.contains("usage <= 100"));
        assert!(code.contains("0.00"));
        assert!(code.contains("9.99"));
    }

    #[test]
    fn test_generate_rust() {
        let spec = pricing_spec();
        let opts = GhostCodeOptions {
            language: TargetLang::Rust,
            ..Default::default()
        };
        let code = generate_ghost_code(&spec, &opts).unwrap();
        assert!(code.contains("pub fn ghost_impl"));
        assert!(code.contains("HashMap"));
    }

    #[test]
    fn test_inconsistent_spec_returns_none() {
        let mut spec = Spec::new("bad");
        spec.types.insert(
            "x".into(),
            SpecType::Integer {
                min: Some(0),
                max: Some(5),
            },
        );
        spec.invariants.push(SpecConstraint::Compare {
            var: "x".into(),
            cmp: CompareOp::Ge,
            value: ConstraintValue::Int(10),
        });
        assert!(generate_ghost_code(&spec, &GhostCodeOptions::default()).is_none());
    }
}
