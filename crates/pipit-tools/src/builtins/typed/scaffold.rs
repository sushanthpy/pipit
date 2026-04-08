//! Project Scaffolding Tool — batch directory + file generation.
//!
//! Creates an entire project directory structure in a single tool call.
//! This avoids the 50+ sequential `write_file` calls otherwise required
//! to bootstrap a new project from a requirements specification.
//!
//! Schema: `{ project_root: string, files: [{path, content}], directories: [string] }`
//! Purity: Destructive (creates many files at once — requires approval).

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

use crate::typed_tool::*;
use crate::{ToolContext, ToolDisplay, ToolError};

/// Input schema for the scaffold_project tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScaffoldInput {
    /// Root directory for the project (relative to workspace or absolute).
    /// Created if it doesn't exist.
    pub project_root: String,

    /// Directories to create (relative to project_root).
    /// Parent directories are created automatically.
    /// Example: ["src/components", "src/services", "tests", "docs"]
    #[serde(default)]
    pub directories: Vec<String>,

    /// Files to create with their content (relative to project_root).
    /// Parent directories are created automatically.
    pub files: Vec<FileSpec>,
}

/// A file to create as part of the scaffold.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FileSpec {
    /// File path relative to project_root.
    /// Example: "src/main.rs", "package.json", "docker/Dockerfile"
    pub path: String,

    /// File content. Can be empty string for placeholder files.
    pub content: String,
}

/// The scaffold_project tool.
pub struct ScaffoldProjectTool;

#[async_trait]
impl TypedTool for ScaffoldProjectTool {
    type Input = ScaffoldInput;

    const NAME: &'static str = "scaffold_project";
    const CAPABILITIES: CapabilitySet = CapabilitySet::FS_WRITE;
    const PURITY: Purity = Purity::Destructive;

    fn describe() -> ToolCard {
        ToolCard {
            name: Self::NAME.to_string(),
            summary:
                "Create an entire project directory structure with files in a single operation."
                    .to_string(),
            when_to_use:
                "Use when you need to create a new project, module, or multi-file structure. \
                          Much more efficient than calling write_file repeatedly."
                    .to_string(),
            examples: vec![ToolExample {
                description: "Create a Rust project".into(),
                input: serde_json::json!({
                    "project_root": "my-app",
                    "directories": ["src", "tests"],
                    "files": [{"path": "src/main.rs", "content": "fn main() {}"}]
                }),
            }],
            tags: vec![
                "scaffolding".into(),
                "project".into(),
                "batch".into(),
                "create".into(),
            ],
            purity: Self::PURITY,
            capabilities: Self::CAPABILITIES.0,
        }
    }

    async fn execute(
        &self,
        input: ScaffoldInput,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<TypedToolResult, ToolError> {
        let project_root = resolve_project_root(&input.project_root, ctx)?;

        // Validate: no path traversal in any file/directory path
        for dir in &input.directories {
            validate_relative_path(dir)?;
        }
        for file in &input.files {
            validate_relative_path(&file.path)?;
        }

        // Limit: prevent accidental massive scaffolds
        if input.files.len() > 500 {
            return Err(ToolError::InvalidArgs(format!(
                "Too many files ({}). Maximum is 500 per scaffold call.",
                input.files.len()
            )));
        }
        let total_bytes: usize = input.files.iter().map(|f| f.content.len()).sum();
        if total_bytes > 10_000_000 {
            return Err(ToolError::InvalidArgs(format!(
                "Total content size ({} bytes) exceeds 10MB limit.",
                total_bytes
            )));
        }

        // Phase 1: Create the project root
        std::fs::create_dir_all(&project_root).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to create project root: {}", e))
        })?;

        // Phase 2: Create directories
        let mut dirs_created = 0u32;
        for dir in &input.directories {
            let full_path = project_root.join(dir);
            if !full_path.exists() {
                std::fs::create_dir_all(&full_path).map_err(|e| {
                    ToolError::ExecutionFailed(format!("Failed to create directory {}: {}", dir, e))
                })?;
                dirs_created += 1;
            }
        }

        // Phase 3: Create files (atomic: write to temp then rename)
        let mut files_created = 0u32;
        let mut files_skipped = 0u32;
        let mut artifacts = Vec::new();
        let mut edits = Vec::new();

        for file_spec in &input.files {
            let full_path = project_root.join(&file_spec.path);

            // Create parent directories
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    ToolError::ExecutionFailed(format!(
                        "Failed to create parent dir for {}: {}",
                        file_spec.path, e
                    ))
                })?;
            }

            // Skip if file already exists (don't overwrite)
            if full_path.exists() {
                files_skipped += 1;
                continue;
            }

            // Atomic write: temp file in same directory then rename
            let dir = full_path.parent().unwrap_or(&project_root);
            let tmp = match tempfile::NamedTempFile::new_in(dir) {
                Ok(t) => t,
                Err(e) => {
                    // Fallback to direct write if tempfile fails
                    std::fs::write(&full_path, &file_spec.content).map_err(|e| {
                        ToolError::ExecutionFailed(format!(
                            "Failed to write {}: {}",
                            file_spec.path, e
                        ))
                    })?;
                    files_created += 1;
                    artifacts.push(ArtifactKind::FileModified {
                        path: file_spec.path.clone(),
                        before_hash: None,
                        after_hash: None,
                    });
                    continue;
                }
            };
            std::fs::write(tmp.path(), &file_spec.content).map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "Failed to write temp for {}: {}",
                    file_spec.path, e
                ))
            })?;

            // Persist: rename temp file to target path
            if let Err(_) = tmp.persist(&full_path) {
                // persist failed (cross-device?), fall back to copy
                std::fs::write(&full_path, &file_spec.content).map_err(|e| {
                    ToolError::ExecutionFailed(format!("Failed to write {}: {}", file_spec.path, e))
                })?;
            }

            files_created += 1;
            artifacts.push(ArtifactKind::FileModified {
                path: file_spec.path.clone(),
                before_hash: None,
                after_hash: None,
            });
            edits.push(RealizedEdit {
                path: full_path,
                before_hash: None,
                after_hash: None,
                hunks: 1,
            });
        }

        // Build summary
        let summary = format!(
            "Scaffolded project at {}:\n  {} directories created\n  {} files created\n  {} files skipped (already exist)",
            project_root.display(),
            dirs_created,
            files_created,
            files_skipped,
        );

        let mut result = TypedToolResult::mutating(summary);
        for artifact in artifacts {
            result = result.with_artifact(artifact);
        }
        for edit in edits {
            result = result.with_edit(edit);
        }
        Ok(result)
    }
}

/// Resolve project_root to an absolute path, anchored to the workspace.
fn resolve_project_root(project_root: &str, ctx: &ToolContext) -> Result<PathBuf, ToolError> {
    let path = Path::new(project_root);
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(ctx.project_root.join(path))
    }
}

/// Validate that a relative path doesn't escape the project root.
fn validate_relative_path(path: &str) -> Result<(), ToolError> {
    if path.contains("..") {
        return Err(ToolError::InvalidArgs(format!(
            "Path traversal not allowed: {}",
            path
        )));
    }
    if path.starts_with('/') {
        return Err(ToolError::InvalidArgs(format!(
            "Absolute paths not allowed in files/directories: {}. Use relative paths.",
            path
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_path_rejects_traversal() {
        assert!(validate_relative_path("../escape").is_err());
        assert!(validate_relative_path("foo/../../etc/passwd").is_err());
        assert!(validate_relative_path("/absolute/path").is_err());
    }

    #[test]
    fn validate_path_accepts_normal() {
        assert!(validate_relative_path("src/main.rs").is_ok());
        assert!(validate_relative_path("docker/Dockerfile").is_ok());
        assert!(validate_relative_path("package.json").is_ok());
    }

    #[tokio::test]
    async fn scaffold_creates_structure() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ToolContext::new(
            tmp.path().to_path_buf(),
            pipit_config::ApprovalMode::FullAuto,
        );

        let input = ScaffoldInput {
            project_root: "myapp".into(),
            directories: vec!["src".into(), "tests".into(), "docs".into()],
            files: vec![
                FileSpec {
                    path: "src/main.rs".into(),
                    content: "fn main() {}\n".into(),
                },
                FileSpec {
                    path: "Cargo.toml".into(),
                    content: "[package]\nname = \"myapp\"\n".into(),
                },
                FileSpec {
                    path: "README.md".into(),
                    content: "# My App\n".into(),
                },
            ],
        };

        let result = ScaffoldProjectTool
            .execute(input, &ctx, CancellationToken::new())
            .await
            .unwrap();

        assert!(result.mutated);
        assert!(result.content.contains("3 files created"));

        // Verify files exist
        assert!(tmp.path().join("myapp/src/main.rs").exists());
        assert!(tmp.path().join("myapp/Cargo.toml").exists());
        assert!(tmp.path().join("myapp/README.md").exists());
        assert!(tmp.path().join("myapp/tests").is_dir());
        assert!(tmp.path().join("myapp/docs").is_dir());

        // Verify content
        let content = std::fs::read_to_string(tmp.path().join("myapp/src/main.rs")).unwrap();
        assert_eq!(content, "fn main() {}\n");
    }

    #[tokio::test]
    async fn scaffold_skips_existing_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ToolContext::new(
            tmp.path().to_path_buf(),
            pipit_config::ApprovalMode::FullAuto,
        );

        // Pre-create a file
        std::fs::create_dir_all(tmp.path().join("proj")).unwrap();
        std::fs::write(tmp.path().join("proj/existing.txt"), "original\n").unwrap();

        let input = ScaffoldInput {
            project_root: "proj".into(),
            directories: vec![],
            files: vec![
                FileSpec {
                    path: "existing.txt".into(),
                    content: "overwritten".into(),
                },
                FileSpec {
                    path: "new.txt".into(),
                    content: "new content".into(),
                },
            ],
        };

        let result = ScaffoldProjectTool
            .execute(input, &ctx, CancellationToken::new())
            .await
            .unwrap();

        assert!(result.content.contains("1 files created"));
        assert!(result.content.contains("1 files skipped"));

        // Existing file not overwritten
        let existing = std::fs::read_to_string(tmp.path().join("proj/existing.txt")).unwrap();
        assert_eq!(existing, "original\n");
    }

    #[tokio::test]
    async fn scaffold_rejects_path_traversal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ToolContext::new(
            tmp.path().to_path_buf(),
            pipit_config::ApprovalMode::FullAuto,
        );

        let input = ScaffoldInput {
            project_root: "proj".into(),
            directories: vec![],
            files: vec![FileSpec {
                path: "../../etc/passwd".into(),
                content: "hacked".into(),
            }],
        };

        let result = ScaffoldProjectTool
            .execute(input, &ctx, CancellationToken::new())
            .await;
        assert!(result.is_err());
    }
}
