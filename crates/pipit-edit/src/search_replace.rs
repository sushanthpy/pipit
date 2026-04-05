use crate::{AppliedEdit, EditError, EditFormat, EditOp};
use crate::apply::atomic_write;
use std::path::{Path, PathBuf};

/// Search/Replace block format — default for Claude and Gemini.
///
/// Format:
/// ```text
/// path/to/file.rs
/// <<<<<<< SEARCH
/// old code
/// ======= REPLACE
/// new code
/// >>>>>>>
/// ```
pub struct SearchReplaceFormat;

impl EditFormat for SearchReplaceFormat {
    fn name(&self) -> &str {
        "search_replace"
    }

    fn prompt_instructions(&self) -> &str {
        r#"When you need to edit code, use SEARCH/REPLACE blocks:

path/to/file.ext
<<<<<<< SEARCH
exact lines to find in the file
======= REPLACE
replacement lines
>>>>>>>

Rules:
- The SEARCH block must match EXACTLY (including whitespace and indentation).
- Include enough context lines to uniquely identify the location.
- You can use multiple SEARCH/REPLACE blocks for the same file.
- To create a new file, use an empty SEARCH block.
- To delete code, use an empty REPLACE block.
"#
    }

    fn parse(&self, response: &str, known_files: &[PathBuf]) -> Result<Vec<EditOp>, EditError> {
        let mut ops = Vec::new();
        let lines: Vec<&str> = response.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            // Look for a filepath line followed by <<<<<<< SEARCH
            if i + 1 < lines.len() && lines[i + 1].starts_with("<<<<<<< SEARCH") {
                let path_line = lines[i].trim();
                let path = PathBuf::from(path_line);

                i += 2; // skip path and SEARCH marker

                // Collect SEARCH content until =======
                let mut search_lines = Vec::new();
                while i < lines.len()
                    && !lines[i].starts_with("======= REPLACE")
                    && !lines[i].starts_with("=======")
                {
                    search_lines.push(lines[i]);
                    i += 1;
                }

                if i < lines.len() {
                    i += 1; // skip ======= marker
                }

                // Collect REPLACE content until >>>>>>>
                let mut replace_lines = Vec::new();
                while i < lines.len() && !lines[i].starts_with(">>>>>>>") {
                    replace_lines.push(lines[i]);
                    i += 1;
                }

                if i < lines.len() {
                    i += 1; // skip >>>>>>> marker
                }

                let search = search_lines.join("\n");
                let replace = replace_lines.join("\n");

                if search.is_empty() && !replace.is_empty() {
                    // Create file
                    ops.push(EditOp::CreateFile {
                        path,
                        content: replace,
                    });
                } else {
                    ops.push(EditOp::SearchReplace {
                        path,
                        search,
                        replace,
                    });
                }
            } else {
                i += 1;
            }
        }

        Ok(ops)
    }

    fn apply(&self, op: &EditOp, root: &Path) -> Result<AppliedEdit, EditError> {
        match op {
            EditOp::SearchReplace {
                path,
                search,
                replace,
            } => {
                let abs_path = root.join(path);
                let content = std::fs::read_to_string(&abs_path)
                    .map_err(|_| EditError::FileNotFound(path.clone()))?;

                // Try exact match first
                if let Some(pos) = content.find(search.as_str()) {
                    let new_content = format!(
                        "{}{}{}",
                        &content[..pos],
                        replace,
                        &content[pos + search.len()..],
                    );
                    atomic_write(&abs_path, &new_content)?;
                    return Ok(make_applied_edit(path, &content, &new_content));
                }

                // Try fuzzy matching
                if let Some(new_content) = fuzzy_search_replace(&content, search, replace) {
                    atomic_write(&abs_path, &new_content)?;
                    return Ok(make_applied_edit(path, &content, &new_content));
                }

                Err(EditError::SearchNotFound {
                    path: path.clone(),
                    search: search.clone(),
                })
            }
            EditOp::CreateFile { path, content } => {
                let abs_path = root.join(path);
                if let Some(parent) = abs_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                atomic_write(&abs_path, content)?;
                Ok(AppliedEdit {
                    path: path.clone(),
                    diff: format!("+++ {}\n(new file)", path.display()),
                    lines_added: content.lines().count() as u32,
                    lines_removed: 0,
                    before_content: String::new(),
                    after_content: content.clone(),
                })
            }
            EditOp::DeleteFile { path } => {
                let abs_path = root.join(path);
                let before = std::fs::read_to_string(&abs_path).unwrap_or_default();
                std::fs::remove_file(&abs_path)?;
                Ok(AppliedEdit {
                    path: path.clone(),
                    diff: format!("--- {}\n(deleted)", path.display()),
                    lines_added: 0,
                    lines_removed: before.lines().count() as u32,
                    before_content: before,
                    after_content: String::new(),
                })
            }
            _ => Err(EditError::Other(
                "SearchReplaceFormat cannot apply this op type".to_string(),
            )),
        }
    }
}

/// Fuzzy matching: normalize whitespace and try progressively looser strategies.
fn fuzzy_search_replace(content: &str, search: &str, replace: &str) -> Option<String> {
    let content_lines: Vec<&str> = content.lines().collect();
    let search_lines: Vec<&str> = search.lines().collect();

    if search_lines.is_empty() {
        return None;
    }

    // Strategy: line-by-line fuzzy match (ignore leading/trailing whitespace)
    for start in 0..content_lines.len() {
        if start + search_lines.len() > content_lines.len() {
            break;
        }

        let matches = search_lines.iter().enumerate().all(|(j, search_line)| {
            content_lines[start + j].trim() == search_line.trim()
        });

        if matches {
            let end = start + search_lines.len();

            // Build indent mapping from matched search↔content line pairs.
            // Each pair maps a search indent level to the corresponding content indent level.
            // This handles cases where the LLM uses a different indent unit (e.g., 2-space)
            // than the content (e.g., 4-space) — a flat delta would fail in that case.
            let mut indent_pairs: Vec<(usize, usize)> = Vec::new();
            for j in 0..search_lines.len() {
                if !search_lines[j].trim().is_empty() {
                    let s_indent = detect_indent(search_lines[j]).len();
                    let c_indent = detect_indent(content_lines[start + j]).len();
                    indent_pairs.push((s_indent, c_indent));
                }
            }
            indent_pairs.sort_by_key(|p| p.0);
            indent_pairs.dedup_by_key(|p| p.0);

            let indent_char = if detect_indent(content_lines[start]).contains('\t') {
                '\t'
            } else {
                ' '
            };
            let mut result_lines: Vec<String> =
                content_lines[..start].iter().map(|s| s.to_string()).collect();

            // Apply replacement with indent mapping
            for replace_line in replace.lines() {
                let adjusted = if !replace_line.trim().is_empty() {
                    let r_indent = detect_indent(replace_line).len();
                    let new_indent_len = map_indent(r_indent, &indent_pairs);
                    let new_indent: String =
                        std::iter::repeat(indent_char).take(new_indent_len).collect();
                    format!("{}{}", new_indent, replace_line.trim_start())
                } else {
                    replace_line.to_string()
                };
                result_lines.push(adjusted);
            }

            result_lines.extend(content_lines[end..].iter().map(|s| s.to_string()));
            return Some(result_lines.join("\n"));
        }
    }

    None
}

/// Map a replacement indent level to the correct content indent level using
/// the search↔content indent lookup table. Falls back to flat delta for
/// indents not seen in the search block.
fn map_indent(replace_indent: usize, pairs: &[(usize, usize)]) -> usize {
    if pairs.is_empty() {
        return replace_indent;
    }

    // Exact match in the lookup table
    for &(s, c) in pairs {
        if s == replace_indent {
            return c;
        }
    }

    // Below minimum known indent: use flat delta from the minimum pair
    if replace_indent < pairs[0].0 {
        let delta = pairs[0].1 as isize - pairs[0].0 as isize;
        return (replace_indent as isize + delta).max(0) as usize;
    }

    // Above maximum known indent: use flat delta from the maximum pair
    let last = pairs[pairs.len() - 1];
    if replace_indent > last.0 {
        let delta = last.1 as isize - last.0 as isize;
        return (replace_indent as isize + delta).max(0) as usize;
    }

    // Between two known points: linear interpolation
    for i in 0..pairs.len() - 1 {
        let (s1, c1) = pairs[i];
        let (s2, c2) = pairs[i + 1];
        if replace_indent > s1 && replace_indent < s2 {
            let ratio = (replace_indent - s1) as f64 / (s2 - s1) as f64;
            let mapped = c1 as f64 + ratio * (c2 as f64 - c1 as f64);
            return mapped.round() as usize;
        }
    }

    replace_indent
}

fn detect_indent(line: &str) -> &str {
    let trimmed = line.trim_start();
    &line[..line.len() - trimmed.len()]
}

fn make_applied_edit(path: &Path, before: &str, after: &str) -> AppliedEdit {
    let diff = similar::TextDiff::from_lines(before, after);
    let unified = diff
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{}", path.display()), &format!("b/{}", path.display()))
        .to_string();

    let added = after.lines().count() as i64;
    let removed = before.lines().count() as i64;

    AppliedEdit {
        path: path.to_path_buf(),
        diff: unified,
        lines_added: added.max(0) as u32,
        lines_removed: removed.max(0) as u32,
        before_content: before.to_string(),
        after_content: after.to_string(),
    }
}
