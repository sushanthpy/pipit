//! # Typed Bridge Envelope (Architecture Task 5)
//!
//! Typed in-memory representations for tool-call and approval payloads.
//! At the wire boundary (SDK/bridge), these serialize to/from
//! `serde_json::Value` for backward compatibility. Internal processing
//! uses typed pattern matching with exhaustiveness guarantees.
//!
//! Migration path: typed envelope internally, JSON fallback at the edge.
//! Version negotiation via `SdkVersion` controls which fields are serialized.
//!
//! Boundary decoding: O(n) in payload size.
//! Internal dispatch: typed pattern matching (compile-time exhaustiveness).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Typed tool call arguments — exhaustive match replaces dynamic JSON validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum TypedToolArgs {
    /// Read a file.
    ReadFile {
        path: String,
        #[serde(default)]
        start_line: Option<u64>,
        #[serde(default)]
        end_line: Option<u64>,
    },
    /// Edit a file.
    EditFile {
        path: String,
        old_text: String,
        new_text: String,
    },
    /// Write a new file.
    WriteFile { path: String, content: String },
    /// Execute a shell command.
    Bash {
        command: String,
        #[serde(default)]
        working_dir: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// Search files.
    Search {
        query: String,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        regex: bool,
    },
    /// List directory contents.
    ListDir { path: String },
    /// Delegate to a subagent.
    Subagent {
        task: String,
        #[serde(default)]
        context: Option<String>,
        #[serde(default)]
        allowed_tools: Vec<String>,
        #[serde(default)]
        isolated: bool,
    },
    /// Any tool not yet in the typed system — wire fallback.
    Untyped {
        tool_name: String,
        args: serde_json::Value,
    },
}

impl TypedToolArgs {
    /// Tool name for display and dispatch.
    pub fn tool_name(&self) -> &str {
        match self {
            Self::ReadFile { .. } => "read_file",
            Self::EditFile { .. } => "edit_file",
            Self::WriteFile { .. } => "write_file",
            Self::Bash { .. } => "bash",
            Self::Search { .. } => "search",
            Self::ListDir { .. } => "list_dir",
            Self::Subagent { .. } => "subagent",
            Self::Untyped { tool_name, .. } => tool_name,
        }
    }

    /// Whether this tool call is read-only (no side effects).
    pub fn is_read_only(&self) -> bool {
        matches!(
            self,
            Self::ReadFile { .. } | Self::Search { .. } | Self::ListDir { .. }
        )
    }

    /// Whether this tool call mutates files.
    pub fn is_file_mutation(&self) -> bool {
        matches!(self, Self::EditFile { .. } | Self::WriteFile { .. })
    }

    /// Convert from untyped JSON (wire boundary decode).
    /// Falls back to `Untyped` if the tool name is not recognized.
    pub fn from_wire(tool_name: &str, args: &serde_json::Value) -> Self {
        match tool_name {
            "read_file" => Self::ReadFile {
                path: args["path"].as_str().unwrap_or("").to_string(),
                start_line: args["start_line"].as_u64(),
                end_line: args["end_line"].as_u64(),
            },
            "edit_file" => Self::EditFile {
                path: args["path"].as_str().unwrap_or("").to_string(),
                old_text: args["old_text"].as_str().unwrap_or("").to_string(),
                new_text: args["new_text"].as_str().unwrap_or("").to_string(),
            },
            "write_file" => Self::WriteFile {
                path: args["path"].as_str().unwrap_or("").to_string(),
                content: args["content"].as_str().unwrap_or("").to_string(),
            },
            "bash" => Self::Bash {
                command: args["command"].as_str().unwrap_or("").to_string(),
                working_dir: args["working_dir"].as_str().map(|s| s.to_string()),
                timeout_ms: args["timeout_ms"].as_u64(),
            },
            "search" => Self::Search {
                query: args["query"].as_str().unwrap_or("").to_string(),
                path: args["path"].as_str().map(|s| s.to_string()),
                regex: args["regex"].as_bool().unwrap_or(false),
            },
            "list_dir" => Self::ListDir {
                path: args["path"].as_str().unwrap_or(".").to_string(),
            },
            "subagent" => Self::Subagent {
                task: args["task"].as_str().unwrap_or("").to_string(),
                context: args["context"].as_str().map(|s| s.to_string()),
                allowed_tools: args["allowed_tools"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default(),
                isolated: args["isolated"].as_bool().unwrap_or(false),
            },
            _ => Self::Untyped {
                tool_name: tool_name.to_string(),
                args: args.clone(),
            },
        }
    }

    /// Convert back to untyped JSON (wire boundary encode).
    pub fn to_wire(&self) -> (String, serde_json::Value) {
        let name = self.tool_name().to_string();
        let args = match self {
            Self::ReadFile {
                path,
                start_line,
                end_line,
            } => {
                let mut m = serde_json::Map::new();
                m.insert("path".into(), serde_json::Value::String(path.clone()));
                if let Some(s) = start_line {
                    m.insert("start_line".into(), (*s).into());
                }
                if let Some(e) = end_line {
                    m.insert("end_line".into(), (*e).into());
                }
                serde_json::Value::Object(m)
            }
            Self::Untyped { args, .. } => args.clone(),
            // For other variants, serialize via serde
            other => serde_json::to_value(other).unwrap_or_default(),
        };
        (name, args)
    }

    /// Extract affected file paths (for blast radius / conflict analysis).
    pub fn affected_paths(&self) -> Vec<&str> {
        match self {
            Self::ReadFile { path, .. }
            | Self::EditFile { path, .. }
            | Self::WriteFile { path, .. }
            | Self::ListDir { path } => vec![path.as_str()],
            Self::Search { path: Some(p), .. } => vec![p.as_str()],
            _ => Vec::new(),
        }
    }
}

/// Typed approval request — replaces untyped JSON approval payloads internally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedApprovalRequest {
    /// Unique call identifier.
    pub call_id: String,
    /// Typed tool arguments.
    pub typed_args: TypedToolArgs,
    /// Risk assessment.
    pub risk: ApprovalRisk,
}

/// Risk level for approval decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalRisk {
    /// No risk — read-only operation.
    None,
    /// Low risk — reversible file edit.
    Low,
    /// Medium risk — shell command or external call.
    Medium,
    /// High risk — destructive or irreversible operation.
    High,
}

impl TypedApprovalRequest {
    /// Create from wire-format approval event.
    pub fn from_wire(call_id: &str, tool_name: &str, args: &serde_json::Value) -> Self {
        let typed_args = TypedToolArgs::from_wire(tool_name, args);
        let risk = match &typed_args {
            TypedToolArgs::ReadFile { .. }
            | TypedToolArgs::Search { .. }
            | TypedToolArgs::ListDir { .. } => ApprovalRisk::None,
            TypedToolArgs::EditFile { .. } | TypedToolArgs::WriteFile { .. } => ApprovalRisk::Low,
            TypedToolArgs::Bash { command, .. } => {
                if command.contains("rm ")
                    || command.contains("drop ")
                    || command.contains("--force")
                {
                    ApprovalRisk::High
                } else {
                    ApprovalRisk::Medium
                }
            }
            TypedToolArgs::Subagent { isolated, .. } => {
                if *isolated {
                    ApprovalRisk::Low
                } else {
                    ApprovalRisk::Medium
                }
            }
            TypedToolArgs::Untyped { .. } => ApprovalRisk::Medium,
        };
        Self {
            call_id: call_id.to_string(),
            typed_args,
            risk,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_wire_read_file() {
        let args = serde_json::json!({"path": "src/main.rs", "start_line": 1, "end_line": 50});
        let typed = TypedToolArgs::from_wire("read_file", &args);
        assert!(matches!(typed, TypedToolArgs::ReadFile { .. }));
        assert!(typed.is_read_only());
        assert!(!typed.is_file_mutation());
    }

    #[test]
    fn from_wire_unknown_tool_falls_back() {
        let args = serde_json::json!({"custom": true});
        let typed = TypedToolArgs::from_wire("my_plugin_tool", &args);
        assert!(matches!(typed, TypedToolArgs::Untyped { .. }));
        assert_eq!(typed.tool_name(), "my_plugin_tool");
    }

    #[test]
    fn approval_risk_assessment() {
        let req = TypedApprovalRequest::from_wire(
            "c1",
            "bash",
            &serde_json::json!({"command": "rm -rf /tmp/test"}),
        );
        assert_eq!(req.risk, ApprovalRisk::High);

        let req = TypedApprovalRequest::from_wire(
            "c2",
            "read_file",
            &serde_json::json!({"path": "foo.rs"}),
        );
        assert_eq!(req.risk, ApprovalRisk::None);
    }

    #[test]
    fn affected_paths_extraction() {
        let typed = TypedToolArgs::from_wire(
            "edit_file",
            &serde_json::json!({
                "path": "src/lib.rs",
                "old_text": "a",
                "new_text": "b"
            }),
        );
        assert_eq!(typed.affected_paths(), vec!["src/lib.rs"]);
    }
}
