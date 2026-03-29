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
/// Uses lazy loading when a server exposes >20 tools.
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
    manager.register_tools(registry);

    tracing::info!(
        servers = manager.server_names().len(),
        tools = total,
        "MCP initialization complete"
    );
    Some(manager)
}
