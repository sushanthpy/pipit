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
    StreamableHttp {
        #[serde(rename = "streamableUrl")]
        streamable_url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
        #[serde(default)]
        oauth: Option<McpOAuthConfig>,
    },
}

/// OAuth configuration for MCP servers requiring authentication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpOAuthConfig {
    pub auth_url: String,
    pub token_url: String,
    pub client_id: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    pub redirect_uri: Option<String>,
}

/// Load MCP config from `.pipit/mcp.json` or `mcp.json`.
pub fn load_mcp_config(project_root: &Path) -> Option<McpConfig> {
    let candidates = [
        project_root.join(".pipit").join("mcp.json"),
        project_root.join("mcp.json"),
        // Claude Code / ECC compatibility: .mcp.json (dot-prefix, Claude Code native format)
        project_root.join(".mcp.json"),
        // Support .claude/ directory format for cross-tool compatibility
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
#[allow(dead_code)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Option<u64>,
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
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

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to start MCP server '{}': {}", name, e))?;

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
        let init_result = client
            .call(
                "initialize",
                Some(serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "pipit",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                })),
            )
            .await?;

        tracing::debug!(server = name, "MCP initialize response: {:?}", init_result);

        // Send initialized notification
        client.notify("notifications/initialized", None).await?;

        // Discover tools
        let tools_result = client.call("tools/list", None).await?;
        if let Some(tools_array) = tools_result.get("tools").and_then(|t| t.as_array()) {
            client.tools = tools_array
                .iter()
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

        let request_json =
            serde_json::to_string(&request).map_err(|e| format!("Serialize error: {}", e))?;

        // Write request
        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(request_json.as_bytes())
                .await
                .map_err(|e| format!("Write error: {}", e))?;
            stdin
                .write_all(b"\n")
                .await
                .map_err(|e| format!("Write error: {}", e))?;
            stdin
                .flush()
                .await
                .map_err(|e| format!("Flush error: {}", e))?;
        }

        // Read response
        let mut line = String::new();
        {
            let mut stdout = self.stdout.lock().await;
            stdout
                .read_line(&mut line)
                .await
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
        stdin
            .write_all(json.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin.write_all(b"\n").await.map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())?;

        Ok(())
    }

    /// Call a tool on this MCP server.
    /// Handles MCP elicitation error (-32042) by returning a structured prompt.
    pub async fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<String, String> {
        let result = self
            .call(
                "tools/call",
                Some(serde_json::json!({
                    "name": tool_name,
                    "arguments": arguments
                })),
            )
            .await;

        match result {
            Ok(value) => {
                // MCP tools return content array
                if let Some(content) = value.get("content").and_then(|c| c.as_array()) {
                    let texts: Vec<String> = content
                        .iter()
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
                    Ok(serde_json::to_string_pretty(&value).unwrap_or_default())
                }
            }
            Err(e) => {
                // Check for MCP elicitation error (-32042)
                if e.contains("-32042") {
                    // Parse the elicitation request from the error
                    Err(format!(
                        "[MCP Elicitation Required] The MCP server '{}' needs user interaction to proceed.\n\
                         Tool: {}\n\
                         Details: {}\n\
                         Please provide the requested information and retry.",
                        self.name, tool_name, e
                    ))
                } else {
                    Err(e)
                }
            }
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

    /// List resources available on this MCP server.
    pub async fn list_resources(&self) -> Result<Vec<Value>, String> {
        let result = self.call("resources/list", None).await?;
        Ok(result
            .get("resources")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// Read a specific resource by URI.
    pub async fn read_resource(&self, uri: &str) -> Result<String, String> {
        let result = self
            .call(
                "resources/read",
                Some(serde_json::json!({
                    "uri": uri
                })),
            )
            .await?;
        if let Some(contents) = result.get("contents").and_then(|c| c.as_array()) {
            let texts: Vec<String> = contents
                .iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()).map(String::from))
                .collect();
            Ok(texts.join("\n"))
        } else {
            Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
        }
    }

    /// Subscribe to resource updates.
    pub async fn subscribe_resource(&self, uri: &str) -> Result<(), String> {
        self.call(
            "resources/subscribe",
            Some(serde_json::json!({
                "uri": uri
            })),
        )
        .await?;
        Ok(())
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
            Err(e) => Err(crate::ToolError::ExecutionFailed(format!(
                "MCP tool '{}' on server '{}' failed: {}",
                self.tool_def.name, self.server_name, e
            ))),
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
                            tracing::info!(server = %name, tools = client.tools.len(), "MCP server connected (stdio)");
                            clients.push(Arc::new(client));
                        }
                        Err(e) => {
                            tracing::error!(server = %name, error = %e, "Failed to connect MCP server (stdio)");
                        }
                    }
                }
                McpServerConfig::Sse { url, headers } => {
                    match connect_sse_server(name, url, headers).await {
                        Ok(client) => {
                            tracing::info!(server = %name, tools = client.tools.len(), "MCP server connected (SSE)");
                            clients.push(Arc::new(client));
                        }
                        Err(e) => {
                            tracing::error!(server = %name, url = %url, error = %e, "Failed to connect MCP server (SSE)");
                        }
                    }
                }
                McpServerConfig::StreamableHttp {
                    streamable_url,
                    headers,
                    oauth,
                } => {
                    let mut effective_headers = headers.clone();
                    // If OAuth config is present, attempt token acquisition
                    if let Some(oauth_config) = oauth {
                        match acquire_oauth_token(oauth_config).await {
                            Ok(token) => {
                                effective_headers.insert(
                                    "Authorization".to_string(),
                                    format!("Bearer {}", token),
                                );
                                tracing::info!(server = %name, "OAuth token acquired for MCP server");
                            }
                            Err(e) => {
                                tracing::warn!(server = %name, error = %e, "OAuth token acquisition failed, trying without auth");
                            }
                        }
                    }
                    match connect_streamable_http_server(name, streamable_url, &effective_headers)
                        .await
                    {
                        Ok(client) => {
                            tracing::info!(server = %name, tools = client.tools.len(), "MCP server connected (streamable HTTP)");
                            clients.push(Arc::new(client));
                        }
                        Err(e) => {
                            tracing::error!(server = %name, url = %streamable_url, error = %e, "Failed to connect MCP server (streamable HTTP)");
                        }
                    }
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
                let tools: Vec<(String, String)> = client
                    .tools
                    .iter()
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

// ═══════════════════════════════════════════════════════════════════════════
//  SSE Transport — connect via HTTP POST (JSON-RPC over SSE)
// ═══════════════════════════════════════════════════════════════════════════

/// Connect to an MCP server via SSE transport.
async fn connect_sse_server(
    name: &str,
    url: &str,
    headers: &HashMap<String, String>,
) -> Result<McpClient, String> {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    // Initialize
    let init_request = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "pipit", "version": env!("CARGO_PKG_VERSION") }
        }
    });

    let mut req = http.post(url).header("Content-Type", "application/json");
    for (key, value) in headers {
        req = req.header(key, value);
    }
    let resp = req
        .json(&init_request)
        .send()
        .await
        .map_err(|e| format!("SSE initialize failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "SSE init HTTP {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        ));
    }

    // Notify initialized
    let mut n = http.post(url).header("Content-Type", "application/json");
    for (k, v) in headers {
        n = n.header(k, v);
    }
    let _ = n
        .json(
            &serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
        )
        .send()
        .await;

    // List tools
    let mut t = http.post(url).header("Content-Type", "application/json");
    for (k, v) in headers {
        t = t.header(k, v);
    }
    let tools_resp = t
        .json(&serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}))
        .send()
        .await
        .map_err(|e| format!("SSE tools/list: {e}"))?;
    let tools_json: Value = tools_resp
        .json()
        .await
        .map_err(|e| format!("SSE tools parse: {e}"))?;

    let tools: Vec<McpToolDef> = tools_json
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| serde_json::from_value(t.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    // Placeholder child for SSE (TODO: refactor McpClient to Transport trait)
    let mut cmd = Command::new("sleep");
    cmd.arg("86400")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| format!("SSE placeholder: {e}"))?;
    let stdin = child.stdin.take().ok_or("No stdin")?;
    let stdout = child.stdout.take().ok_or("No stdout")?;

    Ok(McpClient {
        name: name.to_string(),
        child: Mutex::new(child),
        stdin: Mutex::new(stdin),
        stdout: Mutex::new(BufReader::new(stdout)),
        next_id: Mutex::new(3),
        tools,
    })
}

// ═══════════════════════════════════════════════════════════════════════════
//  Streamable HTTP Transport (MCP 2025-03-26)
// ═══════════════════════════════════════════════════════════════════════════

/// Connect via Streamable HTTP (the default transport in MCP spec 2025-03-26).
async fn connect_streamable_http_server(
    name: &str,
    url: &str,
    headers: &HashMap<String, String>,
) -> Result<McpClient, String> {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let init_request = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {
                "tools": { "listChanged": true },
                "resources": { "subscribe": true, "listChanged": true }
            },
            "clientInfo": { "name": "pipit", "version": env!("CARGO_PKG_VERSION") }
        }
    });

    let mut req = http
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let resp = req
        .json(&init_request)
        .send()
        .await
        .map_err(|e| format!("Streamable HTTP init: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "Streamable HTTP init {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        ));
    }

    let session_id = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    // Notify initialized
    let mut n = http.post(url).header("Content-Type", "application/json");
    for (k, v) in headers {
        n = n.header(k, v);
    }
    if let Some(ref sid) = session_id {
        n = n.header("Mcp-Session-Id", sid);
    }
    let _ = n
        .json(
            &serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
        )
        .send()
        .await;

    // List tools
    let mut t = http
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json");
    for (k, v) in headers {
        t = t.header(k, v);
    }
    if let Some(ref sid) = session_id {
        t = t.header("Mcp-Session-Id", sid);
    }
    let tools_resp = t
        .json(&serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}))
        .send()
        .await
        .map_err(|e| format!("Streamable HTTP tools/list: {e}"))?;
    let tools_json: Value = tools_resp
        .json()
        .await
        .map_err(|e| format!("Streamable HTTP tools parse: {e}"))?;

    let tools: Vec<McpToolDef> = tools_json
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| serde_json::from_value(t.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    let mut cmd = Command::new("sleep");
    cmd.arg("86400")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Streamable HTTP placeholder: {e}"))?;
    let stdin = child.stdin.take().ok_or("No stdin")?;
    let stdout = child.stdout.take().ok_or("No stdout")?;

    Ok(McpClient {
        name: name.to_string(),
        child: Mutex::new(child),
        stdin: Mutex::new(stdin),
        stdout: Mutex::new(BufReader::new(stdout)),
        next_id: Mutex::new(3),
        tools,
    })
}

// ═══════════════════════════════════════════════════════════════════════════
//  OAuth Token Acquisition
// ═══════════════════════════════════════════════════════════════════════════

/// Acquire an OAuth access token using client credentials or cached token.
async fn acquire_oauth_token(config: &McpOAuthConfig) -> Result<String, String> {
    // Try cached token from environment
    if let Ok(token) = std::env::var("PIPIT_MCP_OAUTH_TOKEN") {
        return Ok(token);
    }

    let http = reqwest::Client::new();
    let scopes_joined = config.scopes.join(" ");
    let mut params = vec![
        ("grant_type", "client_credentials"),
        ("client_id", config.client_id.as_str()),
    ];
    if !config.scopes.is_empty() {
        params.push(("scope", &scopes_joined));
    }

    let resp = http
        .post(&config.token_url)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("OAuth request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "OAuth failed ({}): {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        ));
    }

    let json: Value = resp.json().await.map_err(|e| format!("OAuth parse: {e}"))?;
    json.get("access_token")
        .and_then(|t| t.as_str())
        .map(String::from)
        .ok_or_else(|| "No access_token in OAuth response".into())
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
