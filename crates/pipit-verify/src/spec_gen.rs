//! LLM Spec Generator — Task FV-2
//!
//! Generates CSL specifications from natural language requirements.
//! Per-constraint accuracy ~85% → per-spec P(all correct) = 0.85^n.
//! [REVIEW] markers on uncertain constraints reduce human effort ~85%.

use crate::csl::*;
use serde::{Deserialize, Serialize};

/// Prompt template for LLM spec generation.
pub fn build_spec_generation_prompt(requirement: &str, csl_grammar: &str) -> String {
    format!(
        r#"You are generating a formal constraint specification from a natural language requirement.

## CSL Grammar
{grammar}

## Requirement
{requirement}

## Instructions
1. Declare all variables with types (Int, Bool, or Enum).
2. Express each requirement as a CSL constraint.
3. If you are uncertain about a constraint, wrap it with REVIEW: and explain why.
4. Output ONLY valid CSL JSON. No explanations outside the JSON.

## Examples
Input: "balance must never go negative"
Output: {{"variables": [{{"name": "balance", "var_type": "Int", "description": "Account balance"}}],
         "constraints": [{{"Linear": {{"terms": [[1, "balance"]], "comparison": "Ge", "bound": 0}}}}]}}

Input: "transfer amount must be positive and not exceed balance"
Output: {{"variables": [
    {{"name": "amount", "var_type": "Int", "description": "Transfer amount"}},
    {{"name": "balance", "var_type": "Int", "description": "Current balance"}}
  ],
  "constraints": [
    {{"Linear": {{"terms": [[1, "amount"]], "comparison": "Gt", "bound": 0}}}},
    {{"Linear": {{"terms": [[1, "amount"], [-1, "balance"]], "comparison": "Le", "bound": 0}}}}
  ]}}

Now generate for the following requirement:
{requirement}"#,
        grammar = csl_grammar,
        requirement = requirement,
    )
}

/// Template CSL grammar documentation for the LLM prompt.
pub fn csl_grammar_doc() -> &'static str {
    r#"CSL (Constraint Specification Language) — JSON format:
- Variables: {"name": str, "var_type": "Int"|"Bool"|{"Enum": [str...]}, "description": str}
- Constraints (one of):
  - {"Linear": {"terms": [[coeff, var], ...], "comparison": "Le"|"Lt"|"Ge"|"Gt"|"Eq"|"Ne", "bound": int}}
  - {"BoolConst": {"var": str, "value": bool}}
  - {"IntEqual": {"var": str, "value": int}}
  - {"And": [constraint, ...]}
  - {"Or": [constraint, ...]}
  - {"Not": constraint}
  - {"Implies": [precondition, postcondition]}
  - {"ReviewRequired": {"constraint": constraint, "reason": str}}
- Functions (uninterpreted): {"name": str, "params": [type, ...], "return_type": type}"#
}

/// Parse LLM-generated CSL JSON into a CslSpec, handling partial/malformed output.
pub fn parse_llm_spec_output(name: &str, json_text: &str) -> Result<CslSpec, String> {
    // Try to extract JSON from the response (may have markdown fences)
    let json = extract_json(json_text);

    let parsed: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("Invalid JSON from LLM: {}", e))?;

    let mut spec = CslSpec::new(name);

    // Parse variables
    if let Some(vars) = parsed.get("variables").and_then(|v| v.as_array()) {
        for var in vars {
            let name = var
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let var_type = match var.get("var_type").and_then(|v| v.as_str()) {
                Some("Int") => CslType::Int,
                Some("Bool") => CslType::Bool,
                _ => CslType::Int,
            };
            let desc = var
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            spec.add_variable(name, var_type, desc);
        }
    }

    // Parse constraints
    if let Some(constraints) = parsed.get("constraints").and_then(|v| v.as_array()) {
        for c in constraints {
            if let Some(constraint) = parse_constraint(c) {
                spec.add_constraint(constraint);
            }
        }
    }

    Ok(spec)
}

fn parse_constraint(value: &serde_json::Value) -> Option<CslConstraint> {
    if let Some(linear) = value.get("Linear") {
        let terms: Vec<(i64, String)> = linear
            .get("terms")
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|term| {
                        let arr = term.as_array()?;
                        Some((arr.first()?.as_i64()?, arr.get(1)?.as_str()?.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let cmp = match linear.get("comparison").and_then(|v| v.as_str()) {
            Some("Le") => CslCmp::Le,
            Some("Lt") => CslCmp::Lt,
            Some("Ge") => CslCmp::Ge,
            Some("Gt") => CslCmp::Gt,
            Some("Eq") => CslCmp::Eq,
            Some("Ne") => CslCmp::Ne,
            _ => CslCmp::Le,
        };
        let bound = linear.get("bound").and_then(|v| v.as_i64()).unwrap_or(0);
        return Some(CslConstraint::Linear {
            terms,
            comparison: cmp,
            bound,
        });
    }

    if let Some(beq) = value.get("BoolConst") {
        return Some(CslConstraint::BoolConst {
            var: beq.get("var").and_then(|v| v.as_str()).unwrap_or("").into(),
            value: beq.get("value").and_then(|v| v.as_bool()).unwrap_or(false),
        });
    }

    if let Some(ieq) = value.get("IntEqual") {
        return Some(CslConstraint::IntEqual {
            var: ieq.get("var").and_then(|v| v.as_str()).unwrap_or("").into(),
            value: ieq.get("value").and_then(|v| v.as_i64()).unwrap_or(0),
        });
    }

    if let Some(review) = value.get("ReviewRequired") {
        let inner = review.get("constraint").and_then(parse_constraint)?;
        let reason = review
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .into();
        return Some(CslConstraint::ReviewRequired {
            constraint: Box::new(inner),
            reason,
        });
    }

    None
}

fn extract_json(text: &str) -> String {
    // Remove markdown code fences
    let cleaned = text.trim();
    if let Some(start) = cleaned.find('{') {
        if let Some(end) = cleaned.rfind('}') {
            return cleaned[start..=end].to_string();
        }
    }
    cleaned.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_spec() {
        let json = r#"{"variables": [{"name": "x", "var_type": "Int", "description": "value"}],
                       "constraints": [{"Linear": {"terms": [[1, "x"]], "comparison": "Ge", "bound": 0}}]}"#;
        let spec = parse_llm_spec_output("test", json).unwrap();
        assert_eq!(spec.variables.len(), 1);
        assert_eq!(spec.constraints.len(), 1);
    }

    #[test]
    fn test_parse_with_review_marker() {
        let json = r#"{"variables": [{"name": "x", "var_type": "Int", "description": "input"}],
                       "constraints": [
                           {"Linear": {"terms": [[1, "x"]], "comparison": "Ge", "bound": 0}},
                           {"ReviewRequired": {"constraint": {"Linear": {"terms": [[1, "x"]], "comparison": "Le", "bound": 1000}}, "reason": "upper bound unclear"}}
                       ]}"#;
        let spec = parse_llm_spec_output("test", json).unwrap();
        assert_eq!(spec.review_count(), 1);
    }

    #[test]
    fn test_grammar_doc() {
        let doc = csl_grammar_doc();
        assert!(doc.contains("Linear"));
        assert!(doc.contains("ReviewRequired"));
    }
}
