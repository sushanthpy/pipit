
//! Extended Tool Suite
//!
//! Network and execution tools that complement the typed tool system.
//! Each tool implements the `Tool` trait, provides JSON Schema, and declares
//! a `ResourceSignature` for the conflict-aware scheduler.
//!
//! Active tools registered here: WebFetch, WebSearch.
//! Remaining tools (Sleep, Task, Todo, Config, Brief, Cron, Notebook,
//! ToolSearch, PlanMode, Worktree) have been superseded by typed equivalents.

pub mod extra_tools;
pub mod production_tools;

use async_trait::async_trait;
use pipit_config::ApprovalMode;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolContext, ToolError, ToolResult};

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 1: WebFetch — HTTP GET with content extraction
// ═══════════════════════════════════════════════════════════════════════════

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str { "web_fetch" }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "URL to fetch"},
                "max_bytes": {"type": "integer", "description": "Max response bytes (default: 100000)"},
                "extract_text": {"type": "boolean", "description": "Strip HTML tags, return text only"}
            },
            "required": ["url"]
        })
    }

    fn description(&self) -> &str {
        "Fetch a URL and return its content. Supports text extraction from HTML."
    }

    fn is_mutating(&self) -> bool { false }

    async fn execute(&self, args: Value, _ctx: &ToolContext, cancel: CancellationToken) -> Result<ToolResult, ToolError> {
        let url = args.get("url").and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("url is required".into()))?;
        let max_bytes = args.get("max_bytes").and_then(|v| v.as_u64()).unwrap_or(100_000) as usize;
        let extract_text = args.get("extract_text").and_then(|v| v.as_bool()).unwrap_or(true);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("pipit-cli/0.2.0")
            .build()
            .map_err(|e| ToolError::ExecutionFailed(format!("HTTP client error: {e}")))?;

        let response = tokio::select! {
            r = client.get(url).send() => r.map_err(|e| ToolError::ExecutionFailed(format!("Fetch failed: {e}")))?,
            _ = cancel.cancelled() => return Err(ToolError::ExecutionFailed("Cancelled".into())),
        };

        let status = response.status();
        let content_type = response.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let body = response.bytes().await
            .map_err(|e| ToolError::ExecutionFailed(format!("Read body failed: {e}")))?;

        let body = if body.len() > max_bytes {
            &body[..max_bytes]
        } else {
            &body[..]
        };

        let text = String::from_utf8_lossy(body);

        let output = if extract_text && content_type.contains("html") {
            strip_html_tags(&text)
        } else {
            text.to_string()
        };

        Ok(ToolResult::text(format!(
            "Status: {status}\nContent-Type: {content_type}\nSize: {} bytes\n\n{output}",
            body.len()
        )))
    }
}

fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;

    for ch in html.chars() {
        match ch {
            '<' => {
                in_tag = true;
                // Check for script/style opening
                let rest = &html[html.len().saturating_sub(result.len())..];
                if rest.starts_with("<script") { in_script = true; }
                if rest.starts_with("<style") { in_style = true; }
            }
            '>' => {
                in_tag = false;
                continue;
            }
            _ if in_tag => continue,
            _ if in_script || in_style => {
                // Check for closing tags
                if html[..].contains("</script>") { in_script = false; }
                if html[..].contains("</style>") { in_style = false; }
                continue;
            }
            '\n' | '\r' | '\t' => {
                if !result.ends_with(' ') && !result.ends_with('\n') {
                    result.push('\n');
                }
            }
            _ => result.push(ch),
        }
    }

    // Collapse multiple newlines
    let mut collapsed = String::with_capacity(result.len());
    let mut prev_newline = false;
    for ch in result.chars() {
        if ch == '\n' {
            if !prev_newline {
                collapsed.push(ch);
            }
            prev_newline = true;
        } else {
            prev_newline = false;
            collapsed.push(ch);
        }
    }
    collapsed.trim().to_string()
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 2: WebSearch — Search engine query
// ═══════════════════════════════════════════════════════════════════════════

pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str { "web_search" }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Search query"},
                "max_results": {"type": "integer", "description": "Max results (default: 5)"}
            },
            "required": ["query"]
        })
    }
    fn description(&self) -> &str { "Search the web and return results with titles, URLs, and snippets." }
    fn is_mutating(&self) -> bool { false }

    async fn execute(&self, args: Value, _ctx: &ToolContext, cancel: CancellationToken) -> Result<ToolResult, ToolError> {
        let query = args.get("query").and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("query is required".into()))?;
        let max_results = args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

        // Check for API key in environment
        let api_key = std::env::var("PIPIT_SEARCH_API_KEY")
            .or_else(|_| std::env::var("BRAVE_SEARCH_API_KEY"))
            .or_else(|_| std::env::var("SERPAPI_API_KEY"));

        match api_key {
            Ok(key) => {
                // Brave Search API integration
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(15))
                    .build()
                    .map_err(|e| ToolError::ExecutionFailed(format!("HTTP client error: {e}")))?;

                let url = format!(
                    "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
                    urlencoding::encode(query), max_results
                );

                let response = tokio::select! {
                    r = client.get(&url)
                        .header("Accept", "application/json")
                        .header("Accept-Encoding", "gzip")
                        .header("X-Subscription-Token", &key)
                        .send() => r.map_err(|e| ToolError::ExecutionFailed(format!("Search API error: {e}")))?,
                    _ = cancel.cancelled() => return Err(ToolError::ExecutionFailed("Cancelled".into())),
                };

                if !response.status().is_success() {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    return Ok(ToolResult::text(format!("Search API returned {status}: {body}")));
                }

                let data: Value = response.json().await
                    .map_err(|e| ToolError::ExecutionFailed(format!("Parse search results: {e}")))?;

                let mut results = String::new();
                if let Some(web) = data.get("web").and_then(|w| w.get("results")).and_then(|r| r.as_array()) {
                    for (i, item) in web.iter().take(max_results).enumerate() {
                        let title = item.get("title").and_then(|t| t.as_str()).unwrap_or("(no title)");
                        let url = item.get("url").and_then(|u| u.as_str()).unwrap_or("");
                        let description = item.get("description").and_then(|d| d.as_str()).unwrap_or("");
                        results.push_str(&format!("{}. {}\n   {}\n   {}\n\n", i+1, title, url, description));
                    }
                }

                if results.is_empty() {
                    Ok(ToolResult::text(format!("No results found for: \"{query}\"")))
                } else {
                    Ok(ToolResult::text(format!("Search results for \"{query}\":\n\n{results}")))
                }
            }
            Err(_) => {
                Ok(ToolResult::text(format!(
                    "Search results for: \"{query}\"\n\n\
                     [Web search requires an API key. Set one of:\n\
                     - PIPIT_SEARCH_API_KEY (Brave Search)\n\
                     - BRAVE_SEARCH_API_KEY\n\
                     - SERPAPI_API_KEY\n\
                     Get a free key at https://api.search.brave.com/register]"
                )))
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 3: Sleep — Delay execution
// ═══════════════════════════════════════════════════════════════════════════

pub struct SleepTool;

#[async_trait]
impl Tool for SleepTool {
    fn name(&self) -> &str { "sleep" }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "seconds": {"type": "number", "description": "Seconds to sleep (max 300)"},
                "reason": {"type": "string", "description": "Why waiting (shown to user)"}
            },
            "required": ["seconds"]
        })
    }
    fn description(&self) -> &str { "Wait for a specified duration. Useful for polling or rate limiting." }
    fn is_mutating(&self) -> bool { false }

    async fn execute(&self, args: Value, _ctx: &ToolContext, cancel: CancellationToken) -> Result<ToolResult, ToolError> {
        let seconds = args.get("seconds").and_then(|v| v.as_f64())
            .ok_or_else(|| ToolError::InvalidArgs("seconds is required".into()))?;
        let seconds = seconds.min(300.0).max(0.0);
        let reason = args.get("reason").and_then(|v| v.as_str()).unwrap_or("waiting");

        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs_f64(seconds)) => {},
            _ = cancel.cancelled() => return Err(ToolError::ExecutionFailed("Cancelled".into())),
        }

        Ok(ToolResult::text(format!("Slept for {seconds:.1}s ({reason})")))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 4: Todo — Persistent task list for the current session
// ═══════════════════════════════════════════════════════════════════════════

pub struct TodoTool {
    items: Arc<Mutex<Vec<TodoItem>>>,
    persist_path: Option<PathBuf>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TodoItem {
    id: usize,
    text: String,
    status: TodoStatus,
    created_at: String,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
enum TodoStatus { Pending, InProgress, Done }

impl TodoTool {
    pub fn new() -> Self {
        Self { items: Arc::new(Mutex::new(Vec::new())), persist_path: None }
    }

    pub fn with_project_root(project_root: &std::path::Path) -> Self {
        let path = project_root.join(".pipit").join("todo.json");
        let items = if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|content| serde_json::from_str::<Vec<TodoItem>>(&content).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Self {
            items: Arc::new(Mutex::new(items)),
            persist_path: Some(path),
        }
    }

    fn persist(&self) {
        if let Some(ref path) = self.persist_path {
            let items = self.items.lock().unwrap();
            if let Ok(json) = serde_json::to_string_pretty(&*items) {
                let _ = std::fs::create_dir_all(path.parent().unwrap());
                let _ = std::fs::write(path, json);
            }
        }
    }
}

#[async_trait]
impl Tool for TodoTool {
    fn name(&self) -> &str { "todo" }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["add", "list", "update", "remove"], "description": "Action to perform"},
                "text": {"type": "string", "description": "Todo text (for add)"},
                "id": {"type": "integer", "description": "Todo ID (for update/remove)"},
                "status": {"type": "string", "enum": ["pending", "in_progress", "done"], "description": "New status (for update)"}
            },
            "required": ["action"]
        })
    }
    fn description(&self) -> &str { "Manage a persistent todo list for tracking sub-tasks during a session." }
    fn is_mutating(&self) -> bool { true }

    async fn execute(&self, args: Value, _ctx: &ToolContext, _cancel: CancellationToken) -> Result<ToolResult, ToolError> {
        let action = args.get("action").and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("action is required".into()))?;

        let mut items = self.items.lock().unwrap();

        match action {
            "add" => {
                let text = args.get("text").and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidArgs("text is required for add".into()))?;
                let id = items.len() + 1;
                items.push(TodoItem {
                    id,
                    text: text.to_string(),
                    status: TodoStatus::Pending,
                    created_at: chrono::Utc::now().to_rfc3339(),
                });
                drop(items);
                self.persist();
                Ok(ToolResult::mutating(format!("Added todo #{id}: {text}")))
            }
            "list" => {
                if items.is_empty() {
                    return Ok(ToolResult::text("No todos."));
                }
                let list: Vec<String> = items.iter().map(|item| {
                    let status = match item.status {
                        TodoStatus::Pending => "[ ]",
                        TodoStatus::InProgress => "[~]",
                        TodoStatus::Done => "[x]",
                    };
                    format!("#{} {} {}", item.id, status, item.text)
                }).collect();
                Ok(ToolResult::text(list.join("\n")))
            }
            "update" => {
                let id = args.get("id").and_then(|v| v.as_u64())
                    .ok_or_else(|| ToolError::InvalidArgs("id is required for update".into()))? as usize;
                let status_str = args.get("status").and_then(|v| v.as_str()).unwrap_or("done");
                let status = match status_str {
                    "pending" => TodoStatus::Pending,
                    "in_progress" => TodoStatus::InProgress,
                    "done" => TodoStatus::Done,
                    _ => return Err(ToolError::InvalidArgs(format!("Unknown status: {status_str}"))),
                };
                if let Some(item) = items.iter_mut().find(|i| i.id == id) {
                    item.status = status;
                    drop(items);
                    self.persist();
                    Ok(ToolResult::mutating(format!("Updated todo #{id} → {status_str}")))
                } else {
                    Err(ToolError::InvalidArgs(format!("Todo #{id} not found")))
                }
            }
            "remove" => {
                let id = args.get("id").and_then(|v| v.as_u64())
                    .ok_or_else(|| ToolError::InvalidArgs("id is required for remove".into()))? as usize;
                let before = items.len();
                items.retain(|i| i.id != id);
                if items.len() < before {
                    drop(items);
                    self.persist();
                    Ok(ToolResult::mutating(format!("Removed todo #{id}")))
                } else {
                    Err(ToolError::InvalidArgs(format!("Todo #{id} not found")))
                }
            }
            _ => Err(ToolError::InvalidArgs(format!("Unknown action: {action}"))),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 5: Config — Runtime configuration viewer/editor
// ═══════════════════════════════════════════════════════════════════════════

pub struct ConfigTool;

#[async_trait]
impl Tool for ConfigTool {
    fn name(&self) -> &str { "config" }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["get", "set", "list"], "description": "Action to perform"},
                "key": {"type": "string", "description": "Config key (dot-separated, e.g., 'model.default_model')"},
                "value": {"type": "string", "description": "New value (for set)"}
            },
            "required": ["action"]
        })
    }
    fn description(&self) -> &str { "View or modify runtime configuration settings." }
    fn is_mutating(&self) -> bool { true }

    async fn execute(&self, args: Value, _ctx: &ToolContext, _cancel: CancellationToken) -> Result<ToolResult, ToolError> {
        let action = args.get("action").and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("action is required".into()))?;

        match action {
            "list" => {
                Ok(ToolResult::text(
                    "Available config keys:\n\
                     model.default_model — Current model ID\n\
                     model.context_window — Context window size\n\
                     approval — Permission mode (default/plan/auto/yolo)\n\
                     context.max_turns — Maximum turns per session\n\
                     context.compression_threshold — When to trigger compaction\n\
                     ui.theme — UI theme\n\
                     ui.show_thinking — Show model thinking\n\
                     ui.show_cost — Show cost tracking"
                ))
            }
            "get" => {
                let key = args.get("key").and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidArgs("key is required for get".into()))?;
                // Read from project config file
                let config_path = _ctx.project_root.join(".pipit").join("config.toml");
                if config_path.exists() {
                    let content = tokio::fs::read_to_string(&config_path).await
                        .map_err(|e| ToolError::ExecutionFailed(format!("Read config: {e}")))?;
                    let config: toml::Value = content.parse()
                        .map_err(|e| ToolError::ExecutionFailed(format!("Parse config: {e}")))?;
                    // Navigate dotted key path
                    let parts: Vec<&str> = key.split('.').collect();
                    let mut current = &config;
                    for part in &parts {
                        current = current.get(part)
                            .ok_or_else(|| ToolError::InvalidArgs(format!("Key not found: {key}")))?;
                    }
                    Ok(ToolResult::text(format!("{key} = {current}")))
                } else {
                    Ok(ToolResult::text(format!("No project config at {}. Use 'set' to create.", config_path.display())))
                }
            }
            "set" => {
                let key = args.get("key").and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidArgs("key is required for set".into()))?;
                let value = args.get("value").and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidArgs("value is required for set".into()))?;
                let config_path = _ctx.project_root.join(".pipit").join("config.toml");
                let _ = std::fs::create_dir_all(_ctx.project_root.join(".pipit"));

                let mut config: toml::Value = if config_path.exists() {
                    let content = tokio::fs::read_to_string(&config_path).await
                        .map_err(|e| ToolError::ExecutionFailed(format!("Read config: {e}")))?;
                    content.parse().unwrap_or(toml::Value::Table(toml::map::Map::new()))
                } else {
                    toml::Value::Table(toml::map::Map::new())
                };

                // Set the value via dotted path
                let parts: Vec<&str> = key.split('.').collect();
                let mut current = &mut config;
                for (i, part) in parts.iter().enumerate() {
                    if i == parts.len() - 1 {
                        // Set the value
                        if let toml::Value::Table(table) = current {
                            table.insert(part.to_string(), toml::Value::String(value.to_string()));
                        }
                    } else {
                        // Navigate/create intermediate tables
                        if let toml::Value::Table(table) = current {
                            current = table.entry(part.to_string())
                                .or_insert(toml::Value::Table(toml::map::Map::new()));
                        }
                    }
                }

                let serialized = toml::to_string_pretty(&config)
                    .map_err(|e| ToolError::ExecutionFailed(format!("Serialize config: {e}")))?;
                tokio::fs::write(&config_path, &serialized).await
                    .map_err(|e| ToolError::ExecutionFailed(format!("Write config: {e}")))?;

                Ok(ToolResult::mutating(format!("Set config '{key}' = '{value}' (saved to {})", config_path.display())))
            }
            _ => Err(ToolError::InvalidArgs(format!("Unknown action: {action}"))),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 6: Task — Background task management
// ═══════════════════════════════════════════════════════════════════════════

pub struct TaskTool {
    tasks: Arc<Mutex<Vec<TaskRecord>>>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct TaskRecord {
    id: String,
    description: String,
    status: String,
    task_type: String,
    started_at: String,
    output: Option<String>,
}

impl TaskTool {
    pub fn new() -> Self {
        Self { tasks: Arc::new(Mutex::new(Vec::new())) }
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str { "task" }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["create", "list", "get", "stop"], "description": "Task action"},
                "description": {"type": "string", "description": "Task description (for create)"},
                "task_type": {"type": "string", "enum": ["bash", "agent"], "description": "Task type (for create)"},
                "command": {"type": "string", "description": "Command to run (for bash tasks)"},
                "task_id": {"type": "string", "description": "Task ID (for get/stop)"}
            },
            "required": ["action"]
        })
    }
    fn description(&self) -> &str { "Create and manage background tasks (bash commands, sub-agents)." }
    fn is_mutating(&self) -> bool { true }

    async fn execute(&self, args: Value, ctx: &ToolContext, _cancel: CancellationToken) -> Result<ToolResult, ToolError> {
        let action = args.get("action").and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("action is required".into()))?;

        match action {
            "create" => {
                let desc = args.get("description").and_then(|v| v.as_str()).unwrap_or("unnamed task");
                let task_type = args.get("task_type").and_then(|v| v.as_str()).unwrap_or("bash");
                let command = args.get("command").and_then(|v| v.as_str());
                let id = format!("task_{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap());

                // Create task output directory
                let task_dir = ctx.project_root.join(".pipit").join("tasks");
                let _ = std::fs::create_dir_all(&task_dir);
                let log_path = task_dir.join(format!("{id}.log"));
                let pid_path = task_dir.join(format!("{id}.pid"));

                if task_type == "bash" {
                    if let Some(cmd) = command {
                        // Spawn real background process
                        let log_file = std::fs::File::create(&log_path)
                            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to create log: {e}")))?;
                        let log_err = log_file.try_clone()
                            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to clone log: {e}")))?;

                        let child = tokio::process::Command::new("sh")
                            .args(["-c", cmd])
                            .current_dir(ctx.current_dir())
                            .stdout(std::process::Stdio::from(log_file))
                            .stderr(std::process::Stdio::from(log_err))
                            .spawn()
                            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to spawn: {e}")))?;

                        // Save PID
                        if let Some(pid) = child.id() {
                            let _ = std::fs::write(&pid_path, pid.to_string());
                        }

                        // Track in memory
                        let mut tasks = self.tasks.lock().unwrap();
                        tasks.push(TaskRecord {
                            id: id.clone(),
                            description: desc.to_string(),
                            status: "running".to_string(),
                            task_type: task_type.to_string(),
                            started_at: chrono::Utc::now().to_rfc3339(),
                            output: Some(log_path.to_string_lossy().to_string()),
                        });

                        // Spawn monitor to update status when process exits
                        let tasks_clone = self.tasks.clone();
                        let id_clone = id.clone();
                        tokio::spawn(async move {
                            let mut child = child;
                            let status = child.wait().await;
                            let mut tasks = tasks_clone.lock().unwrap();
                            if let Some(task) = tasks.iter_mut().find(|t| t.id == id_clone) {
                                task.status = match status {
                                    Ok(s) if s.success() => "completed".to_string(),
                                    Ok(s) => format!("failed({})", s.code().unwrap_or(-1)),
                                    Err(e) => format!("error: {e}"),
                                };
                            }
                        });

                        Ok(ToolResult::mutating(format!(
                            "Created background task {id}: {desc}\n  Command: {cmd}\n  Log: {}\n  \
                             Use task_output to read output.",
                            log_path.display()
                        )))
                    } else {
                        Err(ToolError::InvalidArgs("command is required for bash tasks".into()))
                    }
                } else {
                    let mut tasks = self.tasks.lock().unwrap();
                    tasks.push(TaskRecord {
                        id: id.clone(),
                        description: desc.to_string(),
                        status: "running".to_string(),
                        task_type: task_type.to_string(),
                        started_at: chrono::Utc::now().to_rfc3339(),
                        output: None,
                    });
                    Ok(ToolResult::mutating(format!("Created task {id}: {desc}")))
                }
            }
            "list" => {
                let tasks = self.tasks.lock().unwrap();
                if tasks.is_empty() {
                    return Ok(ToolResult::text("No active tasks."));
                }
                let lines: Vec<String> = tasks.iter().map(|t| {
                    format!("{} [{}] {} — {}", t.id, t.status, t.task_type, t.description)
                }).collect();
                Ok(ToolResult::text(lines.join("\n")))
            }
            "get" | "stop" => {
                let task_id = args.get("task_id").and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidArgs("task_id is required".into()))?;
                let mut tasks = self.tasks.lock().unwrap();
                if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                    if action == "stop" {
                        // Kill the process if it has a PID file
                        let pid_path = ctx.project_root.join(".pipit").join("tasks").join(format!("{task_id}.pid"));
                        if pid_path.exists() {
                            if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
                                if let Ok(pid) = pid_str.trim().parse::<i32>() {
                                    // Send SIGTERM
                                    unsafe { libc::kill(pid, libc::SIGTERM); }
                                }
                            }
                        }
                        task.status = "stopped".to_string();
                        Ok(ToolResult::mutating(format!("Stopped task {task_id}")))
                    } else {
                        // Read last 50 lines of log if available
                        let mut info = serde_json::to_string_pretty(task).unwrap_or_default();
                        if let Some(ref log) = task.output {
                            if let Ok(content) = std::fs::read_to_string(log) {
                                let lines: Vec<&str> = content.lines().collect();
                                let start = lines.len().saturating_sub(50);
                                let tail = lines[start..].join("\n");
                                info.push_str(&format!("\n\n--- Last output ---\n{tail}"));
                            }
                        }
                        Ok(ToolResult::text(info))
                    }
                } else {
                    Err(ToolError::InvalidArgs(format!("Task {task_id} not found")))
                }
            }
            _ => Err(ToolError::InvalidArgs(format!("Unknown action: {action}"))),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 7: Brief — Summarize recent session activity
// ═══════════════════════════════════════════════════════════════════════════

pub struct BriefTool;

#[async_trait]
impl Tool for BriefTool {
    fn name(&self) -> &str { "brief" }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "scope": {"type": "string", "enum": ["session", "recent", "changes"], "description": "What to summarize"}
            }
        })
    }
    fn description(&self) -> &str { "Generate a summary of recent session activity, changes made, or conversation context." }
    fn is_mutating(&self) -> bool { false }

    async fn execute(&self, args: Value, ctx: &ToolContext, _cancel: CancellationToken) -> Result<ToolResult, ToolError> {
        let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("session");

        match scope {
            "changes" => {
                // List git changes in project
                let output = tokio::process::Command::new("git")
                    .args(["diff", "--stat", "HEAD"])
                    .current_dir(&ctx.project_root)
                    .output()
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("git diff failed: {e}")))?;
                let diff = String::from_utf8_lossy(&output.stdout);
                if diff.trim().is_empty() {
                    Ok(ToolResult::text("No uncommitted changes."))
                } else {
                    Ok(ToolResult::text(format!("Changes since HEAD:\n{diff}")))
                }
            }
            _ => {
                Ok(ToolResult::text(
                    "Session summary: Use /brief in the REPL for a full summary with \
                     token usage, cost, files modified, and conversation highlights."
                ))
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 8: Cron — Schedule recurring tasks
// ═══════════════════════════════════════════════════════════════════════════

pub struct CronTool {
    schedules: Arc<Mutex<Vec<CronEntry>>>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct CronEntry {
    id: String,
    schedule: String,
    command: String,
    description: String,
    enabled: bool,
}

impl CronTool {
    pub fn new() -> Self {
        Self { schedules: Arc::new(Mutex::new(Vec::new())) }
    }
}

#[async_trait]
impl Tool for CronTool {
    fn name(&self) -> &str { "cron" }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["create", "list", "delete"], "description": "Cron action"},
                "schedule": {"type": "string", "description": "Cron expression (e.g., '*/5 * * * *')"},
                "command": {"type": "string", "description": "Command to run on schedule"},
                "description": {"type": "string", "description": "Description of the scheduled task"},
                "cron_id": {"type": "string", "description": "Cron ID (for delete)"}
            },
            "required": ["action"]
        })
    }
    fn description(&self) -> &str { "Create, list, or delete scheduled recurring tasks." }
    fn is_mutating(&self) -> bool { true }

    async fn execute(&self, args: Value, _ctx: &ToolContext, _cancel: CancellationToken) -> Result<ToolResult, ToolError> {
        let action = args.get("action").and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("action is required".into()))?;

        match action {
            "create" => {
                let schedule = args.get("schedule").and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidArgs("schedule is required".into()))?;
                let command = args.get("command").and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidArgs("command is required".into()))?;
                let desc = args.get("description").and_then(|v| v.as_str()).unwrap_or("scheduled task");
                let id = format!("cron_{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap());
                let mut schedules = self.schedules.lock().unwrap();
                schedules.push(CronEntry {
                    id: id.clone(),
                    schedule: schedule.to_string(),
                    command: command.to_string(),
                    description: desc.to_string(),
                    enabled: true,
                });
                Ok(ToolResult::mutating(format!("Created cron job {id}: '{schedule}' → {command}")))
            }
            "list" => {
                let schedules = self.schedules.lock().unwrap();
                if schedules.is_empty() {
                    return Ok(ToolResult::text("No scheduled tasks."));
                }
                let lines: Vec<String> = schedules.iter().map(|s| {
                    let status = if s.enabled { "active" } else { "paused" };
                    format!("{} [{}] {} — {} ({})", s.id, status, s.schedule, s.description, s.command)
                }).collect();
                Ok(ToolResult::text(lines.join("\n")))
            }
            "delete" => {
                let cron_id = args.get("cron_id").and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidArgs("cron_id is required".into()))?;
                let mut schedules = self.schedules.lock().unwrap();
                let before = schedules.len();
                schedules.retain(|s| s.id != cron_id);
                if schedules.len() < before {
                    Ok(ToolResult::mutating(format!("Deleted cron job {cron_id}")))
                } else {
                    Err(ToolError::InvalidArgs(format!("Cron job {cron_id} not found")))
                }
            }
            _ => Err(ToolError::InvalidArgs(format!("Unknown action: {action}"))),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 9: Team — Local-first team memory and shared context
//
//  Architecture: 3 of 4 actions (create, list, share) work offline via
//  TOML/markdown file I/O. Only `sync` needs the daemon. Secret scanning
//  runs on every `share` write. Graceful degradation when daemon is
//  unavailable.
// ═══════════════════════════════════════════════════════════════════════════

pub struct TeamTool;

#[async_trait]
impl Tool for TeamTool {
    fn name(&self) -> &str { "team" }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["create", "list", "share", "sync"], "description": "Team action: create a team, list teams, share a key-value, or sync with daemon"},
                "name": {"type": "string", "description": "Team name (for create)"},
                "key": {"type": "string", "description": "Shared memory key (for share)"},
                "value": {"type": "string", "description": "Shared memory value (for share)"}
            },
            "required": ["action"]
        })
    }
    fn description(&self) -> &str { "Manage team shared context, conventions, and memory. Create, list, share work offline; sync requires pipit-daemon." }
    fn is_mutating(&self) -> bool { true }

    async fn execute(&self, args: Value, ctx: &ToolContext, _cancel: CancellationToken) -> Result<ToolResult, ToolError> {
        let action = args.get("action").and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("action is required".into()))?;

        let project_root = &ctx.project_root;
        let pipit_dir = project_root.join(".pipit");
        let teams_path = pipit_dir.join("teams.toml");
        let team_dir = pipit_dir.join("team");

        match action {
            "create" => {
                let name = args.get("name").and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidArgs("name is required for create".into()))?;

                // Ensure directories exist
                std::fs::create_dir_all(&team_dir)
                    .map_err(|e| ToolError::ExecutionFailed(format!("Failed to create team dir: {}", e)))?;

                // Read or create teams.toml
                let mut teams: toml::Value = if teams_path.exists() {
                    let content = std::fs::read_to_string(&teams_path)
                        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read teams.toml: {}", e)))?;
                    content.parse::<toml::Value>()
                        .unwrap_or(toml::Value::Table(toml::map::Map::new()))
                } else {
                    toml::Value::Table(toml::map::Map::new())
                };

                // Add team entry
                let team_id = format!("team-{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0000"));
                if let toml::Value::Table(table) = &mut teams {
                    let teams_arr = table.entry("teams".to_string())
                        .or_insert(toml::Value::Array(Vec::new()));
                    if let toml::Value::Array(arr) = teams_arr {
                        let mut entry = toml::map::Map::new();
                        entry.insert("id".to_string(), toml::Value::String(team_id.clone()));
                        entry.insert("name".to_string(), toml::Value::String(name.to_string()));
                        entry.insert("created_at".to_string(), toml::Value::String(
                            chrono::Utc::now().to_rfc3339()
                        ));
                        entry.insert("members".to_string(), toml::Value::Array(Vec::new()));
                        arr.push(toml::Value::Table(entry));
                    }
                }

                // Write teams.toml
                let content = toml::to_string_pretty(&teams)
                    .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize teams: {}", e)))?;
                std::fs::write(&teams_path, &content)
                    .map_err(|e| ToolError::ExecutionFailed(format!("Failed to write teams.toml: {}", e)))?;

                // Create team shared.toml
                let shared_path = team_dir.join("shared.toml");
                if !shared_path.exists() {
                    std::fs::write(&shared_path, "# Team shared context\n[shared]\n")
                        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to create shared.toml: {}", e)))?;
                }

                Ok(ToolResult::text(format!(
                    "Created team '{}' (id: {}). Team config: {}\nShared context: {}",
                    name, team_id,
                    teams_path.display(),
                    team_dir.join("shared.toml").display(),
                )))
            }

            "list" => {
                if !teams_path.exists() {
                    return Ok(ToolResult::text(
                        "No teams configured. Use `team create --name <name>` to create one.".to_string()
                    ));
                }

                let content = std::fs::read_to_string(&teams_path)
                    .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read teams.toml: {}", e)))?;
                let teams: toml::Value = content.parse()
                    .map_err(|e| ToolError::ExecutionFailed(format!("Failed to parse teams.toml: {}", e)))?;

                let mut output = String::from("Teams:\n");
                if let Some(toml::Value::Array(arr)) = teams.get("teams") {
                    for (i, team) in arr.iter().enumerate() {
                        let name = team.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        let id = team.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                        let members = team.get("members")
                            .and_then(|v| v.as_array())
                            .map(|a| a.len())
                            .unwrap_or(0);
                        output.push_str(&format!(
                            "  {}. {} (id: {}, {} members)\n",
                            i + 1, name, id, members
                        ));
                    }
                } else {
                    output.push_str("  (none)\n");
                }

                // Show shared context if present
                let shared_path = team_dir.join("shared.toml");
                if shared_path.exists() {
                    if let Ok(shared_content) = std::fs::read_to_string(&shared_path) {
                        let shared: toml::Value = shared_content.parse().unwrap_or(toml::Value::Table(toml::map::Map::new()));
                        if let Some(toml::Value::Table(table)) = shared.get("shared") {
                            output.push_str(&format!("\nShared context ({} entries):\n", table.len()));
                            for (key, value) in table.iter().take(10) {
                                let val_owned = value.to_string();
                                let val_str = value.as_str().unwrap_or(&val_owned);
                                let preview: String = val_str.chars().take(80).collect();
                                output.push_str(&format!("  {}: {}\n", key, preview));
                            }
                        }
                    }
                }

                Ok(ToolResult::text(output))
            }

            "share" => {
                let key = args.get("key").and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidArgs("key is required for share".into()))?;
                let value = args.get("value").and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidArgs("value is required for share".into()))?;

                // Secret scanning — reject if secrets detected
                if team_tool_scan_secrets(value) {
                    return Ok(ToolResult::text(
                        "REJECTED: The value contains what appears to be a secret (API key, token, password). \
                         Secrets must not be shared via team memory. Use environment variables instead.".to_string()
                    ));
                }

                // Ensure team dir exists
                std::fs::create_dir_all(&team_dir)
                    .map_err(|e| ToolError::ExecutionFailed(format!("Failed to create team dir: {}", e)))?;

                let shared_path = team_dir.join("shared.toml");
                let mut shared: toml::Value = if shared_path.exists() {
                    let content = std::fs::read_to_string(&shared_path)
                        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read shared.toml: {}", e)))?;
                    content.parse().unwrap_or(toml::Value::Table(toml::map::Map::new()))
                } else {
                    let mut root = toml::map::Map::new();
                    root.insert("shared".to_string(), toml::Value::Table(toml::map::Map::new()));
                    toml::Value::Table(root)
                };

                // Set the key-value pair
                if let toml::Value::Table(root) = &mut shared {
                    let section = root.entry("shared".to_string())
                        .or_insert(toml::Value::Table(toml::map::Map::new()));
                    if let toml::Value::Table(table) = section {
                        table.insert(key.to_string(), toml::Value::String(value.to_string()));
                    }
                }

                let content = toml::to_string_pretty(&shared)
                    .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize: {}", e)))?;
                std::fs::write(&shared_path, &content)
                    .map_err(|e| ToolError::ExecutionFailed(format!("Failed to write shared.toml: {}", e)))?;

                Ok(ToolResult::text(format!(
                    "Shared '{key}' to team context at {}",
                    shared_path.display()
                )))
            }

            "sync" => {
                // Sync requires daemon — graceful degradation
                Ok(ToolResult::text(
                    "Sync requires pipit-daemon. Local team features (create, list, share) work without it.\n\
                     To enable sync:\n\
                     1. Start the daemon: pipit daemon start\n\
                     2. Configure sync: set `team.sync_enabled = true` in .pipit/config.toml\n\
                     3. Run `team sync` again to push/pull team shared context.".to_string()
                ))
            }

            other => {
                Ok(ToolResult::text(format!(
                    "Unknown team action '{}'. Available: create, list, share, sync", other
                )))
            }
        }
    }
}

/// Lightweight secret scanner for team shared values.
/// Checks for common secret patterns (API keys, tokens, passwords).
fn team_tool_scan_secrets(value: &str) -> bool {
    let patterns = [
        "sk-",         // OpenAI keys
        "sk-ant-",     // Anthropic keys
        "ghp_",        // GitHub PATs
        "gho_",        // GitHub OAuth
        "glpat-",      // GitLab PAT
        "AKIA",        // AWS access key
        "xoxb-",       // Slack bot token
        "xoxp-",       // Slack user token
        "-----BEGIN",  // PEM private keys
        "Bearer ",     // Bearer tokens
    ];

    // Check for known prefixes
    for pat in &patterns {
        if value.contains(pat) {
            return true;
        }
    }

    // Check for base64-like high entropy strings (>40 chars, alphanumeric)
    if value.len() > 40 {
        let alnum_count = value.chars().filter(|c| c.is_alphanumeric() || *c == '+' || *c == '/' || *c == '=').count();
        if alnum_count as f64 / value.len() as f64 > 0.85 {
            return true;
        }
    }

    false
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 10: NotebookEdit — Jupyter notebook cell editing
// ═══════════════════════════════════════════════════════════════════════════

pub struct NotebookEditTool;

#[async_trait]
impl Tool for NotebookEditTool {
    fn name(&self) -> &str { "notebook_edit" }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to .ipynb file"},
                "action": {"type": "string", "enum": ["read", "edit_cell", "add_cell", "delete_cell"], "description": "Action to perform"},
                "cell_index": {"type": "integer", "description": "Cell index (0-based)"},
                "content": {"type": "string", "description": "New cell content"},
                "cell_type": {"type": "string", "enum": ["code", "markdown"], "description": "Cell type (for add)"}
            },
            "required": ["path", "action"]
        })
    }
    fn description(&self) -> &str { "Read and edit Jupyter notebook cells (.ipynb files)." }
    fn is_mutating(&self) -> bool { true }

    async fn execute(&self, args: Value, ctx: &ToolContext, _cancel: CancellationToken) -> Result<ToolResult, ToolError> {
        let path = args.get("path").and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("path is required".into()))?;
        let action = args.get("action").and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("action is required".into()))?;

        let full_path = ctx.current_dir().join(path);
        let content = tokio::fs::read_to_string(&full_path).await
            .map_err(|e| ToolError::ExecutionFailed(format!("Read notebook failed: {e}")))?;

        let mut notebook: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| ToolError::ExecutionFailed(format!("Parse notebook failed: {e}")))?;

        match action {
            "read" => {
                let cells = notebook.get("cells").and_then(|c| c.as_array())
                    .ok_or_else(|| ToolError::ExecutionFailed("Invalid notebook format".into()))?;
                let mut output = String::new();
                for (i, cell) in cells.iter().enumerate() {
                    let cell_type = cell.get("cell_type").and_then(|t| t.as_str()).unwrap_or("unknown");
                    let source = cell.get("source").and_then(|s| s.as_array())
                        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(""))
                        .unwrap_or_default();
                    output.push_str(&format!("--- Cell {i} [{cell_type}] ---\n{source}\n\n"));
                }
                Ok(ToolResult::text(output))
            }
            "edit_cell" => {
                let cell_index = args.get("cell_index").and_then(|v| v.as_u64())
                    .ok_or_else(|| ToolError::InvalidArgs("cell_index is required".into()))? as usize;
                let new_content = args.get("content").and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidArgs("content is required".into()))?;

                let cells = notebook.get_mut("cells").and_then(|c| c.as_array_mut())
                    .ok_or_else(|| ToolError::ExecutionFailed("Invalid notebook".into()))?;

                if cell_index >= cells.len() {
                    return Err(ToolError::InvalidArgs(format!("Cell index {cell_index} out of range")));
                }

                let lines: Vec<serde_json::Value> = new_content.lines()
                    .map(|l| serde_json::Value::String(format!("{l}\n")))
                    .collect();
                cells[cell_index]["source"] = serde_json::Value::Array(lines);

                let serialized = serde_json::to_string_pretty(&notebook)
                    .map_err(|e| ToolError::ExecutionFailed(format!("Serialize failed: {e}")))?;
                tokio::fs::write(&full_path, &serialized).await
                    .map_err(|e| ToolError::ExecutionFailed(format!("Write failed: {e}")))?;

                Ok(ToolResult::mutating(format!("Edited cell {cell_index} in {path}")))
            }
            "add_cell" => {
                let new_content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let cell_type = args.get("cell_type").and_then(|v| v.as_str()).unwrap_or("code");

                let cells = notebook.get_mut("cells").and_then(|c| c.as_array_mut())
                    .ok_or_else(|| ToolError::ExecutionFailed("Invalid notebook".into()))?;

                let lines: Vec<serde_json::Value> = new_content.lines()
                    .map(|l| serde_json::Value::String(format!("{l}\n")))
                    .collect();

                let new_cell = serde_json::json!({
                    "cell_type": cell_type,
                    "source": lines,
                    "metadata": {},
                    "outputs": []
                });

                cells.push(new_cell);
                let idx = cells.len() - 1;

                let serialized = serde_json::to_string_pretty(&notebook)
                    .map_err(|e| ToolError::ExecutionFailed(format!("Serialize failed: {e}")))?;
                tokio::fs::write(&full_path, &serialized).await
                    .map_err(|e| ToolError::ExecutionFailed(format!("Write failed: {e}")))?;

                Ok(ToolResult::mutating(format!("Added {cell_type} cell at index {idx} in {path}")))
            }
            "delete_cell" => {
                let cell_index = args.get("cell_index").and_then(|v| v.as_u64())
                    .ok_or_else(|| ToolError::InvalidArgs("cell_index is required".into()))? as usize;

                let cells = notebook.get_mut("cells").and_then(|c| c.as_array_mut())
                    .ok_or_else(|| ToolError::ExecutionFailed("Invalid notebook".into()))?;

                if cell_index >= cells.len() {
                    return Err(ToolError::InvalidArgs(format!("Cell index {cell_index} out of range")));
                }

                cells.remove(cell_index);

                let serialized = serde_json::to_string_pretty(&notebook)
                    .map_err(|e| ToolError::ExecutionFailed(format!("Serialize failed: {e}")))?;
                tokio::fs::write(&full_path, &serialized).await
                    .map_err(|e| ToolError::ExecutionFailed(format!("Write failed: {e}")))?;

                Ok(ToolResult::mutating(format!("Deleted cell {cell_index} from {path}")))
            }
            _ => Err(ToolError::InvalidArgs(format!("Unknown action: {action}"))),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tool 11: ToolSearch — TF-IDF search over available tools
// ═══════════════════════════════════════════════════════════════════════════

pub struct ToolSearchTool {
    /// Precomputed tool index: name → (description, schema summary)
    index: Arc<Mutex<Vec<ToolIndexEntry>>>,
}

#[derive(Debug, Clone)]
struct ToolIndexEntry {
    name: String,
    description: String,
    category: String,
}

impl ToolSearchTool {
    pub fn new() -> Self {
        Self { index: Arc::new(Mutex::new(Vec::new())) }
    }

    pub fn rebuild_index(&self, tools: &[(String, String)]) {
        let mut index = self.index.lock().unwrap();
        index.clear();
        for (name, description) in tools {
            let category = categorize_tool(name);
            index.push(ToolIndexEntry {
                name: name.clone(),
                description: description.clone(),
                category,
            });
        }
    }
}

fn categorize_tool(name: &str) -> String {
    match name {
        "bash" | "powershell" | "sleep" => "execution".to_string(),
        "read_file" | "write_file" | "edit_file" | "multi_edit" | "notebook_edit" => "file".to_string(),
        "grep" | "glob" | "list_directory" => "search".to_string(),
        "web_fetch" | "web_search" => "network".to_string(),
        "todo" | "task" | "cron" | "brief" | "team" => "project".to_string(),
        "config" | "skill" | "tool_search" => "meta".to_string(),
        _ if name.starts_with("mcp_") => "mcp".to_string(),
        _ => "other".to_string(),
    }
}

/// Simple TF-IDF cosine similarity for tool search.
/// score(q, d) = Σ(tf(t,q) · idf(t) · tf(t,d) · idf(t)) / (||q|| · ||d||)
fn search_score(query: &str, text: &str) -> f64 {
    let query_terms: Vec<&str> = query.split_whitespace().collect();
    let text_lower = text.to_ascii_lowercase();
    let mut score = 0.0;

    for term in &query_terms {
        let term_lower = term.to_ascii_lowercase();
        // Exact name match gets highest weight
        if text_lower.starts_with(&term_lower) {
            score += 10.0;
        }
        // Substring match
        if text_lower.contains(&term_lower) {
            score += 3.0;
        }
        // Word boundary match
        for word in text_lower.split_whitespace() {
            if word.starts_with(&term_lower) {
                score += 2.0;
            }
        }
    }

    score
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &str { "tool_search" }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Search query to find tools by name or description"},
                "category": {"type": "string", "description": "Optional category filter (execution, file, search, network, project, meta, mcp)"}
            },
            "required": ["query"]
        })
    }
    fn description(&self) -> &str { "Search for available tools by keyword. Use when the tool list is too large to scan." }
    fn is_mutating(&self) -> bool { false }

    async fn execute(&self, args: Value, _ctx: &ToolContext, _cancel: CancellationToken) -> Result<ToolResult, ToolError> {
        let query = args.get("query").and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("query is required".into()))?;
        let category = args.get("category").and_then(|v| v.as_str());

        let index = self.index.lock().unwrap();

        let mut results: Vec<(f64, &ToolIndexEntry)> = index.iter()
            .filter(|e| category.map_or(true, |c| e.category == c))
            .map(|e| {
                let score = search_score(query, &format!("{} {}", e.name, e.description));
                (score, e)
            })
            .filter(|(score, _)| *score > 0.0)
            .collect();

        results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(10);

        if results.is_empty() {
            return Ok(ToolResult::text(format!("No tools found matching '{query}'")));
        }

        let lines: Vec<String> = results.iter().map(|(score, e)| {
            format!("{} [{}] (score: {:.1}) — {}", e.name, e.category, score, e.description)
        }).collect();

        Ok(ToolResult::text(format!("Found {} tools:\n{}", results.len(), lines.join("\n"))))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Enhanced Registry — registers all extended tools
// ═══════════════════════════════════════════════════════════════════════════

/// Register extended tools into an existing registry.
///
/// Only registers tools that are NOT superseded by the typed tool system.
/// Typed equivalents (task, brief, config, sleep, notebook, tool_search,
/// schedule, plan_mode, worktree) are registered via `typed::register_all_typed_tools()`.
pub fn register_extended_tools(registry: &mut crate::ToolRegistry) {
    use std::sync::Arc;
    registry.register(Arc::new(WebFetchTool));
    registry.register(Arc::new(WebSearchTool));
    // Extra tools (PowerShell, REPL, Skill, LSP, RemoteTrigger)
    extra_tools::register_extra_tools(registry);
    // MCP resources, Auth, SendMessage, TaskOutput
    production_tools::register_production_tools(registry);
}
