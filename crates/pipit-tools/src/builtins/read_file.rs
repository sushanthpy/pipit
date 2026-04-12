use crate::builtins::extended::production_tools::FileStateCache;
use crate::{Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use once_cell::sync::Lazy;
use pipit_config::ApprovalMode;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Global file state cache shared between ReadFileTool and WriteFileTool.
pub static FILE_STATE_CACHE: Lazy<FileStateCache> = Lazy::new(FileStateCache::new);

/// Known image extensions and their MIME types.
fn image_mime_type(ext: &str) -> Option<&'static str> {
    match ext {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "svg" => Some("image/svg+xml"),
        "bmp" => Some("image/bmp"),
        "ico" => Some("image/x-icon"),
        _ => None,
    }
}

/// Read file contents with optional line range.
pub struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path relative to project root"
                },
                "start_line": {
                    "type": "integer",
                    "description": "Starting line number (1-indexed, optional)"
                },
                "end_line": {
                    "type": "integer",
                    "description": "Ending line number (1-indexed, inclusive, optional)"
                }
            },
            "required": ["path"]
        })
    }

    fn description(&self) -> &str {
        "Read the contents of a file. Returns line-numbered output. For large files, output is capped and a continuation hint is shown. Use start_line/end_line for targeted reads."
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

        let abs_path = ctx.project_root.join(path_str);

        // Security: prevent path traversal
        let canonical = abs_path
            .canonicalize()
            .map_err(|e| ToolError::ExecutionFailed(format!("Cannot resolve path: {}", e)))?;

        let project_canonical = ctx.project_root.canonicalize().map_err(|e| {
            ToolError::ExecutionFailed(format!("Cannot resolve project root: {}", e))
        })?;

        if !canonical.starts_with(&project_canonical) {
            return Err(ToolError::PermissionDenied(
                "Path is outside project root".to_string(),
            ));
        }

        // Pre-flight: check file size before reading
        let metadata = tokio::fs::metadata(&canonical)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Cannot stat file: {}", e)))?;
        let file_size = metadata.len();
        const MAX_READ_BYTES: u64 = 1_048_576; // 1MB

        if file_size > MAX_READ_BYTES {
            return Ok(ToolResult::text(format!(
                "File is {:.1}MB ({} bytes). Too large to read in full.\n\
                 Use start_line/end_line parameters to read specific sections, \
                 or use `grep` to search for relevant content.",
                file_size as f64 / 1_048_576.0,
                file_size
            )));
        }

        // Image file detection — return base64-encoded content with MIME type
        if let Some(ext) = canonical.extension().and_then(|e| e.to_str()) {
            if let Some(mime) = image_mime_type(&ext.to_lowercase()) {
                const MAX_IMAGE_BYTES: u64 = 5_242_880; // 5MB for images
                if file_size > MAX_IMAGE_BYTES {
                    return Ok(ToolResult::text(format!(
                        "Image file is {:.1}MB — too large to embed (max 5MB).\n\
                         Path: {path_str}\nType: {mime}",
                        file_size as f64 / 1_048_576.0,
                    )));
                }
                let raw_bytes = tokio::fs::read(&canonical)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("Cannot read image: {}", e)))?;
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&raw_bytes);
                return Ok(ToolResult {
                    content: format!(
                        "[Image: {path_str} ({mime}, {:.1}KB)]\ndata:{mime};base64,{b64}",
                        raw_bytes.len() as f64 / 1024.0
                    ),
                    display: None,
                    mutated: false,
                    content_bytes: raw_bytes.len(),
                    artifacts: Vec::new(),
                    edits: Vec::new(),
                });
            }
        }

        let content = tokio::fs::read_to_string(&canonical)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Cannot read file: {}", e)))?;

        // Record content hash for stale-write detection
        FILE_STATE_CACHE.record(&canonical, &content);

        let start_line = args["start_line"].as_u64().map(|n| n as usize);
        let end_line = args["end_line"].as_u64().map(|n| n as usize);

        /// Maximum lines returned per read call.  Keeps each tool result
        /// small enough that the model can read many files per turn without
        /// overflowing context.  Mirrors pi's ~2000-line / 50 KB ceiling.
        const MAX_READ_LINES: usize = 2000;
        /// Hard byte ceiling — avoids one huge minified file blowing context.
        const MAX_OUTPUT_BYTES: usize = 50 * 1024; // 50 KB

        let all_lines: Vec<&str> = content.lines().collect();
        let total_lines = all_lines.len();

        let start_idx = match start_line {
            Some(s) => s.saturating_sub(1).min(total_lines),
            None => 0,
        };
        let end_idx = match end_line {
            Some(e) => e.min(total_lines),
            None => total_lines,
        };

        // Enforce per-read line + byte limits
        let mut capped_end = end_idx;
        let mut byte_limited = false;
        {
            let mut bytes = 0usize;
            for i in start_idx..end_idx {
                if (i - start_idx) >= MAX_READ_LINES {
                    capped_end = i;
                    break;
                }
                bytes += all_lines[i].len() + 1; // +1 for newline
                if bytes > MAX_OUTPUT_BYTES {
                    capped_end = i + 1;
                    byte_limited = true;
                    break;
                }
            }
        }

        let selected: Vec<String> = all_lines[start_idx..capped_end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>4} | {}", start_idx + i + 1, line))
            .collect();
        let mut result = selected.join("\n");

        // Append continuation hint when truncated
        if capped_end < end_idx {
            let shown_start = start_idx + 1;
            let shown_end = capped_end;
            let next_start = capped_end + 1;
            if byte_limited {
                result.push_str(&format!(
                    "\n\n[Showing lines {}-{} of {} (50KB limit). Use start_line={} to continue.]",
                    shown_start, shown_end, total_lines, next_start
                ));
            } else {
                result.push_str(&format!(
                    "\n\n[Showing lines {}-{} of {}. Use start_line={} to continue.]",
                    shown_start, shown_end, total_lines, next_start
                ));
            }
        }

        Ok(ToolResult::text(result))
    }
}
