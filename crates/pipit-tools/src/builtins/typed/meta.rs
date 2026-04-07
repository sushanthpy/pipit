//! Meta Tools — sleep, config, brief, tool_search.
//!
//! Small, high-utility tools. All implemented as TypedTool.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::typed_tool::*;
use crate::{ToolContext, ToolError};

// ═══════════════════════════════════════════════════════════════
//  SLEEP/WAIT TOOL
// ═══════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SleepInput {
    /// Duration to sleep in milliseconds.
    pub duration_ms: u64,
}

/// Cancel-aware sleep tool.
pub struct SleepWaitTool;

#[async_trait]
impl TypedTool for SleepWaitTool {
    type Input = SleepInput;
    const NAME: &'static str = "sleep_typed";
    const CAPABILITIES: CapabilitySet = CapabilitySet::NONE;
    const PURITY: Purity = Purity::Pure;

    fn describe() -> ToolCard {
        ToolCard {
            name: "sleep_typed".into(),
            summary: "Wait for a specified duration".into(),
            when_to_use: "When you need to wait before retrying an operation, or to add a delay between steps.".into(),
            examples: vec![ToolExample {
                description: "Wait 2 seconds".into(),
                input: serde_json::json!({"duration_ms": 2000}),
            }],
            tags: vec!["wait".into(), "sleep".into(), "delay".into(), "timer".into()],
            purity: Purity::Pure,
            capabilities: 0,
        }
    }

    async fn execute(
        &self,
        input: SleepInput,
        _ctx: &ToolContext,
        cancel: CancellationToken,
    ) -> Result<TypedToolResult, ToolError> {
        let duration = std::time::Duration::from_millis(input.duration_ms.min(300_000)); // Cap at 5 min
        tokio::select! {
            _ = tokio::time::sleep(duration) => {
                Ok(TypedToolResult::text(format!("Waited {}ms.", input.duration_ms)))
            }
            _ = cancel.cancelled() => {
                Err(ToolError::ExecutionFailed("Sleep cancelled".into()))
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════
//  CONFIG ACCESS TOOL
// ═══════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ConfigInput {
    /// Get a configuration value.
    Get { key: String },
    /// List all configuration keys.
    List,
}

/// Read/list user configuration.
pub struct ConfigAccessTool;

#[async_trait]
impl TypedTool for ConfigAccessTool {
    type Input = ConfigInput;
    const NAME: &'static str = "config_typed";
    const CAPABILITIES: CapabilitySet = CapabilitySet::NONE;
    const PURITY: Purity = Purity::Pure;

    fn describe() -> ToolCard {
        ToolCard {
            name: "config_typed".into(),
            summary: "Read pipit configuration values".into(),
            when_to_use: "When you need to check the current configuration — model, provider, approval mode, etc.".into(),
            examples: vec![ToolExample {
                description: "Get approval mode".into(),
                input: serde_json::json!({"action": "get", "key": "approval_mode"}),
            }],
            tags: vec!["config".into(), "settings".into(), "preferences".into()],
            purity: Purity::Pure,
            capabilities: 0,
        }
    }

    async fn execute(
        &self,
        input: ConfigInput,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<TypedToolResult, ToolError> {
        match input {
            ConfigInput::Get { key } => {
                let value = match key.as_str() {
                    "project_root" => ctx.project_root.display().to_string(),
                    "cwd" => ctx.current_dir().display().to_string(),
                    "approval_mode" => format!("{:?}", ctx.approval_mode),
                    _ => format!("Unknown config key: {key}"),
                };
                Ok(TypedToolResult::text(format!("{key} = {value}")))
            }
            ConfigInput::List => {
                Ok(TypedToolResult::text(format!(
                    "Configuration:\n  project_root = {}\n  cwd = {}\n  approval_mode = {:?}",
                    ctx.project_root.display(),
                    ctx.current_dir().display(),
                    ctx.approval_mode,
                )))
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════
//  BRIEF CONTEXT TOOL
// ═══════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BriefInput {}

/// Summary of current session context. Useful after compaction.
pub struct BriefContextTool;

#[async_trait]
impl TypedTool for BriefContextTool {
    type Input = BriefInput;
    const NAME: &'static str = "brief";
    const CAPABILITIES: CapabilitySet = CapabilitySet::NONE;
    const PURITY: Purity = Purity::Pure;

    fn describe() -> ToolCard {
        ToolCard {
            name: "brief".into(),
            summary: "Get a concise summary of the current session context".into(),
            when_to_use: "After context compaction, or to orient yourself about the current project state, working directory, and recent activity.".into(),
            examples: vec![ToolExample {
                description: "Get session brief".into(),
                input: serde_json::json!({}),
            }],
            tags: vec!["context".into(), "summary".into(), "session".into(), "orient".into()],
            purity: Purity::Pure,
            capabilities: 0,
        }
    }

    async fn execute(
        &self,
        _input: BriefInput,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<TypedToolResult, ToolError> {
        let cwd = ctx.current_dir();
        let root = &ctx.project_root;

        // Build a brief from available context
        let mut sections = vec![
            format!("**Working directory:** {}", cwd.display()),
            format!("**Project root:** {}", root.display()),
            format!("**Approval mode:** {:?}", ctx.approval_mode),
        ];

        // Check for key files
        for name in &["README.md", "Cargo.toml", "package.json", "pyproject.toml", ".git"] {
            if root.join(name).exists() {
                sections.push(format!("  ✓ {name} exists"));
            }
        }

        Ok(TypedToolResult::text(sections.join("\n")))
    }
}

// ═══════════════════════════════════════════════════════════════
//  TOOL SEARCH (META-SEARCH OVER TOOL CARDS)
// ═══════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ToolSearchInput {
    /// Search query.
    pub query: String,
    /// Maximum results (default: 10).
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 { 10 }

/// BM25 search over ToolCard descriptions.
pub struct TypedToolSearchTool {
    index: Arc<Mutex<ToolSearchIndex>>,
}

impl TypedToolSearchTool {
    pub fn new() -> Self {
        let mut index = ToolSearchIndex::new();
        // Seed with known tool cards.
        // In production, this is populated from all registered TypedTools.
        for card in Self::builtin_cards() {
            index.add(card);
        }
        Self { index: Arc::new(Mutex::new(index)) }
    }

    fn builtin_cards() -> Vec<ToolCard> {
        vec![
            // Core tools (ToolCard for legacy tools that don't have TypedTool)
            ToolCard {
                name: "read_file".into(), summary: "Read file contents with line range".into(),
                when_to_use: "When you need to see file contents".into(), examples: vec![],
                tags: vec!["file".into(), "read".into(), "filesystem".into()],
                purity: Purity::Pure, capabilities: CapabilitySet::FS_READ.0,
            },
            ToolCard {
                name: "write_file".into(), summary: "Create or overwrite a file".into(),
                when_to_use: "When you need to create a new file or replace all contents".into(), examples: vec![],
                tags: vec!["file".into(), "write".into(), "create".into()],
                purity: Purity::Mutating, capabilities: CapabilitySet::FS_WRITE.0,
            },
            ToolCard {
                name: "edit_file".into(), summary: "Search and replace in a file".into(),
                when_to_use: "When you need to modify specific parts of an existing file".into(), examples: vec![],
                tags: vec!["file".into(), "edit".into(), "modify".into(), "search".into(), "replace".into()],
                purity: Purity::Mutating, capabilities: CapabilitySet::FS_WRITE.0,
            },
            ToolCard {
                name: "bash".into(), summary: "Execute shell commands".into(),
                when_to_use: "When you need to run commands, install packages, run tests, or interact with the system".into(), examples: vec![],
                tags: vec!["shell".into(), "command".into(), "execution".into(), "terminal".into()],
                purity: Purity::Destructive, capabilities: CapabilitySet::PROCESS_EXEC.0,
            },
            ToolCard {
                name: "grep".into(), summary: "Search file contents with regex".into(),
                when_to_use: "When you need to find text patterns across files".into(), examples: vec![],
                tags: vec!["search".into(), "regex".into(), "find".into(), "grep".into()],
                purity: Purity::Pure, capabilities: CapabilitySet::FS_READ.0,
            },
            ToolCard {
                name: "glob".into(), summary: "Find files by name pattern".into(),
                when_to_use: "When you need to find files matching a glob pattern".into(), examples: vec![],
                tags: vec!["file".into(), "find".into(), "pattern".into(), "glob".into()],
                purity: Purity::Pure, capabilities: CapabilitySet::FS_READ.0,
            },
            ToolCard {
                name: "list_directory".into(), summary: "List directory contents".into(),
                when_to_use: "When you need to see what files are in a directory".into(), examples: vec![],
                tags: vec!["directory".into(), "list".into(), "ls".into(), "filesystem".into()],
                purity: Purity::Pure, capabilities: CapabilitySet::FS_READ.0,
            },
            ToolCard {
                name: "subagent".into(), summary: "Delegate a task to a sub-agent".into(),
                when_to_use: "When a task can be done independently by a parallel agent".into(), examples: vec![],
                tags: vec!["agent".into(), "delegate".into(), "parallel".into(), "subagent".into()],
                purity: Purity::Mutating, capabilities: CapabilitySet::DELEGATE.0,
            },
        ]
    }
}

#[async_trait]
impl TypedTool for TypedToolSearchTool {
    type Input = ToolSearchInput;
    const NAME: &'static str = "tool_search";
    const CAPABILITIES: CapabilitySet = CapabilitySet::NONE;
    const PURITY: Purity = Purity::Pure;

    fn describe() -> ToolCard {
        ToolCard {
            name: "tool_search".into(),
            summary: "Search available tools by keyword".into(),
            when_to_use: "When you're unsure which tool to use. Search by keyword to find the right tool for your task.".into(),
            examples: vec![ToolExample {
                description: "Find file-related tools".into(),
                input: serde_json::json!({"query": "file edit", "limit": 5}),
            }],
            tags: vec!["meta".into(), "search".into(), "discovery".into(), "help".into()],
            purity: Purity::Pure,
            capabilities: 0,
        }
    }

    async fn execute(
        &self,
        input: ToolSearchInput,
        _ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<TypedToolResult, ToolError> {
        let index = self.index.lock().map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        let results = index.search(&input.query, input.limit as usize);

        if results.is_empty() {
            return Ok(TypedToolResult::text(format!(
                "No tools found matching '{}'.", input.query
            )));
        }

        let formatted: Vec<String> = results.iter().map(|card| {
            format!(
                "**{}** — {}\n  When: {}\n  Tags: [{}] | Purity: {:?}",
                card.name, card.summary, card.when_to_use,
                card.tags.join(", "),
                card.purity,
            )
        }).collect();

        Ok(TypedToolResult::text(format!(
            "Found {} tool(s) matching '{}':\n\n{}",
            results.len(), input.query,
            formatted.join("\n\n")
        )))
    }
}
