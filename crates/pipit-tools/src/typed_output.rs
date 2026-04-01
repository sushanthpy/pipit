//! Typed Artifact Outputs for Tools (Tool/Skill Task 3)
//!
//! Replaces flat-text ToolResult with a sum type that preserves structure.
//! Large payloads become ArtifactRef(hash) stored in the blob store.
//! Structured outputs plug directly into the evidence pipeline.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// A typed tool output that preserves structure for verification and reuse.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TypedOutput {
    /// Plain text output (most common).
    Text {
        content: String,
    },
    /// Structured JSON output.
    Json {
        data: serde_json::Value,
        /// Optional JSON schema the data conforms to.
        schema_name: Option<String>,
    },
    /// A file diff (patch).
    Patch {
        path: String,
        diff: String,
        lines_added: u32,
        lines_removed: u32,
    },
    /// Reference to a large artifact in the blob store.
    ArtifactRef {
        /// Content hash in the blob store.
        hash: String,
        /// Original size in bytes.
        size: usize,
        /// MIME type.
        mime: String,
        /// One-line summary for inline display.
        summary: String,
    },
    /// Tabular data.
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    /// Execution trace (for shell commands, test runs, etc.).
    Trace {
        command: String,
        stdout: String,
        stderr: String,
        exit_code: Option<i32>,
        duration_ms: u64,
    },
    /// Multiple outputs from a single tool call.
    Multi {
        items: Vec<TypedOutput>,
    },
}

impl TypedOutput {
    /// Get a human-readable text representation for the LLM context.
    pub fn as_context_text(&self) -> String {
        match self {
            TypedOutput::Text { content } => content.clone(),
            TypedOutput::Json { data, .. } => {
                serde_json::to_string_pretty(data).unwrap_or_else(|_| format!("{:?}", data))
            }
            TypedOutput::Patch {
                path,
                diff,
                lines_added,
                lines_removed,
            } => format!(
                "Patch for {}: +{} -{}\n{}",
                path, lines_added, lines_removed, diff
            ),
            TypedOutput::ArtifactRef {
                hash,
                size,
                summary,
                ..
            } => format!(
                "[Stored artifact: {} bytes, hash={}]\n{}",
                size,
                &hash[..hash.len().min(12)],
                summary
            ),
            TypedOutput::Table { headers, rows } => {
                let mut out = headers.join(" | ");
                out.push('\n');
                out.push_str(&"-".repeat(out.len()));
                out.push('\n');
                for row in rows {
                    out.push_str(&row.join(" | "));
                    out.push('\n');
                }
                out
            }
            TypedOutput::Trace {
                command,
                stdout,
                stderr,
                exit_code,
                duration_ms,
            } => {
                let mut out = format!("$ {} ({}ms", command, duration_ms);
                if let Some(code) = exit_code {
                    out.push_str(&format!(", exit={}", code));
                }
                out.push_str(")\n");
                if !stdout.is_empty() {
                    out.push_str(stdout);
                }
                if !stderr.is_empty() {
                    out.push_str("\n[stderr]\n");
                    out.push_str(stderr);
                }
                out
            }
            TypedOutput::Multi { items } => items
                .iter()
                .map(|i| i.as_context_text())
                .collect::<Vec<_>>()
                .join("\n---\n"),
        }
    }

    /// Get the size in bytes of the output content.
    pub fn content_size(&self) -> usize {
        match self {
            TypedOutput::Text { content } => content.len(),
            TypedOutput::Json { data, .. } => {
                serde_json::to_string(data).map(|s| s.len()).unwrap_or(0)
            }
            TypedOutput::Patch { diff, .. } => diff.len(),
            TypedOutput::ArtifactRef { size, .. } => *size,
            TypedOutput::Table { headers, rows } => {
                headers.iter().map(|h| h.len()).sum::<usize>()
                    + rows.iter().flat_map(|r| r.iter().map(|c| c.len())).sum::<usize>()
            }
            TypedOutput::Trace {
                stdout, stderr, ..
            } => stdout.len() + stderr.len(),
            TypedOutput::Multi { items } => items.iter().map(|i| i.content_size()).sum(),
        }
    }

    /// Whether this output should be stored in the blob store.
    pub fn should_store_as_blob(&self, threshold: usize) -> bool {
        self.content_size() > threshold
    }

    /// Convert to an ArtifactRef (for blob store storage).
    pub fn to_artifact_ref(&self, hash: String, summary: String) -> TypedOutput {
        TypedOutput::ArtifactRef {
            hash,
            size: self.content_size(),
            mime: self.infer_mime(),
            summary,
        }
    }

    fn infer_mime(&self) -> String {
        match self {
            TypedOutput::Json { .. } => "application/json".to_string(),
            TypedOutput::Patch { .. } => "text/x-diff".to_string(),
            TypedOutput::Table { .. } => "text/csv".to_string(),
            TypedOutput::Trace { .. } => "text/x-trace".to_string(),
            _ => "text/plain".to_string(),
        }
    }
}

/// Extended ToolResult that carries both the legacy text representation
/// and the typed output for downstream use.
#[derive(Debug, Clone)]
pub struct RichToolResult {
    /// Legacy text content (for backward compatibility).
    pub content: String,
    /// Typed output (for structured processing).
    pub typed: TypedOutput,
    /// Whether this tool mutated the project.
    pub mutated: bool,
    /// TUI display hint.
    pub display: Option<crate::ToolDisplay>,
}

impl RichToolResult {
    /// Create from a legacy ToolResult (wraps text as TypedOutput::Text).
    pub fn from_legacy(result: crate::ToolResult) -> Self {
        Self {
            content: result.content.clone(),
            typed: TypedOutput::Text {
                content: result.content,
            },
            mutated: result.mutated,
            display: result.display,
        }
    }

    /// Create with a typed output.
    pub fn with_typed(typed: TypedOutput, mutated: bool) -> Self {
        let content = typed.as_context_text();
        Self {
            content,
            typed,
            mutated,
            display: None,
        }
    }

    /// Convert to legacy ToolResult (for backward compatibility).
    pub fn to_legacy(&self) -> crate::ToolResult {
        crate::ToolResult {
            content: self.content.clone(),
            display: self.display.clone(),
            mutated: self.mutated,
            content_bytes: self.content.len(),
        }
    }
}
