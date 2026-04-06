//! MCP (Model Context Protocol) Client — Gap #1
//!
//! Connects pipit to the MCP ecosystem: hundreds of external tool servers
//! (GitHub, Slack, databases, Figma, Notion, etc.) become native pipit tools.
//!
//! MCP servers expose tools via JSON-RPC over stdio or HTTP+SSE.
//! This module discovers tools from MCP servers and wraps them as pipit `Tool` impls.
//!
//! Config: `.pipit/mcp.json` or `--mcp-server` CLI flag.
//! Protocol: JSON-RPC 2.0 over stdin/stdout (stdio transport).

use async_trait::async_trait;
use pipit_config::ApprovalMode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

// ═══════════════════════════════════════════════════════════════════════════
//  MCP Configuration
// ═══════════════════════════════════════════════════════════════════════════

/// MCP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(rename = "mcpServers", default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpServerConfig {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    Sse {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

/// Load MCP config from `.pipit/mcp.json` or `mcp.json`.
pub fn load_mcp_config(project_root: &Path) -> Option<McpConfig> {
    let candidates = [
        project_root.join(".pipit").join("mcp.json"),
        project_root.join("mcp.json"),
        // Also support Claude Code's format for compatibility
        project_root.join(".claude").join("mcp.json"),
    ];

    for path in &candidates {
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(path) {
                if let Ok(config) = serde_json::from_str::<McpConfig>(&content) {
                    tracing::info!(path = %path.display(), servers = config.mcp_servers.len(), "loaded MCP config");
                    return Some(config);
                }
            }
        }
    }
    None
}

// ═══════════════════════════════════════════════════════════════════════════
//  MCP Client (stdio transport)
// ═══════════════════════════════════════════════════════════════════════════

/// A connection to a single MCP server.
pub struct McpClient {
    pub name: String,
    child: Mutex<Child>,
    stdin: Mutex<tokio::process::ChildStdin>,
    stdout: Mutex<BufReader<tokio::process::ChildStdout>>,
    next_id: Mutex<u64>,
    pub tools: Vec<McpToolDef>,
}

/// An MCP tool definition (from the server's tools/list response).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// JSON-RPC 2.0 request.
#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

/// JSON-RPC 2.0 response.
#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Option<u64>,
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    data: Option<Value>,
}

impl McpClient {
    /// Connect to an MCP server via stdio transport.
    pub async fn connect_stdio(
        name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Self, String> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let mut child = cmd.spawn().map_err(|e| format!("Failed to start MCP server '{}': {}", name, e))?;

        let stdin = child.stdin.take().ok_or("No stdin")?;
        let stdout = child.stdout.take().ok_or("No stdout")?;

        let mut client = Self {
            name: name.to_string(),
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            next_id: Mutex::new(1),
            tools: Vec::new(),
        };

        // Initialize: send initialize request
        let init_result = client.call("initialize", Some(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "pipit",
                "version": env!("CARGO_PKG_VERSION")
            }
        }))).await?;

        tracing::debug!(server = name, "MCP initialize response: {:?}", init_result);

        // Send initialized notification
        client.notify("notifications/initialized", None).await?;

        // Discover tools
        let tools_result = client.call("tools/list", None).await?;
        if let Some(tools_array) = tools_result.get("tools").and_then(|t| t.as_array()) {
            client.tools = tools_array.iter()
                .filter_map(|t| serde_json::from_value::<McpToolDef>(t.clone()).ok())
                .collect();
            tracing::info!(
                server = name,
                tools = client.tools.len(),
                "MCP server connected, discovered {} tools",
                client.tools.len()
            );
        }

        Ok(client)
    }

    /// Send a JSON-RPC request and await the response.
    async fn call(&self, method: &str, params: Option<Value>) -> Result<Value, String> {
        let id = {
            let mut next = self.next_id.lock().await;
            let id = *next;
            *next += 1;
            id
        };

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        };

        let request_json = serde_json::to_string(&request)
            .map_err(|e| format!("Serialize error: {}", e))?;

        // Write request
        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(request_json.as_bytes()).await
                .map_err(|e| format!("Write error: {}", e))?;
            stdin.write_all(b"\n").await
                .map_err(|e| format!("Write error: {}", e))?;
            stdin.flush().await
                .map_err(|e| format!("Flush error: {}", e))?;
        }

        // Read response
        let mut line = String::new();
        {
            let mut stdout = self.stdout.lock().await;
            stdout.read_line(&mut line).await
                .map_err(|e| format!("Read error: {}", e))?;
        }

        let response: JsonRpcResponse = serde_json::from_str(&line)
            .map_err(|e| format!("Parse error: {} (line: {})", e, line.trim()))?;

        if let Some(err) = response.error {
            return Err(format!("MCP error {}: {}", err.code, err.message));
        }

        Ok(response.result.unwrap_or(Value::Null))
    }

    /// Send a JSON-RPC notification (no response expected).
    async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), String> {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params.unwrap_or(Value::Object(serde_json::Map::new()))
        });

        let json = serde_json::to_string(&notification).map_err(|e| e.to_string())?;

        let mut stdin = self.stdin.lock().await;
        stdin.write_all(json.as_bytes()).await.map_err(|e| e.to_string())?;
        stdin.write_all(b"\n").await.map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())?;

        Ok(())
    }

    /// Call a tool on this MCP server.
    pub async fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<String, String> {
        let result = self.call("tools/call", Some(serde_json::json!({
            "name": tool_name,
            "arguments": arguments
        }))).await?;

        // MCP tools return content array
        if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
            let texts: Vec<String> = content.iter()
                .filter_map(|item| {
                    if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                        item.get("text").and_then(|t| t.as_str()).map(String::from)
                    } else {
                        Some(serde_json::to_string_pretty(item).unwrap_or_default())
                    }
                })
                .collect();
            Ok(texts.join("\n"))
        } else {
            Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
        }
    }

    /// Shut down the MCP server gracefully.
    pub async fn shutdown(&self) {
        let _ = self.notify("notifications/cancelled", None).await;
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
    }

    /// Send a raw JSON-RPC method call to the MCP server.
    /// Use this for protocol-level calls like resources/list, resources/read.
    pub async fn call_method(&self, method: &str, params: Option<Value>) -> Result<Value, String> {
        self.call(method, params).await
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  MCP Tool Wrapper — makes MCP tools look like native pipit tools
// ═══════════════════════════════════════════════════════════════════════════

/// Wraps an MCP server tool as a pipit `Tool` implementation.
pub struct McpToolWrapper {
    pub server_name: String,
    pub tool_def: McpToolDef,
    client: Arc<McpClient>,
}

impl McpToolWrapper {
    pub fn new(client: Arc<McpClient>, tool_def: McpToolDef) -> Self {
        Self {
            server_name: client.name.clone(),
            tool_def,
            client,
        }
    }
}

#[async_trait]
impl crate::Tool for McpToolWrapper {
    fn name(&self) -> &str {
        &self.tool_def.name
    }

    fn schema(&self) -> Value {
        self.tool_def.input_schema.clone()
    }

    fn description(&self) -> &str {
        &self.tool_def.description
    }

    async fn execute(
        &self,
        args: Value,
        _ctx: &crate::ToolContext,
        _cancel: CancellationToken,
    ) -> Result<crate::ToolResult, crate::ToolError> {
        match self.client.call_tool(&self.tool_def.name, args).await {
            Ok(result) => Ok(crate::ToolResult::text(result)),
            Err(e) => Err(crate::ToolError::ExecutionFailed(
                format!("MCP tool '{}' on server '{}' failed: {}", self.tool_def.name, self.server_name, e)
            )),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  MCP Manager — lifecycle management for all MCP servers
// ═══════════════════════════════════════════════════════════════════════════

/// Manages connections to all configured MCP servers.
pub struct McpManager {
    clients: Vec<Arc<McpClient>>,
}

impl McpManager {
    /// Connect to all MCP servers defined in the config.
    pub async fn from_config(config: &McpConfig) -> Self {
        let mut clients = Vec::new();

        for (name, server_config) in &config.mcp_servers {
            match server_config {
                McpServerConfig::Stdio { command, args, env } => {
                    match McpClient::connect_stdio(name, command, args, env).await {
                        Ok(client) => {
                            tracing::info!(server = %name, tools = client.tools.len(), "MCP server connected");
                            clients.push(Arc::new(client));
                        }
                        Err(e) => {
                            tracing::error!(server = %name, error = %e, "Failed to connect MCP server");
                        }
                    }
                }
                McpServerConfig::Sse { url, .. } => {
                    tracing::warn!(server = %name, url = %url, "SSE transport not yet implemented");
                }
            }
        }

        Self { clients }
    }

    /// Register all MCP tools into a pipit ToolRegistry.
    pub fn register_tools(&self, registry: &mut crate::ToolRegistry) {
        for client in &self.clients {
            for tool_def in &client.tools {
                let wrapper = McpToolWrapper::new(Arc::clone(client), tool_def.clone());
                registry.register(Arc::new(wrapper));
                tracing::debug!(
                    server = %client.name,
                    tool = %tool_def.name,
                    "Registered MCP tool"
                );
            }
        }
    }

    /// Two-stage registration: eagerly register tools for small servers,
    /// lazily index tools for servers above the threshold.
    /// Returns Some(LazyToolIndex) if any servers were lazily indexed.
    pub fn register_tools_lazy(
        &self,
        registry: &mut crate::ToolRegistry,
        threshold: usize,
    ) -> Option<crate::lazy_index::LazyToolIndex> {
        let mut lazy_index = crate::lazy_index::LazyToolIndex::new();
        let mut has_lazy = false;

        for client in &self.clients {
            if client.tools.len() <= threshold {
                // Eager: register all tools directly
                for tool_def in &client.tools {
                    let wrapper = McpToolWrapper::new(Arc::clone(client), tool_def.clone());
                    registry.register(Arc::new(wrapper));
                    tracing::debug!(
                        server = %client.name,
                        tool = %tool_def.name,
                        "Eagerly registered MCP tool"
                    );
                }
            } else {
                // Lazy: index tools without registering them
                let tools: Vec<(String, String)> = client.tools.iter()
                    .map(|t| (t.name.clone(), t.description.clone()))
                    .collect();
                lazy_index.index_server(&client.name, &tools);
                has_lazy = true;
                tracing::info!(
                    server = %client.name,
                    tools = client.tools.len(),
                    "Lazily indexed MCP server ({} tools > {} threshold)",
                    client.tools.len(),
                    threshold
                );
            }
        }

        if has_lazy { Some(lazy_index) } else { None }
    }

    /// Clone the client Arcs for use by the mcp_search meta-tool.
    pub fn clone_clients(&self) -> Vec<Arc<McpClient>> {
        self.clients.clone()
    }

    /// Get references to all connected clients (for health checks).
    pub fn clients(&self) -> &[Arc<McpClient>] {
        &self.clients
    }

    /// Get total tool count across all servers.
    pub fn tool_count(&self) -> usize {
        self.clients.iter().map(|c| c.tools.len()).sum()
    }

    /// Get all server names.
    pub fn server_names(&self) -> Vec<&str> {
        self.clients.iter().map(|c| c.name.as_str()).collect()
    }

    /// Shut down all MCP servers.
    pub async fn shutdown_all(&self) {
        for client in &self.clients {
            client.shutdown().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_mcp_config() {
        let json = r#"{
            "mcpServers": {
                "filesystem": {
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
                    "env": {}
                },
                "github": {
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-github"],
                    "env": {"GITHUB_TOKEN": "ghp_xxx"}
                }
            }
        }"#;

        let config: McpConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.mcp_servers.len(), 2);
        assert!(config.mcp_servers.contains_key("filesystem"));
        assert!(config.mcp_servers.contains_key("github"));
    }

    #[test]
    fn test_parse_empty_config() {
        let json = r#"{"mcpServers": {}}"#;
        let config: McpConfig = serde_json::from_str(json).unwrap();
        assert!(config.mcp_servers.is_empty());
    }

    #[test]
    fn test_mcp_tool_def_parse() {
        let json = r#"{
            "name": "read_file",
            "description": "Read a file",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }
        }"#;
        let tool: McpToolDef = serde_json::from_str(json).unwrap();
        assert_eq!(tool.name, "read_file");
    }
}
