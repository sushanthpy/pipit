use crate::apply::atomic_write;
use crate::{AppliedEdit, EditError, EditFormat, EditOp};
use std::path::{Path, PathBuf};

/// Whole-file format — for small files or weak models.
/// LLM outputs complete file contents in a fenced block.
pub struct WholeFileFormat;

impl EditFormat for WholeFileFormat {
    fn name(&self) -> &str {
        "whole_file"
    }

    fn prompt_instructions(&self) -> &str {
        r#"When editing code, output the complete file contents in a fenced code block with the filename:

```path/to/file.ext
complete file contents here
```

Rules:
- Include the ENTIRE file content, not just changed parts.
- The filename goes on the same line as the opening fence.
"#
    }

    fn parse(&self, response: &str, _known_files: &[PathBuf]) -> Result<Vec<EditOp>, EditError> {
        let mut ops = Vec::new();
        let mut in_block = false;
        let mut current_path: Option<PathBuf> = None;
        let mut current_content = String::new();

        for line in response.lines() {
            if !in_block {
                // Look for ```path/to/file.ext
                if line.starts_with("```") && line.len() > 3 && !line[3..].trim().is_empty() {
                    let path_str = line[3..].trim();
                    // Skip language-only fences like ```rust
                    if path_str.contains('/') || path_str.contains('.') {
                        let clean = path_str.trim_end_matches('`');
                        current_path = Some(PathBuf::from(clean));
                        current_content.clear();
                        in_block = true;
                    }
                }
            } else if line.trim() == "```" {
                if let Some(path) = current_path.take() {
                    ops.push(EditOp::WholeFile {
                        path,
                        content: current_content.clone(),
                    });
                }
                in_block = false;
            } else {
                if !current_content.is_empty() {
                    current_content.push('\n');
                }
                current_content.push_str(line);
            }
        }

        Ok(ops)
    }

    fn apply(&self, op: &EditOp, root: &Path) -> Result<AppliedEdit, EditError> {
        match op {
            EditOp::WholeFile { path, content } => {
                let abs_path = root.join(path);
                let before = std::fs::read_to_string(&abs_path).unwrap_or_default();

                if let Some(parent) = abs_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }

                atomic_write(&abs_path, content)?;

                let diff = similar::TextDiff::from_lines(&before, content);
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
                    lines_added: content.lines().count() as u32,
                    lines_removed: before.lines().count() as u32,
                    before_content: before,
                    after_content: content.clone(),
                })
            }
            _ => Err(EditError::Other(
                "WholeFileFormat cannot apply this op type".to_string(),
            )),
        }
    }
}
