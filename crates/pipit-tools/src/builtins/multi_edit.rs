//! MultiEdit Tool
//!
//! Applies multiple search/replace edits to the same file atomically.
//! All edits succeed or none do. Conflict detection for overlapping ranges.
//!
//! This reduces N tool calls to 1 for coordinated changes (e.g., updating
//! an import AND the usage site), preventing partial-edit file corruption.

use crate::{Tool, ToolContext, ToolDisplay, ToolError, ToolResult};
use async_trait::async_trait;
use pipit_config::ApprovalMode;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Apply multiple search/replace edits to a single file atomically.
pub struct MultiEditTool;

#[async_trait]
impl Tool for MultiEditTool {
    fn name(&self) -> &str {
        "multi_edit_file"
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path relative to project root"
                },
                "edits": {
                    "type": "array",
                    "description": "Array of search/replace edit operations to apply atomically",
                    "items": {
                        "type": "object",
                        "properties": {
                            "search": {
                                "type": "string",
                                "description": "The exact text to search for"
                            },
                            "replace": {
                                "type": "string",
                                "description": "The replacement text"
                            }
                        },
                        "required": ["search", "replace"]
                    },
                    "minItems": 1
                }
            },
            "required": ["path", "edits"]
        })
    }

    fn description(&self) -> &str {
        "Apply multiple search/replace edits to a single file atomically. \
         All edits succeed together or none are applied. Edits are applied in \
         reverse order (last edit first) to maintain correct offsets. \
         Use this instead of multiple edit_file calls when making coordinated changes."
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let path_str = args["path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArgs("missing 'path'".to_string()))?;

        let edits = args["edits"]
            .as_array()
            .ok_or_else(|| ToolError::InvalidArgs("missing 'edits' array".to_string()))?;

        if edits.is_empty() {
            return Err(ToolError::InvalidArgs(
                "'edits' array must not be empty".to_string(),
            ));
        }

        let abs_path = ctx.project_root.join(path_str);

        // Security: prevent path traversal
        let project_canonical = ctx.project_root.canonicalize().map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "Cannot canonicalize project root '{}': {}",
                ctx.project_root.display(),
                e
            ))
        })?;

        if abs_path.exists() {
            let canonical = abs_path.canonicalize().map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "Cannot canonicalize '{}': {}",
                    abs_path.display(),
                    e
                ))
            })?;
            if !canonical.starts_with(&project_canonical) {
                return Err(ToolError::PermissionDenied(format!(
                    "Path '{}' is outside the project root",
                    path_str
                )));
            }
        } else {
            return Err(ToolError::ExecutionFailed(format!(
                "File not found: {}",
                path_str
            )));
        }

        // Read the original file
        let original_content = std::fs::read_to_string(&abs_path).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read '{}': {}", path_str, e))
        })?;

        // Parse and validate all edits
        let mut edit_ops: Vec<EditOp> = Vec::with_capacity(edits.len());
        for (i, edit) in edits.iter().enumerate() {
            let search = edit["search"]
                .as_str()
                .ok_or_else(|| ToolError::InvalidArgs(format!("edit[{}] missing 'search'", i)))?;
            let replace = edit["replace"]
                .as_str()
                .ok_or_else(|| ToolError::InvalidArgs(format!("edit[{}] missing 'replace'", i)))?;

            if search.is_empty() {
                return Err(ToolError::InvalidArgs(format!(
                    "edit[{}] 'search' must not be empty (use write_file for new files)",
                    i
                )));
            }

            // Find the search text in the content
            let offset = original_content.find(search);
            match offset {
                Some(start) => {
                    edit_ops.push(EditOp {
                        index: i,
                        start,
                        end: start + search.len(),
                        search: search.to_string(),
                        replace: replace.to_string(),
                        is_fuzzy: false,
                    });
                }
                None => {
                    // Try whitespace-normalized matching as fallback
                    let normalized_content = normalize_whitespace(&original_content);
                    let normalized_search = normalize_whitespace(search);
                    match normalized_content.find(&normalized_search) {
                        Some(norm_start) => {
                            // Find the corresponding position in original content
                            // by counting characters up to the normalized offset
                            let start = find_original_offset(&original_content, norm_start);
                            let end = find_original_offset(
                                &original_content,
                                norm_start + normalized_search.len(),
                            );
                            edit_ops.push(EditOp {
                                index: i,
                                start,
                                end,
                                search: search.to_string(),
                                replace: replace.to_string(),
                                is_fuzzy: true,
                            });
                        }
                        None => {
                            return Err(ToolError::ExecutionFailed(format!(
                                "edit[{}]: search text not found in '{}'. \
                                 Make sure the search text matches exactly.",
                                i, path_str
                            )));
                        }
                    }
                }
            }
        }

        // Check for overlapping ranges (conflict detection)
        // Sort by start position for overlap checking
        edit_ops.sort_by_key(|e| e.start);
        for i in 0..edit_ops.len() - 1 {
            // Add safety margin for fuzzy matches (imprecise offsets)
            let margin = if edit_ops[i].is_fuzzy || edit_ops[i + 1].is_fuzzy {
                // One average line length of margin (~80 chars) for fuzzy offsets
                80
            } else {
                0
            };
            if edit_ops[i].end + margin > edit_ops[i + 1].start {
                return Err(ToolError::ExecutionFailed(format!(
                    "Overlapping edits: edit[{}] ({}..{}) overlaps with edit[{}] ({}..{}). \
                     Split into separate multi_edit_file calls.",
                    edit_ops[i].index,
                    edit_ops[i].start,
                    edit_ops[i].end,
                    edit_ops[i + 1].index,
                    edit_ops[i + 1].start,
                    edit_ops[i + 1].end,
                )));
            }
        }

        // Apply edits in reverse order (highest offset first) to avoid offset invalidation
        edit_ops.sort_by(|a, b| b.start.cmp(&a.start));

        let mut result_content = original_content.clone();
        let mut applied_count = 0;

        for edit in &edit_ops {
            result_content.replace_range(edit.start..edit.end, &edit.replace);
            applied_count += 1;
        }

        // Atomic write: write to temp file, then rename
        let dir = abs_path.parent().ok_or_else(|| {
            ToolError::ExecutionFailed("Cannot determine parent directory".to_string())
        })?;
        let temp_path = dir.join(format!(".pipit-multi-edit-{}", std::process::id()));
        std::fs::write(&temp_path, &result_content)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to write temp file: {}", e)))?;
        std::fs::rename(&temp_path, &abs_path).map_err(|e| {
            // Clean up temp file on failure
            let _ = std::fs::remove_file(&temp_path);
            ToolError::ExecutionFailed(format!(
                "Failed to atomically replace '{}': {}",
                path_str, e
            ))
        })?;

        // Generate diff
        let diff = generate_multi_diff(path_str, &original_content, &result_content);

        Ok(ToolResult {
            content: format!(
                "Applied {} edit(s) to '{}' atomically.",
                applied_count, path_str
            ),
            display: Some(ToolDisplay::Diff {
                path: path_str.into(),
                diff,
            }),
            mutated: true,
            content_bytes: result_content.len(),
            artifacts: Vec::new(),
            edits: Vec::new(),
        })
    }
}

/// A parsed and validated edit operation with its offset range.
struct EditOp {
    index: usize,
    start: usize,
    end: usize,
    search: String,
    replace: String,
    is_fuzzy: bool,
}

/// Normalize whitespace: collapse runs of whitespace into single spaces.
fn normalize_whitespace(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut prev_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                result.push(' ');
                prev_ws = true;
            }
        } else {
            result.push(ch);
            prev_ws = false;
        }
    }
    result
}

/// Map a position in normalized text back to the original text.
fn find_original_offset(original: &str, normalized_pos: usize) -> usize {
    let mut norm_count = 0;
    let mut prev_ws = false;
    for (i, ch) in original.char_indices() {
        if norm_count >= normalized_pos {
            return i;
        }
        if ch.is_whitespace() {
            if !prev_ws {
                norm_count += 1;
                prev_ws = true;
            }
        } else {
            norm_count += 1;
            prev_ws = false;
        }
    }
    original.len()
}

/// Generate a unified diff between original and result content.
fn generate_multi_diff(path: &str, original: &str, result: &str) -> String {
    let original_lines: Vec<&str> = original.lines().collect();
    let result_lines: Vec<&str> = result.lines().collect();

    let mut diff = format!("--- a/{}\n+++ b/{}\n", path, path);

    // Simple line-by-line diff
    let max_lines = original_lines.len().max(result_lines.len());
    let mut i = 0;
    while i < max_lines {
        let orig_line = original_lines.get(i).copied().unwrap_or("");
        let new_line = result_lines.get(i).copied().unwrap_or("");
        if orig_line != new_line {
            // Find the extent of the changed block
            let mut j = i;
            while j < max_lines {
                let ol = original_lines.get(j).copied().unwrap_or("");
                let nl = result_lines.get(j).copied().unwrap_or("");
                if ol == nl {
                    break;
                }
                j += 1;
            }

            diff.push_str(&format!(
                "@@ -{},{} +{},{} @@\n",
                i + 1,
                j - i,
                i + 1,
                j - i
            ));
            for k in i..j {
                if let Some(line) = original_lines.get(k) {
                    diff.push_str(&format!("-{}\n", line));
                }
            }
            for k in i..j {
                if let Some(line) = result_lines.get(k) {
                    diff.push_str(&format!("+{}\n", line));
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }

    diff
}
