//! Higher-Order Repository Analysis Tools (Tool/Skill Task 4)
//!
//! Built-in tools for common codebase reasoning that move analysis out of
//! brittle shell commands. Built on file system + text parsing.
//!
//! Tools: repo_map, symbol_xref, change_impact, test_selector, api_surface

use crate::{Tool, ToolContext, ToolDisplay, ToolError, ToolResult};
use async_trait::async_trait;
use pipit_config::ApprovalMode;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

// ═══════════════════════════════════════════════════════════════════════════
//  Symbol Cross-Reference Tool
// ═══════════════════════════════════════════════════════════════════════════

/// Find all references to a symbol across the project using text search.
pub struct SymbolXrefTool;

#[async_trait]
impl Tool for SymbolXrefTool {
    fn name(&self) -> &str { "symbol_xref" }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "symbol": {
                    "type": "string",
                    "description": "The symbol name to find references for"
                },
                "file_pattern": {
                    "type": "string",
                    "description": "Optional glob pattern to restrict search (e.g., '*.rs', '*.py')"
                }
            },
            "required": ["symbol"]
        })
    }

    fn description(&self) -> &str {
        "Find all references to a symbol across the project. Returns file paths, \
         line numbers, and surrounding context for each reference. More precise \
         than grep for symbol-level analysis."
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let symbol = args["symbol"].as_str()
            .ok_or_else(|| ToolError::InvalidArgs("missing 'symbol'".into()))?;
        let pattern = args["file_pattern"].as_str().unwrap_or("*");

        let output = tokio::process::Command::new("grep")
            .args(["-rn", "--include", pattern, "-w", symbol, "."])
            .current_dir(&ctx.project_root)
            .output()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("grep failed: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = stdout.lines().take(100).collect();

        if lines.is_empty() {
            return Ok(ToolResult::text(format!("No references found for '{}'", symbol)));
        }

        // Parse into structured reference list
        let mut refs = Vec::new();
        let mut files_seen = HashSet::new();
        for line in &lines {
            if let Some((file_line, content)) = line.split_once(':') {
                if let Some((file, line_no)) = file_line.split_once(':') {
                    files_seen.insert(file.to_string());
                    refs.push(format!("  {}:{}: {}", file, line_no, content.trim()));
                }
            }
        }

        let summary = format!(
            "Symbol '{}': {} references in {} files\n{}",
            symbol,
            refs.len(),
            files_seen.len(),
            refs.join("\n")
        );

        Ok(ToolResult::text(summary))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Change Impact Analysis Tool
// ═══════════════════════════════════════════════════════════════════════════

/// Analyze the impact of changes to a file on the rest of the project.
pub struct ChangeImpactTool;

#[async_trait]
impl Tool for ChangeImpactTool {
    fn name(&self) -> &str { "change_impact" }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path to analyze impact for"
                }
            },
            "required": ["path"]
        })
    }

    fn description(&self) -> &str {
        "Analyze the impact of changes to a file. Shows which other files \
         import/reference it, what tests cover it, and the blast radius of modifications."
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let path = args["path"].as_str()
            .ok_or_else(|| ToolError::InvalidArgs("missing 'path'".into()))?;

        // Find files that reference/import this file
        let stem = Path::new(path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(path);

        let output = tokio::process::Command::new("grep")
            .args(["-rln", "--include=*.rs", "--include=*.py", "--include=*.ts",
                   "--include=*.js", "--include=*.go", "--include=*.java",
                   stem, "."])
            .current_dir(&ctx.project_root)
            .output()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("grep failed: {}", e)))?;

        let dependents: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| l.trim() != path)
            .take(50)
            .map(|l| l.to_string())
            .collect();

        // Find related test files
        let test_output = tokio::process::Command::new("grep")
            .args(["-rln", "--include=*test*", "--include=*spec*", stem, "."])
            .current_dir(&ctx.project_root)
            .output()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("grep failed: {}", e)))?;

        let test_files: Vec<String> = String::from_utf8_lossy(&test_output.stdout)
            .lines()
            .take(20)
            .map(|l| l.to_string())
            .collect();

        let summary = format!(
            "Impact analysis for '{}'\n\n\
             Dependent files ({}):\n{}\n\n\
             Related tests ({}):\n{}\n\n\
             Blast radius: {} files potentially affected",
            path,
            dependents.len(),
            if dependents.is_empty() { "  (none found)".to_string() } else { dependents.iter().map(|d| format!("  {}", d)).collect::<Vec<_>>().join("\n") },
            test_files.len(),
            if test_files.is_empty() { "  (none found)".to_string() } else { test_files.iter().map(|t| format!("  {}", t)).collect::<Vec<_>>().join("\n") },
            dependents.len() + test_files.len(),
        );

        Ok(ToolResult::text(summary))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Test Selector Tool
// ═══════════════════════════════════════════════════════════════════════════

/// Select relevant tests for changed files.
pub struct TestSelectorTool;

#[async_trait]
impl Tool for TestSelectorTool {
    fn name(&self) -> &str { "test_selector" }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "changed_files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of changed file paths"
                }
            },
            "required": ["changed_files"]
        })
    }

    fn description(&self) -> &str {
        "Given a list of changed files, identify the relevant test files and \
         test commands that should be run to verify the changes."
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let changed = args["changed_files"].as_array()
            .ok_or_else(|| ToolError::InvalidArgs("missing 'changed_files'".into()))?;

        let mut test_files = HashSet::new();
        let mut test_commands = Vec::new();

        for file_val in changed {
            let file = file_val.as_str().unwrap_or("");
            let stem = Path::new(file)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            let ext = Path::new(file)
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("");

            // Try common test file patterns
            let test_patterns = match ext {
                "rs" => vec![
                    format!("{}test{}", stem, ".rs"),
                    format!("test_{}.rs", stem),
                    format!("{}_test.rs", stem),
                ],
                "py" => vec![
                    format!("test_{}.py", stem),
                    format!("{}_test.py", stem),
                ],
                "ts" | "js" => vec![
                    format!("{}.test.{}", stem, ext),
                    format!("{}.spec.{}", stem, ext),
                ],
                _ => vec![],
            };

            // Search for matching test files
            for pattern in &test_patterns {
                let output = tokio::process::Command::new("find")
                    .args([".", "-name", pattern, "-type", "f"])
                    .current_dir(&ctx.project_root)
                    .output()
                    .await;

                if let Ok(out) = output {
                    for line in String::from_utf8_lossy(&out.stdout).lines() {
                        test_files.insert(line.to_string());
                    }
                }
            }

            // Suggest test commands based on file type
            match ext {
                "rs" => {
                    if let Some(crate_name) = detect_rust_crate(ctx, file) {
                        test_commands.push(format!("cargo test -p {}", crate_name));
                    }
                }
                "py" => test_commands.push(format!("pytest {}", file.replace(".py", "_test.py"))),
                "ts" | "js" => test_commands.push(format!("npx jest --testPathPattern={}", stem)),
                _ => {}
            }
        }

        test_commands.dedup();

        let summary = format!(
            "Test selection for {} changed files:\n\n\
             Test files ({}):\n{}\n\n\
             Suggested commands:\n{}",
            changed.len(),
            test_files.len(),
            if test_files.is_empty() { "  (no specific test files found)".to_string() }
            else { test_files.iter().map(|f| format!("  {}", f)).collect::<Vec<_>>().join("\n") },
            if test_commands.is_empty() { "  (no specific commands — run full test suite)".to_string() }
            else { test_commands.iter().map(|c| format!("  $ {}", c)).collect::<Vec<_>>().join("\n") },
        );

        Ok(ToolResult::text(summary))
    }
}

/// Try to detect which Rust crate a file belongs to.
fn detect_rust_crate(ctx: &ToolContext, file: &str) -> Option<String> {
    let abs = ctx.project_root.join(file);
    let mut dir = abs.parent()?;
    loop {
        let cargo = dir.join("Cargo.toml");
        if cargo.exists() {
            let content = std::fs::read_to_string(&cargo).ok()?;
            for line in content.lines() {
                if line.starts_with("name") {
                    if let Some(name) = line.split('"').nth(1) {
                        return Some(name.to_string());
                    }
                }
            }
        }
        dir = dir.parent()?;
        if dir == ctx.project_root || dir == Path::new("/") {
            break;
        }
    }
    None
}

// ═══════════════════════════════════════════════════════════════════════════
//  API Surface Tool
// ═══════════════════════════════════════════════════════════════════════════

/// List public API surface of a module/file.
pub struct ApiSurfaceTool;

#[async_trait]
impl Tool for ApiSurfaceTool {
    fn name(&self) -> &str { "api_surface" }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File or directory path to analyze"
                }
            },
            "required": ["path"]
        })
    }

    fn description(&self) -> &str {
        "List the public API surface of a file or module. Shows exported functions, \
         types, traits, and their signatures. Useful for understanding module interfaces."
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let path = args["path"].as_str()
            .ok_or_else(|| ToolError::InvalidArgs("missing 'path'".into()))?;

        let abs_path = ctx.project_root.join(path);
        if !abs_path.exists() {
            return Err(ToolError::ExecutionFailed(format!("Path not found: {}", path)));
        }

        // Extract public declarations using grep
        let output = tokio::process::Command::new("grep")
            .args(["-rn", "--include=*.rs",
                   "-E", r"^\s*pub\s+(fn|struct|enum|trait|type|const|static|mod)\s",
                   path])
            .current_dir(&ctx.project_root)
            .output()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("grep failed: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = stdout.lines().take(100).collect();

        if lines.is_empty() {
            return Ok(ToolResult::text(format!(
                "No public API surface found in '{}'", path
            )));
        }

        let mut api_items: HashMap<&str, Vec<String>> = HashMap::new();
        for line in &lines {
            let trimmed = line.trim();
            let kind = if trimmed.contains("pub fn ") { "Functions" }
                else if trimmed.contains("pub struct ") { "Structs" }
                else if trimmed.contains("pub enum ") { "Enums" }
                else if trimmed.contains("pub trait ") { "Traits" }
                else if trimmed.contains("pub type ") { "Types" }
                else if trimmed.contains("pub mod ") { "Modules" }
                else { "Other" };
            api_items.entry(kind).or_default().push(line.to_string());
        }

        let mut summary = format!("Public API surface of '{}'\n\n", path);
        for (kind, items) in &api_items {
            summary.push_str(&format!("{}:\n", kind));
            for item in items {
                summary.push_str(&format!("  {}\n", item));
            }
            summary.push('\n');
        }

        Ok(ToolResult::text(summary))
    }
}
