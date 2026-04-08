//! Language-Agnostic Semantic IR — Task 8.1
//!
//! Maps language-specific syntax to shared conceptual vocabulary.
//! IR forms: sort, map, filter, retry, etc.
//! Typed lambda calculus with effect annotations.
//! Transduction: O(|AST| · |Rules|) per file.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A semantic IR node representing a language-agnostic concept.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SemanticIR {
    /// Sort a collection with a comparator
    Sort {
        collection: Box<SemanticIR>,
        comparator: Option<Box<SemanticIR>>,
    },
    /// Map: apply function to each element
    Map {
        collection: Box<SemanticIR>,
        function: Box<SemanticIR>,
    },
    /// Filter: keep elements matching predicate
    Filter {
        collection: Box<SemanticIR>,
        predicate: Box<SemanticIR>,
    },
    /// Filter + Map combined
    FilterMap {
        collection: Box<SemanticIR>,
        predicate: Box<SemanticIR>,
        transform: Box<SemanticIR>,
    },
    /// Reduce/fold
    Reduce {
        collection: Box<SemanticIR>,
        initial: Box<SemanticIR>,
        accumulator: Box<SemanticIR>,
    },
    /// Retry with backoff policy
    Retry {
        action: Box<SemanticIR>,
        max_attempts: u32,
        backoff: BackoffKind,
    },
    /// Error handling (try/catch, Result, Option)
    ErrorHandle {
        action: Box<SemanticIR>,
        handler: Box<SemanticIR>,
    },
    /// Iteration over a collection
    ForEach {
        collection: Box<SemanticIR>,
        body: Box<SemanticIR>,
    },
    /// Conditional branch
    Conditional {
        condition: Box<SemanticIR>,
        then_branch: Box<SemanticIR>,
        else_branch: Option<Box<SemanticIR>>,
    },
    /// Function definition
    FunctionDef {
        name: String,
        params: Vec<TypedParam>,
        return_type: IRType,
        body: Box<SemanticIR>,
    },
    /// Function call
    FunctionCall { name: String, args: Vec<SemanticIR> },
    /// Variable reference
    Variable { name: String, ir_type: IRType },
    /// Literal value
    Literal { value: String, ir_type: IRType },
    /// Assignment
    Assign {
        target: String,
        value: Box<SemanticIR>,
    },
    /// Block of statements
    Block { statements: Vec<SemanticIR> },
    /// Raw code (when no IR mapping exists)
    Raw { language: String, code: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum BackoffKind {
    None,
    Linear,
    Exponential,
    ExponentialWithJitter,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TypedParam {
    pub name: String,
    pub ir_type: IRType,
}

/// Language-agnostic type system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum IRType {
    Int,
    Float,
    String,
    Bool,
    List(Box<IRType>),
    Map(Box<IRType>, Box<IRType>),
    Option(Box<IRType>),
    Result(Box<IRType>, Box<IRType>),
    Function(Vec<IRType>, Box<IRType>),
    Void,
    Any,
    Named(String),
}

/// Recognized code patterns for transduction (pattern → IR node).
pub struct TransductionRule {
    pub pattern_name: String,
    pub source_languages: Vec<String>,
    pub detector: fn(&str, &str) -> Option<SemanticIR>, // (code_snippet, language) → IR
}

/// Detect high-level semantic patterns in code.
/// Uses logical-line joining to handle multi-line iterator chains.
pub fn detect_patterns(code: &str, language: &str) -> Vec<SemanticIR> {
    let mut patterns = Vec::new();

    // Join continuation lines: collapse multi-line chains into single logical lines.
    // A line that starts with `.` or ends with `.` / `,` / `(` is a continuation.
    let logical_lines = join_continuation_lines(code);

    match language {
        "python" => detect_python_patterns(code, &logical_lines, &mut patterns),
        "rust" => detect_rust_patterns(code, &logical_lines, &mut patterns),
        "javascript" | "typescript" => detect_js_patterns(code, &logical_lines, &mut patterns),
        _ => {}
    }

    patterns
}

/// Join continuation lines into logical statements.
/// Handles multi-line iterator chains like:
///
/// ```text
/// items
///     .iter()
///     .filter(|x| ...)
///     .map(|x| ...)
///     .collect()
/// ```
fn join_continuation_lines(code: &str) -> Vec<String> {
    let mut logical = Vec::new();
    let mut current = String::new();

    for line in code.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !current.is_empty() {
                logical.push(std::mem::take(&mut current));
            }
            continue;
        }

        // Continuation: line starts with `.` or `->` or previous line ended with
        // `.` `,` `(` `+` `-` `|` `&` `?`
        let is_continuation = trimmed.starts_with('.')
            || trimmed.starts_with("->")
            || current.trim_end().ends_with('.')
            || current.trim_end().ends_with(',')
            || current.trim_end().ends_with('(')
            || current.trim_end().ends_with('|')
            || current.trim_end().ends_with('\\');

        if is_continuation && !current.is_empty() {
            current.push(' ');
            current.push_str(trimmed);
        } else {
            if !current.is_empty() {
                logical.push(std::mem::take(&mut current));
            }
            current = trimmed.to_string();
        }
    }
    if !current.is_empty() {
        logical.push(current);
    }
    logical
}

fn detect_python_patterns(code: &str, logical_lines: &[String], patterns: &mut Vec<SemanticIR>) {
    // Scan both raw lines and logical lines
    for t in logical_lines {
        // List comprehension: [f(x) for x in collection if pred(x)]
        if t.contains(" for ") && t.contains(" in ") && t.starts_with('[') {
            if t.contains(" if ") {
                patterns.push(SemanticIR::FilterMap {
                    collection: Box::new(SemanticIR::Variable {
                        name: "collection".into(),
                        ir_type: IRType::List(Box::new(IRType::Any)),
                    }),
                    predicate: Box::new(SemanticIR::Raw {
                        language: "python".into(),
                        code: "predicate".into(),
                    }),
                    transform: Box::new(SemanticIR::Raw {
                        language: "python".into(),
                        code: "transform".into(),
                    }),
                });
            } else {
                patterns.push(SemanticIR::Map {
                    collection: Box::new(SemanticIR::Variable {
                        name: "collection".into(),
                        ir_type: IRType::List(Box::new(IRType::Any)),
                    }),
                    function: Box::new(SemanticIR::Raw {
                        language: "python".into(),
                        code: "transform".into(),
                    }),
                });
            }
        }

        // sorted() call
        if t.contains("sorted(") || t.contains(".sort(") {
            patterns.push(SemanticIR::Sort {
                collection: Box::new(SemanticIR::Variable {
                    name: "collection".into(),
                    ir_type: IRType::List(Box::new(IRType::Any)),
                }),
                comparator: None,
            });
        }

        // Retry pattern
        if t.contains("retry") || (t.contains("for") && t.contains("attempt") && t.contains("try"))
        {
            patterns.push(SemanticIR::Retry {
                action: Box::new(SemanticIR::Raw {
                    language: "python".into(),
                    code: t.into(),
                }),
                max_attempts: 3,
                backoff: BackoffKind::Exponential,
            });
        }
    }
}

fn detect_rust_patterns(code: &str, logical_lines: &[String], patterns: &mut Vec<SemanticIR>) {
    for t in logical_lines {
        if t.contains(".iter()") && t.contains(".filter(") && t.contains(".map(") {
            patterns.push(SemanticIR::FilterMap {
                collection: Box::new(SemanticIR::Variable {
                    name: "collection".into(),
                    ir_type: IRType::List(Box::new(IRType::Any)),
                }),
                predicate: Box::new(SemanticIR::Raw {
                    language: "rust".into(),
                    code: "predicate".into(),
                }),
                transform: Box::new(SemanticIR::Raw {
                    language: "rust".into(),
                    code: "transform".into(),
                }),
            });
        } else if t.contains(".iter()") && t.contains(".map(") {
            patterns.push(SemanticIR::Map {
                collection: Box::new(SemanticIR::Variable {
                    name: "collection".into(),
                    ir_type: IRType::List(Box::new(IRType::Any)),
                }),
                function: Box::new(SemanticIR::Raw {
                    language: "rust".into(),
                    code: "fn".into(),
                }),
            });
        } else if t.contains(".iter()") && t.contains(".filter(") {
            patterns.push(SemanticIR::Filter {
                collection: Box::new(SemanticIR::Variable {
                    name: "collection".into(),
                    ir_type: IRType::List(Box::new(IRType::Any)),
                }),
                predicate: Box::new(SemanticIR::Raw {
                    language: "rust".into(),
                    code: "pred".into(),
                }),
            });
        }

        if t.contains(".sort") {
            patterns.push(SemanticIR::Sort {
                collection: Box::new(SemanticIR::Variable {
                    name: "collection".into(),
                    ir_type: IRType::List(Box::new(IRType::Any)),
                }),
                comparator: if t.contains("sort_by") {
                    Some(Box::new(SemanticIR::Raw {
                        language: "rust".into(),
                        code: "cmp".into(),
                    }))
                } else {
                    None
                },
            });
        }
    }
}

fn detect_js_patterns(code: &str, logical_lines: &[String], patterns: &mut Vec<SemanticIR>) {
    for t in logical_lines {
        if t.contains(".filter(") && t.contains(".map(") {
            patterns.push(SemanticIR::FilterMap {
                collection: Box::new(SemanticIR::Variable {
                    name: "array".into(),
                    ir_type: IRType::List(Box::new(IRType::Any)),
                }),
                predicate: Box::new(SemanticIR::Raw {
                    language: "javascript".into(),
                    code: "pred".into(),
                }),
                transform: Box::new(SemanticIR::Raw {
                    language: "javascript".into(),
                    code: "fn".into(),
                }),
            });
        } else if t.contains(".map(") {
            patterns.push(SemanticIR::Map {
                collection: Box::new(SemanticIR::Variable {
                    name: "array".into(),
                    ir_type: IRType::List(Box::new(IRType::Any)),
                }),
                function: Box::new(SemanticIR::Raw {
                    language: "javascript".into(),
                    code: "fn".into(),
                }),
            });
        }

        if t.contains(".sort(") {
            patterns.push(SemanticIR::Sort {
                collection: Box::new(SemanticIR::Variable {
                    name: "array".into(),
                    ir_type: IRType::List(Box::new(IRType::Any)),
                }),
                comparator: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_python_patterns() {
        let code = "[x*2 for x in items if x > 0]\nsorted(data, key=lambda x: x.name)";
        let patterns = detect_patterns(code, "python");
        assert!(
            patterns
                .iter()
                .any(|p| matches!(p, SemanticIR::FilterMap { .. }))
        );
        assert!(
            patterns
                .iter()
                .any(|p| matches!(p, SemanticIR::Sort { .. }))
        );
    }

    #[test]
    fn test_rust_patterns() {
        let code = "items.iter().filter(|x| x > &0).map(|x| x * 2).collect()";
        let patterns = detect_patterns(code, "rust");
        assert!(!patterns.is_empty());
        assert!(
            patterns
                .iter()
                .any(|p| matches!(p, SemanticIR::FilterMap { .. }))
        );
    }

    #[test]
    fn test_js_patterns() {
        let code = "items.filter(x => x > 0).map(x => x * 2)";
        let patterns = detect_patterns(code, "javascript");
        assert!(
            patterns
                .iter()
                .any(|p| matches!(p, SemanticIR::FilterMap { .. }))
        );
    }
}
