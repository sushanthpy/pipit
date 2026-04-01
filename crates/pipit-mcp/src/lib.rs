//! pipit-mcp: Full MCP (Model Context Protocol) client implementation.
//!
//! Supports stdio and SSE transports. Discovers MCP tools from configured servers
//! and dynamically registers them into pipit's ToolRegistry. Implements lazy loading
//! for servers with >20 tools via an `mcp_search` meta-tool.
//!
//! Re-exports the core MCP client infrastructure from pipit-tools and adds:
//! - SSE transport (HTTP + Server-Sent Events)
//! - Lazy tool loading with mcp_search meta-tool
//! - MCP server lifecycle management from CLI
//! - `pipit mcp add <server>` support

pub use pipit_tools::mcp::{
    load_mcp_config, McpClient, McpConfig, McpManager, McpServerConfig, McpToolDef, McpToolWrapper,
};

mod sse;
pub mod a2a;
pub mod plugins;

pub use sse::SseTransport;

/// Maximum tools to eagerly register per server before switching to lazy mode.
pub const LAZY_TOOL_THRESHOLD: usize = 20;

/// Connect to all configured MCP servers and register tools into the registry.
/// Uses lazy loading when a server exposes >LAZY_TOOL_THRESHOLD tools:
/// instead of registering all tools, we index them and register a single
/// `mcp_search` meta-tool that searches the index on demand.
pub async fn initialize_mcp(
    project_root: &std::path::Path,
    registry: &mut pipit_tools::ToolRegistry,
) -> Option<McpManager> {
    let config = load_mcp_config(project_root)?;
    if config.mcp_servers.is_empty() {
        return None;
    }

    let manager = McpManager::from_config(&config).await;
    let total = manager.tool_count();

    // Two-stage registration: eager for small servers, lazy for large ones.
    let lazy_index = manager.register_tools_lazy(registry, LAZY_TOOL_THRESHOLD);

    if let Some(index) = lazy_index {
        let lazy_count = index.total_tools();
        // Register the mcp_search meta-tool backed by this index.
        let search_tool = McpSearchTool::new(index, manager.clone_clients());
        registry.register(std::sync::Arc::new(search_tool));
        tracing::info!(
            lazy_tools = lazy_count,
            "Registered mcp_search meta-tool for lazy MCP tool access"
        );
    }

    tracing::info!(
        servers = manager.server_names().len(),
        tools = total,
        "MCP initialization complete"
    );
    Some(manager)
}

// ═══════════════════════════════════════════════════════════════════════════
//  mcp_search meta-tool — searches the lazy index and materializes tools
// ═══════════════════════════════════════════════════════════════════════════

use async_trait::async_trait;
use pipit_tools::lazy_index::LazyToolIndex;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// Meta-tool that searches the lazy MCP tool index and can invoke matched tools.
pub struct McpSearchTool {
    index: Arc<Mutex<LazyToolIndex>>,
    clients: Vec<Arc<McpClient>>,
}

impl McpSearchTool {
    pub fn new(index: LazyToolIndex, clients: Vec<Arc<McpClient>>) -> Self {
        Self {
            index: Arc::new(Mutex::new(index)),
            clients,
        }
    }

    fn find_client(&self, server_name: &str) -> Option<&Arc<McpClient>> {
        self.clients.iter().find(|c| c.name == server_name)
    }
}

#[async_trait]
impl pipit_tools::Tool for McpSearchTool {
    fn name(&self) -> &str {
        "mcp_search"
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query to find MCP tools by name or description"
                },
                "invoke": {
                    "type": "string",
                    "description": "If set, invoke this tool by exact name instead of searching"
                },
                "args": {
                    "type": "object",
                    "description": "Arguments to pass when invoking a tool via the 'invoke' field"
                }
            },
            "required": ["query"]
        })
    }

    fn description(&self) -> &str {
        "Search for MCP tools by keyword, or invoke a discovered tool by name. \
         Use query to find tools, then invoke with the tool name and args."
    }

    fn is_mutating(&self) -> bool {
        // Searching is read-only, but invoking a tool may mutate.
        true
    }

    fn requires_approval(&self, mode: pipit_config::ApprovalMode) -> bool {
        !matches!(mode, pipit_config::ApprovalMode::FullAuto)
    }

    async fn execute(
        &self,
        args: Value,
        _ctx: &pipit_tools::ToolContext,
        _cancel: CancellationToken,
    ) -> Result<pipit_tools::ToolResult, pipit_tools::ToolError> {
        // Mode 1: Invoke a specific tool by name
        if let Some(tool_name) = args.get("invoke").and_then(|v| v.as_str()) {
            let server_name = {
                let idx = self.index.lock().unwrap();
                idx.get_by_name(tool_name).map(|e| e.server.clone())
            };

            let Some(server_name) = server_name else {
                return Ok(pipit_tools::ToolResult::error(
                    format!("Tool '{}' not found in MCP index. Use query to search first.", tool_name)
                ));
            };

            let Some(client) = self.find_client(&server_name) else {
                return Ok(pipit_tools::ToolResult::error(
                    format!("MCP server '{}' not connected", server_name)
                ));
            };

            let tool_args = args.get("args").cloned().unwrap_or(Value::Object(Default::default()));
            match client.call_tool(tool_name, tool_args).await {
                Ok(result) => Ok(pipit_tools::ToolResult::text(result)),
                Err(e) => Ok(pipit_tools::ToolResult::error(
                    format!("MCP tool '{}' failed: {}", tool_name, e)
                )),
            }
        } else {
            // Mode 2: Search for tools
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let results = {
                let mut idx = self.index.lock().unwrap();
                idx.search(query, 10)
                    .into_iter()
                    .map(|e| serde_json::json!({
                        "name": e.name,
                        "server": e.server,
                        "description": e.description,
                        "category": e.category,
                    }))
                    .collect::<Vec<_>>()
            };

            if results.is_empty() {
                Ok(pipit_tools::ToolResult::text(
                    format!("No MCP tools found matching '{}'. Try a different search query.", query)
                ))
            } else {
                let json = serde_json::to_string_pretty(&results).unwrap_or_default();
                Ok(pipit_tools::ToolResult::text(
                    format!("Found {} MCP tools matching '{}':\n{}\n\nTo invoke a tool, call mcp_search with invoke=\"tool_name\" and args={{...}}", results.len(), query, json)
                ))
            }
        }
    }
}
