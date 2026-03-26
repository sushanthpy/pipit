use crate::{AppliedEdit, DiffHunk, DiffLine, EditError, EditFormat, EditOp};
use crate::apply::atomic_write;
use std::path::{Path, PathBuf};

/// Unified diff format — preferred by GPT-4 and some other models.
pub struct UnifiedDiffFormat;

impl EditFormat for UnifiedDiffFormat {
    fn name(&self) -> &str {
        "udiff"
    }

    fn prompt_instructions(&self) -> &str {
        r#"When editing code, output unified diffs in fenced code blocks:

```diff
--- a/path/to/file.ext
+++ b/path/to/file.ext
@@ -START,COUNT +START,COUNT @@
 context line
-removed line
+added line
 context line
```

Rules:
- Use standard unified diff format.
- Include enough context lines for unambiguous matching.
- Prefix lines with ' ' (context), '-' (removed), '+' (added).
"#
    }

    fn parse(&self, response: &str, _known_files: &[PathBuf]) -> Result<Vec<EditOp>, EditError> {
        let mut ops = Vec::new();

        // Extract fenced diff blocks
        let blocks = extract_fenced_blocks(response, "diff");

        for block in blocks {
            let parsed = parse_unified_diff(&block)?;
            for file_diff in parsed {
                ops.push(EditOp::UnifiedDiff {
                    path: file_diff.path,
                    hunks: file_diff.hunks,
                });
            }
        }

        Ok(ops)
    }

    fn apply(&self, op: &EditOp, root: &Path) -> Result<AppliedEdit, EditError> {
        match op {
            EditOp::UnifiedDiff { path, hunks } => {
                let abs_path = root.join(path);
                let content = std::fs::read_to_string(&abs_path)
                    .map_err(|_| EditError::FileNotFound(path.clone()))?;

                let new_content = apply_hunks(&content, hunks)?;
                atomic_write(&abs_path, &new_content)?;

                let diff = similar::TextDiff::from_lines(&content, &new_content);
                let unified = diff
                    .unified_diff()
                    .context_radius(3)
                    .header(
                        &format!("a/{}", path.display()),
                        &format!("b/{}", path.display()),
                    )
                    .to_string();

                Ok(AppliedEdit {
                    path: path.clone(),
                    diff: unified,
                    lines_added: hunks
                        .iter()
                        .flat_map(|h| &h.lines)
                        .filter(|l| matches!(l, DiffLine::Added(_)))
                        .count() as u32,
                    lines_removed: hunks
                        .iter()
                        .flat_map(|h| &h.lines)
                        .filter(|l| matches!(l, DiffLine::Removed(_)))
                        .count() as u32,
                    before_content: content,
                    after_content: new_content,
                })
            }
            _ => Err(EditError::Other(
                "UnifiedDiffFormat cannot apply this op type".to_string(),
            )),
        }
    }
}

fn extract_fenced_blocks(text: &str, lang: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut in_block = false;
    let mut current = String::new();
    let fence_start = format!("```{}", lang);

    for line in text.lines() {
        if !in_block {
            if line.trim().starts_with(&fence_start) {
                in_block = true;
                current.clear();
            }
        } else if line.trim() == "```" {
            blocks.push(current.clone());
            in_block = false;
        } else {
            current.push_str(line);
            current.push('\n');
        }
    }

    blocks
}

struct FileDiff {
    path: PathBuf,
    hunks: Vec<DiffHunk>,
}

fn parse_unified_diff(diff_text: &str) -> Result<Vec<FileDiff>, EditError> {
    let mut files = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_hunks: Vec<DiffHunk> = Vec::new();
    let mut current_hunk: Option<DiffHunk> = None;

    for line in diff_text.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            // Save previous file
            if let Some(path) = current_path.take() {
                if let Some(hunk) = current_hunk.take() {
                    current_hunks.push(hunk);
                }
                files.push(FileDiff {
                    path,
                    hunks: std::mem::take(&mut current_hunks),
                });
            }
            current_path = Some(PathBuf::from(path.trim()));
        } else if line.starts_with("--- ") {
            // Skip the --- line
        } else if line.starts_with("@@ ") {
            if let Some(hunk) = current_hunk.take() {
                current_hunks.push(hunk);
            }
            current_hunk = Some(parse_hunk_header(line)?);
        } else if let Some(ref mut hunk) = current_hunk {
            if let Some(rest) = line.strip_prefix('+') {
                hunk.lines.push(DiffLine::Added(rest.to_string()));
            } else if let Some(rest) = line.strip_prefix('-') {
                hunk.lines.push(DiffLine::Removed(rest.to_string()));
            } else if let Some(rest) = line.strip_prefix(' ') {
                hunk.lines.push(DiffLine::Context(rest.to_string()));
            } else if !line.starts_with('\\') {
                hunk.lines.push(DiffLine::Context(line.to_string()));
            }
        }
    }

    // Save last file
    if let Some(path) = current_path {
        if let Some(hunk) = current_hunk {
            current_hunks.push(hunk);
        }
        files.push(FileDiff {
            path,
            hunks: current_hunks,
        });
    }

    Ok(files)
}

fn parse_hunk_header(line: &str) -> Result<DiffHunk, EditError> {
    // @@ -START,COUNT +START,COUNT @@
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 4 {
        return Err(EditError::ParseError(format!(
            "Invalid hunk header: {}",
            line
        )));
    }

    let old_range = parts[1].trim_start_matches('-');
    let new_range = parts[2].trim_start_matches('+');

    let (old_start, old_count) = parse_range(old_range)?;
    let (new_start, new_count) = parse_range(new_range)?;

    Ok(DiffHunk {
        old_start,
        old_count,
        new_start,
        new_count,
        lines: Vec::new(),
    })
}

fn parse_range(range: &str) -> Result<(u32, u32), EditError> {
    let parts: Vec<&str> = range.split(',').collect();
    let start = parts[0]
        .parse::<u32>()
        .map_err(|_| EditError::ParseError(format!("Invalid range: {}", range)))?;
    let count = if parts.len() > 1 {
        parts[1]
            .parse::<u32>()
            .map_err(|_| EditError::ParseError(format!("Invalid range: {}", range)))?
    } else {
        1
    };
    Ok((start, count))
}

fn apply_hunks(content: &str, hunks: &[DiffHunk]) -> Result<String, EditError> {
    let lines: Vec<&str> = content.lines().collect();
    let mut result = Vec::new();
    let mut line_idx = 0;

    for hunk in hunks {
        let hunk_start = (hunk.old_start as usize).saturating_sub(1);

        // Copy lines before this hunk
        while line_idx < hunk_start && line_idx < lines.len() {
            result.push(lines[line_idx].to_string());
            line_idx += 1;
        }

        // Apply hunk
        for diff_line in &hunk.lines {
            match diff_line {
                DiffLine::Context(_) => {
                    if line_idx < lines.len() {
                        result.push(lines[line_idx].to_string());
                        line_idx += 1;
                    }
                }
                DiffLine::Removed(_) => {
                    line_idx += 1; // skip the old line
                }
                DiffLine::Added(text) => {
                    result.push(text.clone());
                }
            }
        }
    }

    // Copy remaining lines
    while line_idx < lines.len() {
        result.push(lines[line_idx].to_string());
        line_idx += 1;
    }

    Ok(result.join("\n"))
}
