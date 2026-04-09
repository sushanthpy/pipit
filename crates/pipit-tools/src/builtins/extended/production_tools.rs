//! Production tools — MCP resources, auth, messaging, and task output.
//!
//! Active tools registered here:
//!   1. ListMcpResources / ReadMcpResource — MCP resource protocol
//!   2. McpAuth — OAuth flow trigger
//!   3. SendMessage — inter-agent messaging
//!   4. TaskOutput — read background task output
//!   5. FileStateCache — stale-write detection shared state
//!
//! PlanMode and Worktree tools now live in the typed tool system.

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolContext, ToolDisplay, ToolError, ToolResult};

// ═══════════════════════════════════════════════════════════════════════════
//  Shared state: FileStateCache for stale-write detection
// ═══════════════════════════════════════════════════════════════════════════

/// Content hash + mtime cache for stale-write detection.
/// ReadFileTool records hashes; WriteFileTool checks them before writing.
#[derive(Debug, Clone, Default)]
pub struct FileStateCache {
    entries: Arc<Mutex<HashMap<PathBuf, FileStateEntry>>>,
}

#[derive(Debug, Clone)]
struct FileStateEntry {
    content_hash: u64,
    recorded_at: std::time::Instant,
}

impl FileStateCache {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Record the content hash of a file after reading it.
    pub fn record(&self, path: &std::path::Path, content: &str) {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut hasher);
        let hash = hasher.finish();
        let mut entries = self.entries.lock().unwrap();
        entries.insert(
            path.to_path_buf(),
            FileStateEntry {
                content_hash: hash,
                recorded_at: std::time::Instant::now(),
            },
        );
    }

    /// Check if a file has been modified since we last read it.
    /// Returns Ok(()) if safe to write, Err(message) if stale.
    pub fn check_stale(&self, path: &std::path::Path, current_content: &str) -> Result<(), String> {
        use std::hash::{Hash, Hasher};
        let entries = self.entries.lock().unwrap();
        if let Some(entry) = entries.get(path) {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            current_content.hash(&mut hasher);
            let current_hash = hasher.finish();
            if current_hash != entry.content_hash {
                return Err(format!(
                    "File {} was modified since last read ({:.1}s ago). \
                     Read the file again before writing to avoid overwriting changes.",
                    path.display(),
                    entry.recorded_at.elapsed().as_secs_f64()
                ));
            }
        }
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 1: EnterPlanMode — switch to read-only planning mode
// ═══════════════════════════════════════════════════════════════════════════

pub struct EnterPlanModeTool {
    mode_stack: Arc<Mutex<Vec<String>>>,
}

impl EnterPlanModeTool {
    pub fn new(mode_stack: Arc<Mutex<Vec<String>>>) -> Self {
        Self { mode_stack }
    }
}

#[async_trait]
impl Tool for EnterPlanModeTool {
    fn name(&self) -> &str {
        "enter_plan_mode"
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "description": "Why switching to plan mode (e.g., 'exploring codebase before making changes')"
                }
            }
        })
    }
    fn description(&self) -> &str {
        "Switch to plan mode (read-only). In plan mode, only read tools (read_file, grep, glob, \
         list_directory) and planning are allowed. Write/execute tools are blocked. \
         Use this to safely explore before making changes."
    }
    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let reason = args
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("planning");
        let mut stack = self.mode_stack.lock().unwrap();
        let current_mode = format!("{:?}", ctx.approval_mode);
        stack.push(current_mode.clone());
        Ok(ToolResult::text(format!(
            "Entered plan mode (saved previous mode: {current_mode}). \
             Only read-only tools are available. Reason: {reason}\n\
             Call exit_plan_mode to restore full capabilities."
        )))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 2: ExitPlanMode — restore previous mode
// ═══════════════════════════════════════════════════════════════════════════

pub struct ExitPlanModeTool {
    mode_stack: Arc<Mutex<Vec<String>>>,
}

impl ExitPlanModeTool {
    pub fn new(mode_stack: Arc<Mutex<Vec<String>>>) -> Self {
        Self { mode_stack }
    }
}

#[async_trait]
impl Tool for ExitPlanModeTool {
    fn name(&self) -> &str {
        "exit_plan_mode"
    }
    fn schema(&self) -> Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn description(&self) -> &str {
        "Exit plan mode and restore previous permission mode. \
         Write and execute tools become available again."
    }
    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        _args: Value,
        _ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let mut stack = self.mode_stack.lock().unwrap();
        if let Some(previous) = stack.pop() {
            Ok(ToolResult::text(format!(
                "Exited plan mode. Restored mode: {previous}"
            )))
        } else {
            Ok(ToolResult::text("Not in plan mode — no mode to restore."))
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 3: EnterWorktree — create git worktree for sandboxed editing
// ═══════════════════════════════════════════════════════════════════════════

pub struct EnterWorktreeTool;

#[async_trait]
impl Tool for EnterWorktreeTool {
    fn name(&self) -> &str {
        "enter_worktree"
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "branch": {
                    "type": "string",
                    "description": "Branch name for the worktree (default: auto-generated)"
                },
                "reason": {
                    "type": "string",
                    "description": "Why creating a worktree (e.g., 'testing risky changes')"
                }
            }
        })
    }
    fn description(&self) -> &str {
        "Create an isolated git worktree for sandboxed editing. Changes in the worktree \
         don't affect the main working directory. Use exit_worktree to discard or merge."
    }
    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let branch = args
            .get("branch")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                format!(
                    "pipit-wt-{}",
                    uuid::Uuid::new_v4().to_string().split('-').next().unwrap()
                )
            });
        let reason = args
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("sandboxed editing");

        let worktree_path = ctx
            .project_root
            .join(".pipit")
            .join("worktrees")
            .join(&branch);

        // Route through VCS gateway — validates via kernel FSM + firewall
        let mut gateway = pipit_vcs::VcsGateway::new(ctx.project_root.clone());
        gateway
            .create_worktree_async(&branch, &branch, &worktree_path, "HEAD")
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("VCS gateway: {e}")))?;

        // Switch cwd to worktree
        ctx.set_cwd(worktree_path.clone());

        Ok(ToolResult::mutating(format!(
            "Created worktree at {} on branch '{branch}' ({reason})\n\
             Working directory is now the worktree. Use exit_worktree to return.",
            worktree_path.display()
        )))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 4: ExitWorktree — remove worktree and restore cwd
// ═══════════════════════════════════════════════════════════════════════════

pub struct ExitWorktreeTool;

#[async_trait]
impl Tool for ExitWorktreeTool {
    fn name(&self) -> &str {
        "exit_worktree"
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["discard", "keep", "merge"],
                    "description": "What to do with worktree changes: discard (rm), keep (detach), merge (merge branch)"
                }
            }
        })
    }
    fn description(&self) -> &str {
        "Exit the current git worktree. Choose to discard changes, keep the branch, or merge into main."
    }
    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("discard");
        let cwd = ctx.current_dir();

        // Check if we're actually in a worktree
        let worktree_check = tokio::process::Command::new("git")
            .args(["rev-parse", "--git-common-dir"])
            .current_dir(&cwd)
            .output()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("git check failed: {e}")))?;

        let common_dir = String::from_utf8_lossy(&worktree_check.stdout)
            .trim()
            .to_string();
        let git_dir = tokio::process::Command::new("git")
            .args(["rev-parse", "--git-dir"])
            .current_dir(&cwd)
            .output()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("git check failed: {e}")))?;
        let git_dir_str = String::from_utf8_lossy(&git_dir.stdout).trim().to_string();

        if common_dir == git_dir_str {
            return Err(ToolError::ExecutionFailed("Not in a worktree".into()));
        }

        // Get the branch name
        let branch_output = tokio::process::Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&cwd)
            .output()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("git branch failed: {e}")))?;
        let branch = String::from_utf8_lossy(&branch_output.stdout)
            .trim()
            .to_string();

        // Restore cwd to project root
        ctx.set_cwd(ctx.project_root.clone());

        // Route through VCS gateway
        let mut gateway = pipit_vcs::VcsGateway::new(ctx.project_root.clone());
        let workspace_id = branch.clone();

        match action {
            "discard" => {
                gateway
                    .remove_worktree_async(&workspace_id, &cwd, &branch, true)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("VCS gateway: {e}")))?;
                Ok(ToolResult::mutating(format!(
                    "Discarded worktree and branch '{branch}'. Returned to project root."
                )))
            }
            "keep" => {
                gateway
                    .remove_worktree_async(&workspace_id, &cwd, &branch, false)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("VCS gateway: {e}")))?;
                Ok(ToolResult::mutating(format!(
                    "Removed worktree but kept branch '{branch}'. Returned to project root."
                )))
            }
            "merge" => {
                // Detect current branch of project root for merge target
                let target = gateway
                    .current_branch()
                    .unwrap_or_else(|_| "main".to_string());
                let result = gateway
                    .merge_worktree_async(&workspace_id, &cwd, &branch, &target)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("VCS gateway: {e}")))?;
                Ok(ToolResult::mutating(result))
            }
            _ => Err(ToolError::InvalidArgs(format!("Unknown action: {action}"))),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 5: ListMcpResources — query MCP server resources
// ═══════════════════════════════════════════════════════════════════════════

pub struct ListMcpResourcesTool;

#[async_trait]
impl Tool for ListMcpResourcesTool {
    fn name(&self) -> &str {
        "list_mcp_resources"
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "server": {
                    "type": "string",
                    "description": "MCP server name (from .pipit/mcp.json)"
                },
                "cursor": {
                    "type": "string",
                    "description": "Pagination cursor from previous response"
                }
            },
            "required": ["server"]
        })
    }
    fn description(&self) -> &str {
        "List available resources from an MCP server. Resources are files, database records, \
         API endpoints, or other structured data exposed by the server."
    }
    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let server = args
            .get("server")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("server name is required".into()))?;
        let cursor = args.get("cursor").and_then(|v| v.as_str());

        // Load MCP config and find the server
        let config = crate::mcp::load_mcp_config(&ctx.project_root).ok_or_else(|| {
            ToolError::ExecutionFailed(
                "No MCP configuration found. Create .pipit/mcp.json to configure MCP servers."
                    .into(),
            )
        })?;

        let server_config = config.mcp_servers.get(server).ok_or_else(|| {
            ToolError::ExecutionFailed(format!(
                "MCP server '{}' not found in config. Available: {}",
                server,
                config
                    .mcp_servers
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;

        // Connect to the server based on transport type
        let client = match server_config {
            crate::mcp::McpServerConfig::Stdio { command, args, env } => {
                crate::mcp::McpClient::connect_stdio(server, command, args, env)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("MCP connect failed: {e}")))?
            }
            crate::mcp::McpServerConfig::Sse { .. } => {
                return Err(ToolError::ExecutionFailed(
                    "SSE transport not supported for resources/list. Use stdio servers.".into(),
                ));
            }
            crate::mcp::McpServerConfig::StreamableHttp { .. } => {
                return Err(ToolError::ExecutionFailed(
                    "Streamable HTTP transport not supported for resources/list. Use stdio servers.".into()
                ));
            }
        };

        let mut params = serde_json::Map::new();
        if let Some(c) = cursor {
            params.insert("cursor".to_string(), Value::String(c.to_string()));
        }

        let result = client
            .call_method("resources/list", Some(Value::Object(params)))
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("resources/list failed: {e}")))?;

        // Format the resources
        let resources = result.get("resources").and_then(|r| r.as_array());
        let next_cursor = result.get("nextCursor").and_then(|c| c.as_str());

        match resources {
            Some(resources) if !resources.is_empty() => {
                let formatted: Vec<String> = resources
                    .iter()
                    .map(|r| {
                        let uri = r.get("uri").and_then(|v| v.as_str()).unwrap_or("?");
                        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or(uri);
                        let desc = r.get("description").and_then(|v| v.as_str());
                        match desc {
                            Some(d) => format!("  - {name} ({uri}): {d}"),
                            None => format!("  - {name} ({uri})"),
                        }
                    })
                    .collect();
                let mut output = format!(
                    "Resources from '{server}' ({} items):\n{}",
                    formatted.len(),
                    formatted.join("\n")
                );
                if let Some(c) = next_cursor {
                    output.push_str(&format!(
                        "\n\n[More results available — use cursor: \"{c}\"]"
                    ));
                }
                Ok(ToolResult::text(output))
            }
            _ => Ok(ToolResult::text(format!(
                "No resources found on MCP server '{server}'."
            ))),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 6: ReadMcpResource — fetch a specific MCP resource
// ═══════════════════════════════════════════════════════════════════════════

pub struct ReadMcpResourceTool;

#[async_trait]
impl Tool for ReadMcpResourceTool {
    fn name(&self) -> &str {
        "read_mcp_resource"
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "server": {
                    "type": "string",
                    "description": "MCP server name"
                },
                "uri": {
                    "type": "string",
                    "description": "Resource URI (from list_mcp_resources)"
                }
            },
            "required": ["server", "uri"]
        })
    }
    fn description(&self) -> &str {
        "Read a specific resource from an MCP server by URI. Returns the resource content."
    }
    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let server = args
            .get("server")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("server name is required".into()))?;
        let uri = args
            .get("uri")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("resource URI is required".into()))?;

        // Load MCP config and find the server
        let config = crate::mcp::load_mcp_config(&ctx.project_root).ok_or_else(|| {
            ToolError::ExecutionFailed(
                "No MCP configuration found. Create .pipit/mcp.json to configure MCP servers."
                    .into(),
            )
        })?;

        let server_config = config.mcp_servers.get(server).ok_or_else(|| {
            ToolError::ExecutionFailed(format!("MCP server '{}' not found in config.", server))
        })?;

        let client = match server_config {
            crate::mcp::McpServerConfig::Stdio { command, args, env } => {
                crate::mcp::McpClient::connect_stdio(server, command, args, env)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("MCP connect failed: {e}")))?
            }
            crate::mcp::McpServerConfig::Sse { .. } => {
                return Err(ToolError::ExecutionFailed(
                    "SSE transport not supported for resources/read. Use stdio servers.".into(),
                ));
            }
            crate::mcp::McpServerConfig::StreamableHttp { .. } => {
                return Err(ToolError::ExecutionFailed(
                    "Streamable HTTP not supported for resources/read. Use stdio servers.".into(),
                ));
            }
        };

        let result = client
            .call_method("resources/read", Some(serde_json::json!({"uri": uri})))
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("resources/read failed: {e}")))?;

        // MCP resource read returns contents array
        let contents = result.get("contents").and_then(|c| c.as_array());
        match contents {
            Some(contents) if !contents.is_empty() => {
                let mut output = String::new();
                for item in contents {
                    let content_uri = item.get("uri").and_then(|v| v.as_str()).unwrap_or(uri);
                    let mime = item
                        .get("mimeType")
                        .and_then(|v| v.as_str())
                        .unwrap_or("text/plain");
                    if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                        output.push_str(&format!("[{content_uri} ({mime})]\n{text}\n"));
                    } else if let Some(blob) = item.get("blob").and_then(|v| v.as_str()) {
                        output.push_str(&format!("[{content_uri} ({mime}, base64)]\n{blob}\n"));
                    }
                }
                Ok(ToolResult::text(output))
            }
            _ => Ok(ToolResult::text(format!(
                "Resource '{uri}' returned no content."
            ))),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 7: McpAuth — trigger OAuth flow for MCP server
// ═══════════════════════════════════════════════════════════════════════════

pub struct McpAuthTool;

#[async_trait]
impl Tool for McpAuthTool {
    fn name(&self) -> &str {
        "mcp_auth"
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "server": {
                    "type": "string",
                    "description": "MCP server name requiring authentication"
                },
                "auth_url": {
                    "type": "string",
                    "description": "Authorization URL (from elicitation error)"
                }
            },
            "required": ["server"]
        })
    }
    fn description(&self) -> &str {
        "Authenticate with an MCP server that requires OAuth. Triggers the PKCE flow: \
         opens browser for user authorization, captures callback, exchanges code for tokens."
    }
    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let server = args
            .get("server")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("server name is required".into()))?;
        let auth_url = args.get("auth_url").and_then(|v| v.as_str());

        if let Some(url) = auth_url {
            // Open browser for OAuth
            let open_cmd = if cfg!(target_os = "macos") {
                "open"
            } else if cfg!(target_os = "linux") {
                "xdg-open"
            } else {
                "start"
            };

            let _ = tokio::process::Command::new(open_cmd).arg(url).spawn();

            Ok(ToolResult::mutating(format!(
                "OAuth flow initiated for MCP server '{server}':\n\
                 1. Browser opened to authorization URL\n\
                 2. Complete authorization in browser\n\
                 3. Pipit will capture the callback and store the token\n\
                 URL: {url}"
            )))
        } else {
            // Check stored credentials
            let cred_path = ctx
                .project_root
                .join(".pipit")
                .join("credentials")
                .join(format!("{server}.json"));
            if cred_path.exists() {
                Ok(ToolResult::text(format!(
                    "MCP server '{server}' has stored credentials at {}",
                    cred_path.display()
                )))
            } else {
                Ok(ToolResult::text(format!(
                    "No credentials for MCP server '{server}'. \
                     Trigger OAuth by providing auth_url, or configure API key in .pipit/mcp.json"
                )))
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 8: SendMessage — inter-agent communication
// ═══════════════════════════════════════════════════════════════════════════

pub struct SendMessageTool {
    outbox: Arc<Mutex<Vec<AgentMessage>>>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct AgentMessage {
    id: String,
    from: String,
    to: String,
    content: String,
    timestamp: String,
}

impl SendMessageTool {
    pub fn new() -> Self {
        Self {
            outbox: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "send_message"
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "target_agent": {
                    "type": "string",
                    "description": "Name or ID of the target agent"
                },
                "message": {
                    "type": "string",
                    "description": "Message content to send"
                },
                "priority": {
                    "type": "string",
                    "enum": ["normal", "high", "urgent"],
                    "description": "Message priority"
                }
            },
            "required": ["target_agent", "message"]
        })
    }
    fn description(&self) -> &str {
        "Send a message to another agent in coordinator mode. The target agent \
         receives the message in its context and can respond via the parent coordinator."
    }
    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        args: Value,
        _ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let target = args
            .get("target_agent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("target_agent is required".into()))?;
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("message is required".into()))?;
        let priority = args
            .get("priority")
            .and_then(|v| v.as_str())
            .unwrap_or("normal");

        let msg = AgentMessage {
            id: uuid::Uuid::new_v4().to_string(),
            from: "current".to_string(),
            to: target.to_string(),
            content: message.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };

        let mut outbox = self.outbox.lock().unwrap();
        outbox.push(msg.clone());

        Ok(ToolResult::mutating(format!(
            "Message sent to agent '{target}' (priority: {priority}):\n  {message}\n\
             Message ID: {}",
            msg.id
        )))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 9: TaskOutput — read background task output
// ═══════════════════════════════════════════════════════════════════════════

pub struct TaskOutputTool {
    output_offsets: Arc<Mutex<HashMap<String, usize>>>,
}

impl TaskOutputTool {
    pub fn new() -> Self {
        Self {
            output_offsets: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl Tool for TaskOutputTool {
    fn name(&self) -> &str {
        "task_output"
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "Task ID to read output from"
                },
                "tail": {
                    "type": "integer",
                    "description": "Number of lines from the end (default: all new output)"
                }
            },
            "required": ["task_id"]
        })
    }
    fn description(&self) -> &str {
        "Read output from a running or completed background task. Returns new output \
         since last read (incremental). Use 'tail' to get the last N lines."
    }
    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let task_id = args
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("task_id is required".into()))?;
        let tail = args
            .get("tail")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);

        // Check for task log file
        let log_path = ctx
            .project_root
            .join(".pipit")
            .join("tasks")
            .join(format!("{task_id}.log"));
        if !log_path.exists() {
            return Err(ToolError::ExecutionFailed(format!(
                "No output file for task '{task_id}'. Task may not have started or log path not configured."
            )));
        }

        let content = tokio::fs::read_to_string(&log_path)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read task log: {e}")))?;

        let output = {
            let mut offsets = self.output_offsets.lock().unwrap();
            let offset = offsets.get(task_id).copied().unwrap_or(0);

            if let Some(n) = tail {
                let lines: Vec<&str> = content.lines().collect();
                let start = lines.len().saturating_sub(n);
                lines[start..].join("\n")
            } else {
                if offset < content.len() {
                    let new_content = &content[offset..];
                    offsets.insert(task_id.to_string(), content.len());
                    new_content.to_string()
                } else {
                    "(no new output)".to_string()
                }
            }
        }; // MutexGuard dropped here before any await

        // Also check if process is still running
        let pid_path = ctx
            .project_root
            .join(".pipit")
            .join("tasks")
            .join(format!("{task_id}.pid"));
        let status = if pid_path.exists() {
            let pid_str = tokio::fs::read_to_string(&pid_path)
                .await
                .unwrap_or_default();
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                // Check if process is alive
                let alive = tokio::process::Command::new("kill")
                    .args(["-0", &pid.to_string()])
                    .output()
                    .await
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                if alive { "running" } else { "completed" }
            } else {
                "unknown"
            }
        } else {
            "no-pid"
        };

        Ok(ToolResult::text(format!(
            "[Task {task_id} — {status}]\n{output}"
        )))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Registration
// ═══════════════════════════════════════════════════════════════════════════

/// Register production-parity tools (non-overlapping with typed tools).
///
/// PlanMode and Worktree are now handled by typed tools (plan_mode, worktree).
pub fn register_production_tools(registry: &mut crate::ToolRegistry) {
    registry.register(Arc::new(ListMcpResourcesTool));
    registry.register(Arc::new(ReadMcpResourceTool));
    registry.register(Arc::new(McpAuthTool));
    registry.register(Arc::new(SendMessageTool::new()));
    registry.register(Arc::new(TaskOutputTool::new()));
    registry.register(Arc::new(A2ADiscoverTool));
}

// ═══════════════════════════════════════════════════════════════════════════
//  A2A (Agent-to-Agent) Discovery Tool
// ═══════════════════════════════════════════════════════════════════════════

pub struct A2ADiscoverTool;

#[async_trait]
impl crate::Tool for A2ADiscoverTool {
    fn name(&self) -> &str {
        "a2a_discover"
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["discover", "send_task", "card"],
                    "description": "discover: find agent at URL, send_task: send a task to an agent, card: show this agent's card"
                },
                "url": { "type": "string", "description": "Base URL of the remote agent" },
                "task": { "type": "string", "description": "Task message to send (for send_task)" }
            },
            "required": ["action"]
        })
    }
    fn description(&self) -> &str {
        "Interact with A2A (Agent-to-Agent) protocol agents. Discover remote agents, send tasks, or view this agent's card."
    }
    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        args: Value,
        _ctx: &crate::ToolContext,
        _cancel: CancellationToken,
    ) -> Result<crate::ToolResult, crate::ToolError> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("card");
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| crate::ToolError::ExecutionFailed(format!("HTTP client: {e}")))?;

        match action {
            "card" => {
                let card = serde_json::json!({
                    "name": "pipit",
                    "description": "AI coding agent with terminal-native tool execution",
                    "version": env!("CARGO_PKG_VERSION"),
                    "capabilities": { "streaming": true, "pushNotifications": false, "stateTransitionHistory": true },
                    "skills": [
                        { "id": "code-edit", "name": "Code Editing", "description": "Edit files with precision" },
                        { "id": "bash-exec", "name": "Shell Execution", "description": "Execute shell commands" },
                        { "id": "code-review", "name": "Code Review", "description": "Review changes" },
                        { "id": "bug-fix", "name": "Bug Fixing", "description": "Diagnose and fix bugs" }
                    ],
                    "defaultInputModes": ["text"],
                    "defaultOutputModes": ["text"]
                });
                Ok(crate::ToolResult::text(format!(
                    "This agent's A2A card:\n{}",
                    serde_json::to_string_pretty(&card).unwrap()
                )))
            }
            "discover" => {
                let url = args.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
                    crate::ToolError::InvalidArgs("url required for discover".into())
                })?;
                let discovery_url = format!("{}/.well-known/agent.json", url.trim_end_matches('/'));
                match http.get(&discovery_url).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        let body = resp.text().await.unwrap_or_default();
                        Ok(crate::ToolResult::text(format!(
                            "Remote agent discovered:\n{}",
                            body
                        )))
                    }
                    Ok(resp) => Ok(crate::ToolResult::error(format!(
                        "A2A discovery failed: HTTP {}",
                        resp.status()
                    ))),
                    Err(e) => Ok(crate::ToolResult::error(format!(
                        "A2A discovery failed: {}",
                        e
                    ))),
                }
            }
            "send_task" => {
                let url = args.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
                    crate::ToolError::InvalidArgs("url required for send_task".into())
                })?;
                let task_text = args.get("task").and_then(|v| v.as_str()).ok_or_else(|| {
                    crate::ToolError::InvalidArgs("task required for send_task".into())
                })?;

                let task_body = serde_json::json!({
                    "id": uuid::Uuid::new_v4().to_string(),
                    "message": {
                        "role": "user",
                        "parts": [{ "type": "text", "text": task_text }]
                    }
                });
                let task_url = format!("{}/tasks/send", url.trim_end_matches('/'));
                match http.post(&task_url).json(&task_body).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        let body = resp.text().await.unwrap_or_default();
                        Ok(crate::ToolResult::text(format!(
                            "A2A task sent. Response:\n{}",
                            body
                        )))
                    }
                    Ok(resp) => Ok(crate::ToolResult::error(format!(
                        "A2A send_task failed: HTTP {}",
                        resp.status()
                    ))),
                    Err(e) => Ok(crate::ToolResult::error(format!(
                        "A2A send_task failed: {}",
                        e
                    ))),
                }
            }
            other => Ok(crate::ToolResult::error(format!(
                "Unknown A2A action: {}",
                other
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;
    use pipit_config::ApprovalMode;

    #[test]
    fn file_state_cache_detects_stale() {
        let cache = FileStateCache::new();
        let path = std::path::Path::new("/tmp/test.txt");
        cache.record(path, "original content");
        assert!(cache.check_stale(path, "original content").is_ok());
        assert!(cache.check_stale(path, "modified content").is_err());
    }

    #[test]
    fn file_state_cache_allows_unknown() {
        let cache = FileStateCache::new();
        let path = std::path::Path::new("/tmp/unknown.txt");
        assert!(cache.check_stale(path, "any content").is_ok());
    }

    #[test]
    fn all_production_tools_have_schemas() {
        let mode_stack = Arc::new(Mutex::new(Vec::new()));
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(EnterPlanModeTool::new(mode_stack.clone())),
            Box::new(ExitPlanModeTool::new(mode_stack)),
            Box::new(EnterWorktreeTool),
            Box::new(ExitWorktreeTool),
            Box::new(ListMcpResourcesTool),
            Box::new(ReadMcpResourceTool),
            Box::new(McpAuthTool),
            Box::new(SendMessageTool::new()),
            Box::new(TaskOutputTool::new()),
        ];
        for tool in &tools {
            let schema = tool.schema();
            assert!(
                schema.get("type").is_some(),
                "Tool {} missing type",
                tool.name()
            );
            assert!(
                schema.get("properties").is_some(),
                "Tool {} missing properties",
                tool.name()
            );
        }
    }

    #[tokio::test]
    async fn plan_mode_stack_works() {
        let mode_stack = Arc::new(Mutex::new(Vec::new()));
        let enter = EnterPlanModeTool::new(mode_stack.clone());
        let exit = ExitPlanModeTool::new(mode_stack.clone());
        let ctx = ToolContext::new(std::env::temp_dir(), ApprovalMode::FullAuto);
        let cancel = CancellationToken::new();

        let r = enter
            .execute(serde_json::json!({"reason": "test"}), &ctx, cancel.clone())
            .await
            .unwrap();
        assert!(r.content.contains("Entered plan mode"));

        let r = exit
            .execute(serde_json::json!({}), &ctx, cancel)
            .await
            .unwrap();
        assert!(r.content.contains("Exited plan mode"));
    }
}
