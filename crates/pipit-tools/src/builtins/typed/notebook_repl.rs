//! Notebook Tool — Jupyter .ipynb editing.
//!
//! A .ipynb is just JSON. Parse, mutate, re-serialize.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::typed_tool::*;
use crate::{ToolContext, ToolError};

/// Notebook action.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum NotebookInput {
    /// Read notebook structure (cell list with types).
    Read { path: String },
    /// Insert a new cell.
    InsertCell {
        path: String,
        /// Index to insert at (0-based). Appends if omitted.
        index: Option<usize>,
        /// Cell type: code or markdown.
        cell_type: CellType,
        /// Cell content.
        content: String,
    },
    /// Edit an existing cell's content.
    EditCell {
        path: String,
        /// Cell index (0-based).
        index: usize,
        /// New content.
        content: String,
    },
    /// Delete a cell.
    DeleteCell {
        path: String,
        /// Cell index (0-based).
        index: usize,
    },
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CellType {
    Code,
    Markdown,
}

/// Notebook editing tool.
pub struct NotebookTool;

#[async_trait]
impl TypedTool for NotebookTool {
    type Input = NotebookInput;
    const NAME: &'static str = "notebook";
    const CAPABILITIES: CapabilitySet =
        CapabilitySet(CapabilitySet::FS_READ.0 | CapabilitySet::FS_WRITE.0);
    const PURITY: Purity = Purity::Mutating;

    fn describe() -> ToolCard {
        ToolCard {
            name: "notebook".into(),
            summary: "Read and edit Jupyter notebooks (.ipynb files)".into(),
            when_to_use: "When working with Jupyter notebooks. Read to see cell structure, insert/edit/delete cells.".into(),
            examples: vec![
                ToolExample {
                    description: "Read notebook".into(),
                    input: serde_json::json!({"action": "read", "path": "analysis.ipynb"}),
                },
                ToolExample {
                    description: "Add code cell".into(),
                    input: serde_json::json!({
                        "action": "insert_cell",
                        "path": "analysis.ipynb",
                        "cell_type": "code",
                        "content": "import pandas as pd\ndf = pd.read_csv('data.csv')"
                    }),
                },
            ],
            tags: vec!["notebook".into(), "jupyter".into(), "ipynb".into(), "cell".into()],
            purity: Purity::Mutating,
            capabilities: CapabilitySet::FS_READ.0 | CapabilitySet::FS_WRITE.0,
        }
    }

    async fn execute(
        &self,
        input: NotebookInput,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<TypedToolResult, ToolError> {
        match input {
            NotebookInput::Read { path } => {
                let full_path = ctx.project_root.join(&path);
                let content = tokio::fs::read_to_string(&full_path).await.map_err(|e| {
                    ToolError::ExecutionFailed(format!("Failed to read {path}: {e}"))
                })?;
                let nb: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
                    ToolError::ExecutionFailed(format!("Invalid notebook JSON: {e}"))
                })?;

                let cells = nb.get("cells").and_then(|c| c.as_array());
                let mut summary = Vec::new();
                if let Some(cells) = cells {
                    for (i, cell) in cells.iter().enumerate() {
                        let ct = cell
                            .get("cell_type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let source = cell
                            .get("source")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|l| l.as_str())
                                    .collect::<Vec<_>>()
                                    .join("")
                            })
                            .unwrap_or_default();
                        let preview = source.lines().next().unwrap_or("(empty)");
                        summary.push(format!("[{i}] {ct}: {preview}"));
                    }
                }
                Ok(TypedToolResult::text(format!(
                    "Notebook: {path}\n{} cells:\n{}",
                    summary.len(),
                    summary.join("\n")
                )))
            }

            NotebookInput::InsertCell {
                path,
                index,
                cell_type,
                content,
            } => {
                let full_path = ctx.project_root.join(&path);
                let file_content = tokio::fs::read_to_string(&full_path)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read: {e}")))?;
                let mut nb: serde_json::Value = serde_json::from_str(&file_content)
                    .map_err(|e| ToolError::ExecutionFailed(format!("Invalid JSON: {e}")))?;

                let ct_str = match cell_type {
                    CellType::Code => "code",
                    CellType::Markdown => "markdown",
                };
                let new_cell = serde_json::json!({
                    "cell_type": ct_str,
                    "source": content.lines().map(|l| format!("{l}\n")).collect::<Vec<_>>(),
                    "metadata": {},
                    "outputs": if ct_str == "code" { serde_json::json!([]) } else { serde_json::json!(null) },
                });

                if let Some(cells) = nb.get_mut("cells").and_then(|c| c.as_array_mut()) {
                    let idx = index.unwrap_or(cells.len());
                    cells.insert(idx.min(cells.len()), new_cell);
                }

                let out = serde_json::to_string_pretty(&nb)
                    .map_err(|e| ToolError::ExecutionFailed(format!("Serialize: {e}")))?;
                tokio::fs::write(&full_path, &out)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("Write: {e}")))?;

                Ok(
                    TypedToolResult::mutating(format!("Inserted {ct_str} cell in {path}"))
                        .with_artifact(ArtifactKind::FileModified {
                            path: path.clone(),
                            before_hash: None,
                            after_hash: None,
                        }),
                )
            }

            NotebookInput::EditCell {
                path,
                index,
                content,
            } => {
                let full_path = ctx.project_root.join(&path);
                let file_content = tokio::fs::read_to_string(&full_path)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("Read: {e}")))?;
                let mut nb: serde_json::Value = serde_json::from_str(&file_content)
                    .map_err(|e| ToolError::ExecutionFailed(format!("JSON: {e}")))?;

                if let Some(cells) = nb.get_mut("cells").and_then(|c| c.as_array_mut()) {
                    if index >= cells.len() {
                        return Err(ToolError::InvalidArgs(format!(
                            "Cell index {index} out of range"
                        )));
                    }
                    let source: Vec<serde_json::Value> = content
                        .lines()
                        .map(|l| serde_json::Value::String(format!("{l}\n")))
                        .collect();
                    cells[index]["source"] = serde_json::Value::Array(source);
                }

                let out = serde_json::to_string_pretty(&nb)
                    .map_err(|e| ToolError::ExecutionFailed(format!("Serialize: {e}")))?;
                tokio::fs::write(&full_path, &out)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("Write: {e}")))?;

                Ok(TypedToolResult::mutating(format!(
                    "Edited cell {index} in {path}"
                )))
            }

            NotebookInput::DeleteCell { path, index } => {
                let full_path = ctx.project_root.join(&path);
                let file_content = tokio::fs::read_to_string(&full_path)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("Read: {e}")))?;
                let mut nb: serde_json::Value = serde_json::from_str(&file_content)
                    .map_err(|e| ToolError::ExecutionFailed(format!("JSON: {e}")))?;

                if let Some(cells) = nb.get_mut("cells").and_then(|c| c.as_array_mut()) {
                    if index >= cells.len() {
                        return Err(ToolError::InvalidArgs(format!(
                            "Cell index {index} out of range"
                        )));
                    }
                    cells.remove(index);
                }

                let out = serde_json::to_string_pretty(&nb)
                    .map_err(|e| ToolError::ExecutionFailed(format!("Serialize: {e}")))?;
                tokio::fs::write(&full_path, &out)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("Write: {e}")))?;

                Ok(TypedToolResult::mutating(format!(
                    "Deleted cell {index} from {path}"
                )))
            }
        }
    }
}
