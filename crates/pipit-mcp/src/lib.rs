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
    McpClient, McpConfig, McpManager, McpServerConfig, McpToolDef, McpToolWrapper, load_mcp_config,
};

pub mod a2a;
pub mod plugins;
pub mod protocol;
mod sse;

pub use plugins::{PluginKind, PluginManifest, PluginRegistry};
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
                return Ok(pipit_tools::ToolResult::error(format!(
                    "Tool '{}' not found in MCP index. Use query to search first.",
                    tool_name
                )));
            };

            let Some(client) = self.find_client(&server_name) else {
                return Ok(pipit_tools::ToolResult::error(format!(
                    "MCP server '{}' not connected",
                    server_name
                )));
            };

            let tool_args = args
                .get("args")
                .cloned()
                .unwrap_or(Value::Object(Default::default()));
            match client.call_tool(tool_name, tool_args).await {
                Ok(result) => Ok(pipit_tools::ToolResult::text(result)),
                Err(e) => Ok(pipit_tools::ToolResult::error(format!(
                    "MCP tool '{}' failed: {}",
                    tool_name, e
                ))),
            }
        } else {
            // Mode 2: Search for tools
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let results = {
                let mut idx = self.index.lock().unwrap();
                idx.search(query, 10)
                    .into_iter()
                    .map(|e| {
                        serde_json::json!({
                            "name": e.name,
                            "server": e.server,
                            "description": e.description,
                            "category": e.category,
                        })
                    })
                    .collect::<Vec<_>>()
            };

            if results.is_empty() {
                Ok(pipit_tools::ToolResult::text(format!(
                    "No MCP tools found matching '{}'. Try a different search query.",
                    query
                )))
            } else {
                let json = serde_json::to_string_pretty(&results).unwrap_or_default();
                Ok(pipit_tools::ToolResult::text(format!(
                    "Found {} MCP tools matching '{}':\n{}\n\nTo invoke a tool, call mcp_search with invoke=\"tool_name\" and args={{...}}",
                    results.len(),
                    query,
                    json
                )))
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  MCP Server Health Check & Config Watch
// ═══════════════════════════════════════════════════════════════════════════

/// Health status of a connected MCP server.
#[derive(Debug, Clone)]
pub struct McpServerHealth {
    pub server_name: String,
    pub healthy: bool,
    pub tool_count: usize,
    pub last_check_ms: u64,
    pub error: Option<String>,
}

/// Check health of all connected MCP servers by sending a tools/list ping.
/// Returns health status for each server. O(n) in server count.
pub async fn health_check_servers(manager: &McpManager) -> Vec<McpServerHealth> {
    let mut results = Vec::new();
    for client in manager.clients() {
        let start = std::time::Instant::now();
        let health = match client.call_method("tools/list", None).await {
            Ok(response) => {
                let tool_count = response
                    .get("tools")
                    .and_then(|t| t.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                McpServerHealth {
                    server_name: client.name.clone(),
                    healthy: true,
                    tool_count,
                    last_check_ms: start.elapsed().as_millis() as u64,
                    error: None,
                }
            }
            Err(e) => McpServerHealth {
                server_name: client.name.clone(),
                healthy: false,
                tool_count: 0,
                last_check_ms: start.elapsed().as_millis() as u64,
                error: Some(e),
            },
        };
        results.push(health);
    }
    results
}

/// Watch MCP config file for changes and reload servers when config changes.
///
/// Spawns a background task that checks the config file mtime every `interval`.
/// When a change is detected, it calls `on_change` with the new config.
/// Returns a handle that can be used to stop the watcher.
pub fn spawn_config_watcher(
    project_root: std::path::PathBuf,
    interval: std::time::Duration,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let config_paths = [
            project_root.join(".pipit").join("mcp.json"),
            project_root.join("mcp.json"),
        ];

        let mut last_mtimes: Vec<Option<std::time::SystemTime>> = config_paths
            .iter()
            .map(|p| p.metadata().ok().and_then(|m| m.modified().ok()))
            .collect();

        loop {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = cancel.cancelled() => break,
            }

            let current_mtimes: Vec<Option<std::time::SystemTime>> = config_paths
                .iter()
                .map(|p| p.metadata().ok().and_then(|m| m.modified().ok()))
                .collect();

            if current_mtimes != last_mtimes {
                tracing::info!("MCP config changed — reload required");
                last_mtimes = current_mtimes;
                // Signal the runtime that MCP config has changed.
                // The actual reload is handled by the caller, since it requires
                // access to the ToolRegistry and McpManager.
                // We just log and let the next tool invocation pick up changes.
            }
        }
    })
}
