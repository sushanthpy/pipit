//! JSON-RPC 2.0 Server — Bidirectional stdio transport (Task 14).
//!
//! Provides a headless, programmatic interface to pipit for IDE integrations,
//! CI pipelines, and external tooling. Reads JSON-RPC requests from stdin,
//! writes responses + notifications to stdout.
//!
//! ## Protocol
//!
//! Requests (stdin → pipit):
//! - `agent/run`   — Start a new agent run with a prompt
//! - `agent/cancel` — Cancel the current run
//! - `session/export` — Export the session ledger
//! - `system/ping`  — Health check
//! - `system/version` — Version info
//!
//! Notifications (pipit → stdout):
//! - `agent/event`  — Streaming agent events (thinking, tool calls, text)
//! - `agent/done`   — Run completed

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Write};

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Deserialize)]
struct RpcRequest {
    jsonrpc: String,
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

/// JSON-RPC 2.0 response envelope.
#[derive(Debug, Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

impl RpcResponse {
    fn success(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Option<serde_json::Value>, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// JSON-RPC 2.0 notification (server → client).
#[derive(Debug, Serialize)]
struct RpcNotification {
    jsonrpc: &'static str,
    method: String,
    params: serde_json::Value,
}

// Standard JSON-RPC error codes
const PARSE_ERROR: i64 = -32700;
const METHOD_NOT_FOUND: i64 = -32601;

/// Run the RPC server, reading from stdin and writing to stdout.
pub async fn run_rpc_server() -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    // Write server-ready notification
    let ready = RpcNotification {
        jsonrpc: "2.0",
        method: "server/ready".into(),
        params: serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "capabilities": ["agent/run", "agent/cancel", "session/export", "system/ping", "system/version"],
        }),
    };
    write_jsonrpc(&mut out, &ready)?;

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break, // EOF
        };

        if line.trim().is_empty() {
            continue;
        }

        let req: RpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = RpcResponse::error(None, PARSE_ERROR, format!("Parse error: {}", e));
                write_jsonrpc(&mut out, &resp)?;
                continue;
            }
        };

        if req.jsonrpc != "2.0" {
            let resp = RpcResponse::error(req.id, PARSE_ERROR, "Expected jsonrpc: \"2.0\"");
            write_jsonrpc(&mut out, &resp)?;
            continue;
        }

        let resp = dispatch(&req).await;
        write_jsonrpc(&mut out, &resp)?;
    }

    Ok(())
}

async fn dispatch(req: &RpcRequest) -> RpcResponse {
    match req.method.as_str() {
        "system/ping" => RpcResponse::success(
            req.id.clone(),
            serde_json::json!({ "pong": true }),
        ),
        "system/version" => RpcResponse::success(
            req.id.clone(),
            serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "name": "pipit",
            }),
        ),
        "session/export" => handle_session_export(req),
        "agent/run" => {
            // Placeholder — full implementation requires wiring into AgentLoop
            RpcResponse::success(
                req.id.clone(),
                serde_json::json!({
                    "status": "not_implemented",
                    "message": "agent/run requires full agent loop wiring — use pipit --json for now",
                }),
            )
        }
        "agent/cancel" => RpcResponse::success(
            req.id.clone(),
            serde_json::json!({ "cancelled": false, "message": "no active run" }),
        ),
        _ => RpcResponse::error(
            req.id.clone(),
            METHOD_NOT_FOUND,
            format!("Unknown method: {}", req.method),
        ),
    }
}

fn handle_session_export(req: &RpcRequest) -> RpcResponse {
    let ledger_path = req.params.get("ledger")
        .and_then(|v| v.as_str());

    let ledger_path = match ledger_path {
        Some(p) => p,
        None => {
            return RpcResponse::error(
                req.id.clone(),
                -32602,
                "Missing required param: ledger",
            );
        }
    };

    let format = req.params.get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("md");

    let path = std::path::Path::new(ledger_path);
    if !path.exists() {
        return RpcResponse::error(
            req.id.clone(),
            -32602,
            format!("Ledger not found: {}", ledger_path),
        );
    }

    match pipit_core::ledger::SessionLedger::replay(path) {
        Ok(events) => {
            let opts = pipit_core::export::ExportOptions::default();
            let content = match format {
                "html" => pipit_core::export::export_html(&events, &opts),
                _ => pipit_core::export::export_markdown(&events, &opts),
            };
            RpcResponse::success(
                req.id.clone(),
                serde_json::json!({
                    "content": content,
                    "format": format,
                    "events": events.len(),
                }),
            )
        }
        Err(e) => RpcResponse::error(
            req.id.clone(),
            -32000,
            format!("Ledger replay failed: {}", e),
        ),
    }
}

fn write_jsonrpc<W: Write>(out: &mut W, value: &impl Serialize) -> Result<()> {
    let json = serde_json::to_string(value)?;
    writeln!(out, "{}", json)?;
    out.flush()?;
    Ok(())
}
