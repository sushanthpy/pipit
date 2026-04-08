//! Idiomatic Code Projector — Task 8.2
//!
//! Maps semantic IR to idiomatic target-language code.
//! Each IR form has ranked projections per language (idiomaticity score).
//! Always selects the highest-scoring projection.
//! Runtime: O(|IR| · max_projections_per_node).

use crate::semantic_ir::*;

/// Project a semantic IR node to idiomatic code in the target language.
pub fn project(ir: &SemanticIR, lang: &str) -> String {
    match lang {
        "python" => project_python(ir),
        "rust" => project_rust(ir),
        "javascript" | "typescript" => project_js(ir, lang),
        _ => project_generic(ir),
    }
}

fn project_python(ir: &SemanticIR) -> String {
    match ir {
        SemanticIR::Sort {
            collection,
            comparator: None,
        } => {
            format!("sorted({})", project_python(collection))
        }
        SemanticIR::Sort {
            collection,
            comparator: Some(cmp),
        } => {
            format!(
                "sorted({}, key={})",
                project_python(collection),
                project_python(cmp)
            )
        }
        SemanticIR::Map {
            collection,
            function,
        } => {
            // Idiomatic: list comprehension (0.95) > map() (0.60) > loop (0.30)
            format!(
                "[{}(x) for x in {}]",
                project_python(function),
                project_python(collection)
            )
        }
        SemanticIR::Filter {
            collection,
            predicate,
        } => {
            format!(
                "[x for x in {} if {}(x)]",
                project_python(collection),
                project_python(predicate)
            )
        }
        SemanticIR::FilterMap {
            collection,
            predicate,
            transform,
        } => {
            format!(
                "[{}(x) for x in {} if {}(x)]",
                project_python(transform),
                project_python(collection),
                project_python(predicate)
            )
        }
        SemanticIR::Reduce {
            collection,
            initial,
            accumulator,
        } => {
            format!(
                "functools.reduce({}, {}, {})",
                project_python(accumulator),
                project_python(collection),
                project_python(initial)
            )
        }
        SemanticIR::ForEach { collection, body } => {
            format!(
                "for item in {}:\n    {}",
                project_python(collection),
                project_python(body)
            )
        }
        SemanticIR::Conditional {
            condition,
            then_branch,
            else_branch,
        } => {
            let mut s = format!(
                "if {}:\n    {}",
                project_python(condition),
                project_python(then_branch)
            );
            if let Some(eb) = else_branch {
                s.push_str(&format!("\nelse:\n    {}", project_python(eb)));
            }
            s
        }
        SemanticIR::ErrorHandle { action, handler } => {
            format!(
                "try:\n    {}\nexcept Exception as e:\n    {}",
                project_python(action),
                project_python(handler)
            )
        }
        SemanticIR::Retry {
            action,
            max_attempts,
            backoff,
        } => {
            let backoff_code = match backoff {
                BackoffKind::Exponential | BackoffKind::ExponentialWithJitter => {
                    "time.sleep(2 ** attempt)"
                }
                BackoffKind::Linear => "time.sleep(attempt)",
                BackoffKind::None => "pass",
            };
            format!(
                "for attempt in range({}):\n    try:\n        {}\n        break\n    except Exception:\n        {}",
                max_attempts,
                project_python(action),
                backoff_code
            )
        }
        SemanticIR::FunctionDef {
            name, params, body, ..
        } => {
            let param_str: Vec<_> = params.iter().map(|p| p.name.clone()).collect();
            format!(
                "def {}({}):\n    {}",
                name,
                param_str.join(", "),
                project_python(body)
            )
        }
        SemanticIR::FunctionCall { name, args } => {
            let arg_str: Vec<_> = args.iter().map(|a| project_python(a)).collect();
            format!("{}({})", name, arg_str.join(", "))
        }
        SemanticIR::Variable { name, .. } => name.clone(),
        SemanticIR::Literal { value, .. } => value.clone(),
        SemanticIR::Assign { target, value } => format!("{} = {}", target, project_python(value)),
        SemanticIR::Block { statements } => statements
            .iter()
            .map(|s| project_python(s))
            .collect::<Vec<_>>()
            .join("\n"),
        SemanticIR::Raw { code, .. } => code.clone(),
    }
}

fn project_rust(ir: &SemanticIR) -> String {
    match ir {
        SemanticIR::Sort {
            collection,
            comparator: None,
        } => {
            format!(
                "{{\n    let mut v = {}.clone();\n    v.sort();\n    v\n}}",
                project_rust(collection)
            )
        }
        SemanticIR::Sort {
            collection,
            comparator: Some(cmp),
        } => {
            format!(
                "{{\n    let mut v = {}.clone();\n    v.sort_by({});\n    v\n}}",
                project_rust(collection),
                project_rust(cmp)
            )
        }
        SemanticIR::Map {
            collection,
            function,
        } => {
            format!(
                "{}.iter().map(|x| {}(x)).collect::<Vec<_>>()",
                project_rust(collection),
                project_rust(function)
            )
        }
        SemanticIR::Filter {
            collection,
            predicate,
        } => {
            format!(
                "{}.iter().filter(|x| {}(x)).cloned().collect::<Vec<_>>()",
                project_rust(collection),
                project_rust(predicate)
            )
        }
        SemanticIR::FilterMap {
            collection,
            predicate,
            transform,
        } => {
            format!(
                "{}.iter().filter(|x| {}(x)).map(|x| {}(x)).collect::<Vec<_>>()",
                project_rust(collection),
                project_rust(predicate),
                project_rust(transform)
            )
        }
        SemanticIR::ErrorHandle { action, handler } => {
            format!(
                "match {} {{\n    Ok(v) => v,\n    Err(e) => {},\n}}",
                project_rust(action),
                project_rust(handler)
            )
        }
        SemanticIR::Conditional {
            condition,
            then_branch,
            else_branch,
        } => {
            let mut s = format!(
                "if {} {{\n    {}\n}}",
                project_rust(condition),
                project_rust(then_branch)
            );
            if let Some(eb) = else_branch {
                s.push_str(&format!(" else {{\n    {}\n}}", project_rust(eb)));
            }
            s
        }
        SemanticIR::FunctionDef {
            name,
            params,
            return_type,
            body,
        } => {
            let param_str: Vec<_> = params
                .iter()
                .map(|p| format!("{}: {}", p.name, ir_type_to_rust(return_type)))
                .collect();
            format!(
                "fn {}({}) -> {} {{\n    {}\n}}",
                name,
                param_str.join(", "),
                ir_type_to_rust(return_type),
                project_rust(body)
            )
        }
        SemanticIR::Variable { name, .. } => name.clone(),
        SemanticIR::Literal { value, .. } => value.clone(),
        SemanticIR::Assign { target, value } => {
            format!("let {} = {};", target, project_rust(value))
        }
        SemanticIR::Block { statements } => statements
            .iter()
            .map(|s| project_rust(s))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => project_generic(ir),
    }
}

fn project_js(ir: &SemanticIR, _lang: &str) -> String {
    match ir {
        SemanticIR::Sort { collection, .. } => {
            format!("[...{}].sort()", project_js(collection, _lang))
        }
        SemanticIR::Map {
            collection,
            function,
        } => format!(
            "{}.map({})",
            project_js(collection, _lang),
            project_js(function, _lang)
        ),
        SemanticIR::Filter {
            collection,
            predicate,
        } => format!(
            "{}.filter({})",
            project_js(collection, _lang),
            project_js(predicate, _lang)
        ),
        SemanticIR::FilterMap {
            collection,
            predicate,
            transform,
        } => {
            format!(
                "{}.filter({}).map({})",
                project_js(collection, _lang),
                project_js(predicate, _lang),
                project_js(transform, _lang)
            )
        }
        SemanticIR::Reduce {
            collection,
            initial,
            accumulator,
        } => {
            format!(
                "{}.reduce({}, {})",
                project_js(collection, _lang),
                project_js(accumulator, _lang),
                project_js(initial, _lang)
            )
        }
        SemanticIR::ErrorHandle { action, handler } => {
            format!(
                "try {{\n    {}\n}} catch (e) {{\n    {}\n}}",
                project_js(action, _lang),
                project_js(handler, _lang)
            )
        }
        SemanticIR::Variable { name, .. } => name.clone(),
        SemanticIR::Literal { value, .. } => value.clone(),
        _ => project_generic(ir),
    }
}

fn project_generic(ir: &SemanticIR) -> String {
    format!("{:?}", ir)
}

fn ir_type_to_rust(ty: &IRType) -> &str {
    match ty {
        IRType::Int => "i64",
        IRType::Float => "f64",
        IRType::String => "String",
        IRType::Bool => "bool",
        IRType::Void => "()",
        _ => "_",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_python_filter_map_is_comprehension() {
        let ir = SemanticIR::FilterMap {
            collection: Box::new(SemanticIR::Variable {
                name: "items".into(),
                ir_type: IRType::List(Box::new(IRType::Int)),
            }),
            predicate: Box::new(SemanticIR::Variable {
                name: "is_valid".into(),
                ir_type: IRType::Any,
            }),
            transform: Box::new(SemanticIR::Variable {
                name: "process".into(),
                ir_type: IRType::Any,
            }),
        };
        let code = project(&ir, "python");
        assert!(
            code.contains("for x in items"),
            "Should use list comprehension: {}",
            code
        );
        assert!(
            code.contains("if is_valid(x)"),
            "Should use inline if: {}",
            code
        );
    }

    #[test]
    fn test_rust_filter_map_is_iterator_chain() {
        let ir = SemanticIR::FilterMap {
            collection: Box::new(SemanticIR::Variable {
                name: "items".into(),
                ir_type: IRType::List(Box::new(IRType::Int)),
            }),
            predicate: Box::new(SemanticIR::Variable {
                name: "is_valid".into(),
                ir_type: IRType::Any,
            }),
            transform: Box::new(SemanticIR::Variable {
                name: "process".into(),
                ir_type: IRType::Any,
            }),
        };
        let code = project(&ir, "rust");
        assert!(
            code.contains(".iter().filter("),
            "Should use iterator chain: {}",
            code
        );
        assert!(code.contains(".map("), "Should chain map: {}", code);
        assert!(code.contains("collect"), "Should collect: {}", code);
    }

    #[test]
    fn test_js_filter_map() {
        let ir = SemanticIR::FilterMap {
            collection: Box::new(SemanticIR::Variable {
                name: "items".into(),
                ir_type: IRType::List(Box::new(IRType::Int)),
            }),
            predicate: Box::new(SemanticIR::Variable {
                name: "isValid".into(),
                ir_type: IRType::Any,
            }),
            transform: Box::new(SemanticIR::Variable {
                name: "process".into(),
                ir_type: IRType::Any,
            }),
        };
        let code = project(&ir, "javascript");
        assert!(
            code.contains(".filter(isValid).map(process)"),
            "Should chain: {}",
            code
        );
    }
}
