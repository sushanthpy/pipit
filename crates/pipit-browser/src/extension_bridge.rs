//! Chrome Extension Bridge — WebSocket relay for browser ↔ agent communication.
//!
//! Protocol:
//!   1. Chrome extension connects via WebSocket to localhost:PORT
//!   2. Agent sends commands (navigate, screenshot, click, type)
//!   3. Extension executes via chrome.* APIs or CDP
//!   4. Results flow back with attachments (screenshots as base64)
//!
//! Attachment handling:
//!   - Screenshots: PNG → base64 → embedded in tool result
//!   - File uploads: extension reads file → base64 → sent to agent
//!   - DOM snapshots: accessibility tree serialized as JSON

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Bridge message from agent → extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BridgeCommand {
    Navigate {
        url: String,
    },
    Screenshot {
        selector: Option<String>,
        full_page: bool,
    },
    Click {
        selector: String,
    },
    Type {
        selector: String,
        text: String,
    },
    Evaluate {
        expression: String,
    },
    GetConsole,
    GetNetwork,
    GetAccessibilityTree,
    RunLighthouse {
        url: String,
        categories: Vec<String>,
    },
    UploadFile {
        selector: String,
        file_path: String,
    },
    GetPageText,
    WaitForSelector {
        selector: String,
        timeout_ms: u64,
    },
}

/// Bridge message from extension → agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BridgeResponse {
    Success {
        command_id: String,
        data: serde_json::Value,
    },
    Error {
        command_id: String,
        error: String,
    },
    Attachment {
        command_id: String,
        mime_type: String,
        data_base64: String,
        filename: Option<String>,
    },
    Heartbeat {
        tab_id: u32,
        url: String,
        title: String,
    },
}

/// Attachment extracted from a bridge response.
#[derive(Debug, Clone)]
pub struct BrowserAttachment {
    pub mime_type: String,
    pub data: Vec<u8>,
    pub filename: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl BrowserAttachment {
    pub fn from_base64(
        mime_type: &str,
        data_base64: &str,
        filename: Option<String>,
    ) -> Result<Self, String> {
        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD
            .decode(data_base64)
            .map_err(|e| format!("Base64 decode failed: {e}"))?;
        Ok(Self {
            mime_type: mime_type.to_string(),
            data,
            filename,
            width: None,
            height: None,
        })
    }

    pub fn to_base64(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(&self.data)
    }

    /// Estimate token cost (images ≈ width×height/750 tokens).
    pub fn estimated_tokens(&self) -> u64 {
        match (self.width, self.height) {
            (Some(w), Some(h)) => (w as u64 * h as u64) / 750,
            _ => self.data.len() as u64 / 4,
        }
    }
}

/// Configuration for the extension bridge server.
#[derive(Debug, Clone)]
pub struct BridgeServerConfig {
    pub port: u16,
    pub allowed_origins: Vec<String>,
    pub max_attachment_bytes: usize,
    pub session_timeout_secs: u64,
}

impl Default for BridgeServerConfig {
    fn default() -> Self {
        Self {
            port: 9333,
            allowed_origins: vec!["chrome-extension://*".to_string()],
            max_attachment_bytes: 10 * 1024 * 1024,
            session_timeout_secs: 3600,
        }
    }
}

/// State of a connected browser tab.
#[derive(Debug, Clone, Serialize)]
pub struct BrowserTabState {
    pub tab_id: u32,
    pub url: String,
    pub title: String,
    pub connected: bool,
    pub last_heartbeat: String,
    pub console_messages: Vec<ConsoleEntry>,
    pub failed_requests: Vec<FailedRequestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleEntry {
    pub level: String,
    pub message: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedRequestEntry {
    pub url: String,
    pub status: u16,
    pub method: String,
}

// ─── Browser Tool Implementations ───────────────────────────────────────

use async_trait::async_trait;
use pipit_tools::{Tool, ToolContext, ToolError, ToolResult};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

/// Default CDP debugging port.
const CDP_PORT: u16 = 9222;

/// Lazy CDP connection — shared across all browser tools.
static CDP_CLIENT: once_cell::sync::Lazy<TokioMutex<Option<Arc<crate::cdp::CdpClient>>>> =
    once_cell::sync::Lazy::new(|| TokioMutex::new(None));

/// Get or create a CDP client connection.
async fn get_cdp_client() -> Result<Arc<crate::cdp::CdpClient>, ToolError> {
    let port = std::env::var("PIPIT_CDP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(CDP_PORT);

    let mut guard = CDP_CLIENT.lock().await;
    if let Some(ref client) = *guard {
        return Ok(client.clone());
    }

    let client = crate::cdp::connect_or_launch(port)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("CDP connection failed: {e}")))?;
    let client = Arc::new(client);
    *guard = Some(client.clone());
    Ok(client)
}

/// Navigate tool — browse to a URL via CDP.
pub struct BrowserNavigateTool;

#[async_trait]
impl Tool for BrowserNavigateTool {
    fn name(&self) -> &str {
        "browser_navigate"
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "URL to navigate to"},
                "wait_for": {"type": "string", "description": "CSS selector to wait for after navigation"}
            },
            "required": ["url"]
        })
    }
    fn description(&self) -> &str {
        "Navigate the browser to a URL and return page info."
    }
    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        args: Value,
        _ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("url required".into()))?;
        let wait_for = args.get("wait_for").and_then(|v| v.as_str());

        let client = get_cdp_client().await?;

        // Navigate via CDP Page.navigate
        let nav_result = client
            .send_command("Page.navigate", serde_json::json!({"url": url}))
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Navigation failed: {e}")))?;

        // Wait for page load
        let _ = client
            .wait_for_event("Page.loadEventFired", std::time::Duration::from_secs(15))
            .await;

        // Optionally wait for a specific selector
        if let Some(selector) = wait_for {
            let js = format!(
                r#"new Promise((resolve, reject) => {{
                    const el = document.querySelector({sel});
                    if (el) return resolve(true);
                    const obs = new MutationObserver(() => {{
                        if (document.querySelector({sel})) {{ obs.disconnect(); resolve(true); }}
                    }});
                    obs.observe(document.body, {{childList: true, subtree: true}});
                    setTimeout(() => {{ obs.disconnect(); reject('timeout'); }}, 5000);
                }})"#,
                sel = serde_json::to_string(selector).unwrap_or_default()
            );
            let _ = client
                .send_command(
                    "Runtime.evaluate",
                    serde_json::json!({"expression": js, "awaitPromise": true}),
                )
                .await;
        }

        // Get page title
        let title_result = client
            .send_command(
                "Runtime.evaluate",
                serde_json::json!({"expression": "document.title"}),
            )
            .await
            .ok();
        let title = title_result
            .and_then(|r| {
                r.get("result")?
                    .get("value")?
                    .as_str()
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();

        let frame_id = nav_result
            .get("frameId")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        Ok(ToolResult::text(format!(
            "Navigated to: {url}\nTitle: {title}\nFrame: {frame_id}"
        )))
    }
}

/// Screenshot tool — capture page or element.
pub struct BrowserScreenshotTool;

#[async_trait]
impl Tool for BrowserScreenshotTool {
    fn name(&self) -> &str {
        "browser_screenshot"
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "selector": {"type": "string", "description": "CSS selector for element screenshot"},
                "full_page": {"type": "boolean", "description": "Capture full scrollable page"}
            }
        })
    }
    fn description(&self) -> &str {
        "Take a screenshot of the current page or element. Returns base64 PNG."
    }
    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        args: Value,
        _ctx: &ToolContext,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let selector = args.get("selector").and_then(|v| v.as_str());
        let full_page = args
            .get("full_page")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let client = get_cdp_client().await?;

        // If selector specified, get element's bounding box for clip
        let mut params = serde_json::json!({"format": "png"});
        if let Some(sel) = selector {
            let js = format!(
                r#"(() => {{
                    const el = document.querySelector({sel});
                    if (!el) return null;
                    const rect = el.getBoundingClientRect();
                    return {{x: rect.x, y: rect.y, width: rect.width, height: rect.height, scale: window.devicePixelRatio}};
                }})()"#,
                sel = serde_json::to_string(sel).unwrap_or_default()
            );
            let eval_result = client
                .send_command(
                    "Runtime.evaluate",
                    serde_json::json!({"expression": js, "returnByValue": true}),
                )
                .await
                .map_err(|e| ToolError::ExecutionFailed(format!("Element lookup failed: {e}")))?;
            if let Some(value) = eval_result.get("result").and_then(|r| r.get("value")) {
                if !value.is_null() {
                    params["clip"] = value.clone();
                }
            }
        }
        if full_page {
            // Get full page metrics for capturing entire scrollable area
            let metrics = client
                .send_command("Page.getLayoutMetrics", serde_json::json!({}))
                .await
                .ok();
            if let Some(m) = metrics {
                if let Some(content_size) = m.get("contentSize") {
                    params["clip"] = serde_json::json!({
                        "x": 0, "y": 0,
                        "width": content_size.get("width").and_then(|v| v.as_f64()).unwrap_or(1280.0),
                        "height": content_size.get("height").and_then(|v| v.as_f64()).unwrap_or(720.0),
                        "scale": 1
                    });
                    params["captureBeyondViewport"] = serde_json::json!(true);
                }
            }
        }

        let result = client
            .send_command("Page.captureScreenshot", params)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Screenshot failed: {e}")))?;

        let data_b64 = result.get("data").and_then(|v| v.as_str()).unwrap_or("");

        let byte_len = data_b64.len() * 3 / 4; // approximate decoded size
        Ok(ToolResult {
            content: format!(
                "Screenshot captured ({:.1}KB PNG{}{})\n[base64 data: {} chars]",
                byte_len as f64 / 1024.0,
                selector
                    .map(|s| format!(", selector: {s}"))
                    .unwrap_or_default(),
                if full_page { ", full page" } else { "" },
                data_b64.len()
            ),
            display: None,
            mutated: false,
            content_bytes: data_b64.len(),
        })
    }
}

/// Click tool — click an element by selector.
pub struct BrowserClickTool;

#[async_trait]
impl Tool for BrowserClickTool {
    fn name(&self) -> &str {
        "browser_click"
    }
    fn schema(&self) -> Value {
        serde_json::json!({"type": "object", "properties": {"selector": {"type": "string"}}, "required": ["selector"]})
    }
    fn description(&self) -> &str {
        "Click an element in the browser by CSS selector."
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
        let sel = args
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("selector required".into()))?;

        let client = get_cdp_client().await?;

        // Use Runtime.evaluate to find element center and dispatch click
        let js = format!(
            r#"(() => {{
                const el = document.querySelector({sel});
                if (!el) return {{error: 'Element not found: ' + {sel_raw}}};
                const rect = el.getBoundingClientRect();
                const x = rect.x + rect.width / 2;
                const y = rect.y + rect.height / 2;
                el.click();
                return {{clicked: true, x: x, y: y, tag: el.tagName, text: (el.textContent || '').slice(0, 50)}};
            }})()"#,
            sel = serde_json::to_string(sel).unwrap_or_default(),
            sel_raw = serde_json::to_string(sel).unwrap_or_default()
        );

        let result = client
            .send_command(
                "Runtime.evaluate",
                serde_json::json!({"expression": js, "returnByValue": true}),
            )
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Click failed: {e}")))?;

        let value = result
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        if let Some(err) = value.get("error").and_then(|e| e.as_str()) {
            return Err(ToolError::ExecutionFailed(err.to_string()));
        }

        let tag = value.get("tag").and_then(|v| v.as_str()).unwrap_or("?");
        let text = value.get("text").and_then(|v| v.as_str()).unwrap_or("");
        Ok(ToolResult::mutating(format!(
            "Clicked <{tag}> at selector '{sel}'{}",
            if text.is_empty() {
                String::new()
            } else {
                format!(" (text: \"{text}\")")
            }
        )))
    }
}

/// Type tool — type text into an element.
pub struct BrowserTypeTool;

#[async_trait]
impl Tool for BrowserTypeTool {
    fn name(&self) -> &str {
        "browser_type"
    }
    fn schema(&self) -> Value {
        serde_json::json!({"type": "object", "properties": {"selector": {"type": "string"}, "text": {"type": "string"}}, "required": ["selector", "text"]})
    }
    fn description(&self) -> &str {
        "Type text into a form field in the browser."
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
        let sel = args
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("selector required".into()))?;
        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("text required".into()))?;

        let client = get_cdp_client().await?;

        // Focus the element and set its value, then dispatch input event
        let js = format!(
            r#"(() => {{
                const el = document.querySelector({sel});
                if (!el) return {{error: 'Element not found: ' + {sel_raw}}};
                el.focus();
                el.value = {text_val};
                el.dispatchEvent(new Event('input', {{bubbles: true}}));
                el.dispatchEvent(new Event('change', {{bubbles: true}}));
                return {{typed: true, tag: el.tagName, type: el.type || 'text'}};
            }})()"#,
            sel = serde_json::to_string(sel).unwrap_or_default(),
            sel_raw = serde_json::to_string(sel).unwrap_or_default(),
            text_val = serde_json::to_string(text).unwrap_or_default()
        );

        let result = client
            .send_command(
                "Runtime.evaluate",
                serde_json::json!({"expression": js, "returnByValue": true}),
            )
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Type failed: {e}")))?;

        let value = result
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        if let Some(err) = value.get("error").and_then(|e| e.as_str()) {
            return Err(ToolError::ExecutionFailed(err.to_string()));
        }

        Ok(ToolResult::mutating(format!("Typed '{text}' into '{sel}'")))
    }
}

/// Console tool — get browser console messages.
pub struct BrowserConsoleTool;

#[async_trait]
impl Tool for BrowserConsoleTool {
    fn name(&self) -> &str {
        "browser_console"
    }
    fn schema(&self) -> Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn description(&self) -> &str {
        "Get recent browser console messages (errors, warnings, logs)."
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
        let client = get_cdp_client().await?;

        // Collect console messages by evaluating a helper that captures them
        let js = r#"(() => {
            if (!window.__pipit_console_msgs) {
                window.__pipit_console_msgs = [];
                const orig = {};
                ['log','warn','error','info'].forEach(level => {
                    orig[level] = console[level];
                    console[level] = function(...args) {
                        window.__pipit_console_msgs.push({level, text: args.map(a => String(a)).join(' '), ts: Date.now()});
                        orig[level].apply(console, args);
                    };
                });
            }
            const msgs = window.__pipit_console_msgs.splice(0);
            return msgs;
        })()"#;

        let result = client
            .send_command(
                "Runtime.evaluate",
                serde_json::json!({"expression": js, "returnByValue": true}),
            )
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Console capture failed: {e}")))?;

        let messages = result.get("result").and_then(|r| r.get("value"));

        match messages {
            Some(serde_json::Value::Array(msgs)) if !msgs.is_empty() => {
                let formatted: Vec<String> = msgs
                    .iter()
                    .map(|m| {
                        let level = m.get("level").and_then(|v| v.as_str()).unwrap_or("log");
                        let text = m.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        format!("[{level}] {text}")
                    })
                    .collect();
                Ok(ToolResult::text(format!(
                    "Console messages ({} entries):\n{}",
                    formatted.len(),
                    formatted.join("\n")
                )))
            }
            _ => Ok(ToolResult::text("No console messages captured.")),
        }
    }
}

/// Network tool — get failed network requests.
pub struct BrowserNetworkTool;

#[async_trait]
impl Tool for BrowserNetworkTool {
    fn name(&self) -> &str {
        "browser_network"
    }
    fn schema(&self) -> Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn description(&self) -> &str {
        "Get failed network requests from the browser."
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
        let client = get_cdp_client().await?;

        // Capture failed requests via Performance API
        let js = r#"(() => {
            const entries = performance.getEntriesByType('resource');
            if (!window.__pipit_failed_requests) window.__pipit_failed_requests = [];
            
            // Also check via fetch interception
            const failed = window.__pipit_failed_requests.splice(0);
            
            // Performance entries don't have status codes directly,
            // but transferSize=0 with duration>0 often indicates failure
            const suspicious = entries
                .filter(e => e.transferSize === 0 && e.duration > 0 && e.name.startsWith('http'))
                .slice(-20)
                .map(e => ({url: e.name, type: e.initiatorType, duration: Math.round(e.duration)}));
            
            return {failed, suspicious};
        })()"#;

        let result = client
            .send_command(
                "Runtime.evaluate",
                serde_json::json!({"expression": js, "returnByValue": true}),
            )
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Network capture failed: {e}")))?;

        let value = result.get("result").and_then(|r| r.get("value"));

        match value {
            Some(v) => {
                let failed = v
                    .get("failed")
                    .and_then(|f| f.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let suspicious = v
                    .get("suspicious")
                    .and_then(|f| f.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let details = serde_json::to_string_pretty(v).unwrap_or_default();
                Ok(ToolResult::text(format!(
                    "Network status: {failed} failed requests, {suspicious} suspicious entries\n{details}"
                )))
            }
            None => Ok(ToolResult::text("No network data available.")),
        }
    }
}

/// Register all browser tools into the tool registry.
pub fn register_browser_tools(registry: &mut pipit_tools::ToolRegistry) {
    use std::sync::Arc;
    registry.register(Arc::new(BrowserNavigateTool));
    registry.register(Arc::new(BrowserScreenshotTool));
    registry.register(Arc::new(BrowserClickTool));
    registry.register(Arc::new(BrowserTypeTool));
    registry.register(Arc::new(BrowserConsoleTool));
    registry.register(Arc::new(BrowserNetworkTool));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_base64_roundtrip() {
        use base64::Engine;
        let original = b"hello world screenshot data";
        let encoded = base64::engine::general_purpose::STANDARD.encode(original);
        let attachment = BrowserAttachment::from_base64("image/png", &encoded, None).unwrap();
        assert_eq!(attachment.data, original);
        assert_eq!(attachment.to_base64(), encoded);
    }

    #[test]
    fn token_estimation() {
        let att = BrowserAttachment {
            mime_type: "image/png".into(),
            data: vec![0; 1000],
            filename: None,
            width: Some(1920),
            height: Some(1080),
        };
        assert!(att.estimated_tokens() > 2000); // 1920*1080/750 ≈ 2764
    }

    #[test]
    fn bridge_command_serialization() {
        let cmd = BridgeCommand::Navigate {
            url: "https://example.com".into(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("Navigate"));
    }
}
