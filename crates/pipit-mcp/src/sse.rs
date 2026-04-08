//! SSE (Server-Sent Events) transport for MCP clients.
//!
//! Connects to an MCP server over HTTP + SSE instead of stdio.
//! The server exposes a POST endpoint for requests and an SSE stream for responses.

use reqwest::Client;
use serde_json::Value;
use std::collections::HashMap;

/// SSE transport for MCP servers.
pub struct SseTransport {
    client: Client,
    base_url: String,
    headers: HashMap<String, String>,
}

impl SseTransport {
    pub fn new(url: &str, headers: HashMap<String, String>) -> Self {
        Self {
            client: Client::new(),
            base_url: url.to_string(),
            headers,
        }
    }

    /// Send a JSON-RPC request via HTTP POST.
    pub async fn send_request(
        &self,
        method: &str,
        id: u64,
        params: Option<Value>,
    ) -> Result<Value, String> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params.unwrap_or(Value::Object(serde_json::Map::new()))
        });

        let mut req = self
            .client
            .post(&self.base_url)
            .header("Content-Type", "application/json");

        for (key, value) in &self.headers {
            req = req.header(key, value);
        }

        let resp = req
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("SSE request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("SSE HTTP {}: {}", status, body));
        }

        let response: Value = resp
            .json()
            .await
            .map_err(|e| format!("SSE parse error: {}", e))?;

        if let Some(err) = response.get("error") {
            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown");
            return Err(format!("MCP error {}: {}", code, msg));
        }

        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Send a notification (no response expected).
    pub async fn send_notification(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), String> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params.unwrap_or(Value::Object(serde_json::Map::new()))
        });

        let mut req = self
            .client
            .post(&self.base_url)
            .header("Content-Type", "application/json");

        for (key, value) in &self.headers {
            req = req.header(key, value);
        }

        let _ = req
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("SSE notification failed: {}", e))?;

        Ok(())
    }
}
