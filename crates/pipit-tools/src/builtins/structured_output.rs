//! Structured Output Tool (Task 2.4)
//!
//! Enables SDK callers to request JSON schema-constrained output from the model.
//! When a caller specifies a `jsonSchema`, this tool is registered and a
//! validation hook ensures the model's final output conforms to the schema.

use crate::{Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use pipit_config::ApprovalMode;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Tool that captures structured output from the model and validates it
/// against a JSON schema provided at registration time.
pub struct StructuredOutputTool {
    /// The JSON schema that output must conform to.
    schema_def: Value,
    /// Human-readable description of what the output should contain.
    output_description: String,
}

impl StructuredOutputTool {
    pub fn new(schema: Value, description: String) -> Self {
        Self {
            schema_def: schema,
            output_description: description,
        }
    }
}

#[async_trait]
impl Tool for StructuredOutputTool {
    fn name(&self) -> &str {
        "structured_output"
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "output": {
                    "type": "string",
                    "description": format!(
                        "The structured output as a JSON string conforming to the required schema. {}",
                        self.output_description
                    )
                }
            },
            "required": ["output"]
        })
    }

    fn description(&self) -> &str {
        "Emit structured output conforming to a JSON schema. \
         The output will be validated against the schema before delivery."
    }

    async fn execute(
        &self,
        args: Value,
        _ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let output_str = args["output"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArgs("missing 'output' string".to_string()))?;

        // Parse the output as JSON
        let output_value: Value = serde_json::from_str(output_str).map_err(|e| {
            ToolError::InvalidArgs(format!(
                "Output is not valid JSON: {}. Output was: {}",
                e,
                truncate(output_str, 200)
            ))
        })?;

        // Validate against schema
        match validate_against_schema(&output_value, &self.schema_def) {
            Ok(()) => Ok(ToolResult::text(format!(
                "Structured output validated successfully.\n{}",
                serde_json::to_string_pretty(&output_value).unwrap_or_default()
            ))),
            Err(errors) => Err(ToolError::InvalidArgs(format!(
                "Output does not conform to schema:\n{}",
                errors.join("\n")
            ))),
        }
    }
}

/// Basic JSON Schema validation (subset of JSON Schema Draft 7).
/// Validates type, required fields, enum values, and nested objects/arrays.
fn validate_against_schema(value: &Value, schema: &Value) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    validate_recursive(value, schema, "", &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_recursive(value: &Value, schema: &Value, path: &str, errors: &mut Vec<String>) {
    // Check type constraint
    if let Some(expected_type) = schema.get("type").and_then(|t| t.as_str()) {
        let actual_type = match value {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(n) => {
                if n.is_i64() || n.is_u64() {
                    "integer"
                } else {
                    "number"
                }
            }
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        };

        // "number" accepts "integer" as well
        let type_matches = actual_type == expected_type
            || (expected_type == "number" && actual_type == "integer");

        if !type_matches {
            errors.push(format!(
                "{}: expected type '{}', got '{}'",
                if path.is_empty() { "$" } else { path },
                expected_type,
                actual_type
            ));
            return;
        }
    }

    // Check enum constraint
    if let Some(enum_values) = schema.get("enum").and_then(|e| e.as_array()) {
        if !enum_values.contains(value) {
            errors.push(format!(
                "{}: value not in enum {:?}",
                if path.is_empty() { "$" } else { path },
                enum_values
            ));
        }
    }

    // Check required fields for objects
    if let (Some(obj), Some(required)) = (
        value.as_object(),
        schema.get("required").and_then(|r| r.as_array()),
    ) {
        for req in required {
            if let Some(field_name) = req.as_str() {
                if !obj.contains_key(field_name) {
                    errors.push(format!(
                        "{}: missing required field '{}'",
                        if path.is_empty() { "$" } else { path },
                        field_name
                    ));
                }
            }
        }
    }

    // Validate properties of objects
    if let (Some(obj), Some(properties)) = (
        value.as_object(),
        schema.get("properties").and_then(|p| p.as_object()),
    ) {
        for (key, prop_schema) in properties {
            if let Some(prop_value) = obj.get(key) {
                let child_path = if path.is_empty() {
                    format!("$.{}", key)
                } else {
                    format!("{}.{}", path, key)
                };
                validate_recursive(prop_value, prop_schema, &child_path, errors);
            }
        }
    }

    // Validate array items
    if let (Some(arr), Some(items_schema)) = (value.as_array(), schema.get("items")) {
        for (i, item) in arr.iter().enumerate() {
            let child_path = if path.is_empty() {
                format!("$[{}]", i)
            } else {
                format!("{}[{}]", path, i)
            };
            validate_recursive(item, items_schema, &child_path, errors);
        }
    }

    // Check minItems / maxItems for arrays
    if let Some(arr) = value.as_array() {
        if let Some(min) = schema.get("minItems").and_then(|m| m.as_u64()) {
            if (arr.len() as u64) < min {
                errors.push(format!(
                    "{}: array has {} items, minimum is {}",
                    if path.is_empty() { "$" } else { path },
                    arr.len(),
                    min
                ));
            }
        }
        if let Some(max) = schema.get("maxItems").and_then(|m| m.as_u64()) {
            if (arr.len() as u64) > max {
                errors.push(format!(
                    "{}: array has {} items, maximum is {}",
                    if path.is_empty() { "$" } else { path },
                    arr.len(),
                    max
                ));
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}
