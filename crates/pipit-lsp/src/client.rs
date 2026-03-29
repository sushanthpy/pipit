//! LSP client: JSON-RPC 2.0 over stdio to language servers.

use crate::{DefinitionResult, LspDiagnostic, ReferencesResult, SourceLocation, TypeInfo};
use serde_json::Value;
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

/// A connection to a single LSP server.
pub struct LspClient {
    pub kind: crate::LspKind,
    child: Mutex<Child>,
    stdin: Mutex<tokio::process::ChildStdin>,
    stdout: Mutex<BufReader<tokio::process::ChildStdout>>,
    next_id: Mutex<u64>,
    pub initialized: bool,
}

impl LspClient {
    /// Start an LSP server and initialize it.
    pub async fn start(kind: crate::LspKind, project_root: &Path) -> Result<Self, String> {
        let binary = kind.binary();

        // Check if the binary exists
        let which = std::process::Command::new("which")
            .arg(binary)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if !which {
            return Err(format!("LSP server '{}' not found in PATH", binary));
        }

        let mut cmd = Command::new(binary);
        cmd.arg("--stdio")
            .current_dir(project_root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        let mut child = cmd.spawn()
            .map_err(|e| format!("Failed to start {}: {}", binary, e))?;

        let stdin = child.stdin.take().ok_or("No stdin")?;
        let stdout = child.stdout.take().ok_or("No stdout")?;

        let mut client = Self {
            kind,
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            next_id: Mutex::new(1),
            initialized: false,
        };

        // Send initialize request
        let root_uri = format!("file://{}", project_root.display());
        let _init_result = client.request("initialize", serde_json::json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "definition": { "dynamicRegistration": false },
                    "references": { "dynamicRegistration": false },
                    "hover": { "dynamicRegistration": false },
                    "publishDiagnostics": { "relatedInformation": true }
                }
            }
        })).await?;

        // Send initialized notification
        client.notify("initialized", serde_json::json!({})).await?;
        client.initialized = true;

        tracing::info!(server = binary, "LSP server initialized");
        Ok(client)
    }

    /// Send a JSON-RPC request with Content-Length header (LSP base protocol).
    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = {
            let mut next = self.next_id.lock().await;
            let id = *next;
            *next += 1;
            id
        };

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let body_str = serde_json::to_string(&body).map_err(|e| e.to_string())?;
        let header = format!("Content-Length: {}\r\n\r\n", body_str.len());

        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(header.as_bytes()).await.map_err(|e| e.to_string())?;
            stdin.write_all(body_str.as_bytes()).await.map_err(|e| e.to_string())?;
            stdin.flush().await.map_err(|e| e.to_string())?;
        }

        // Read response with Content-Length header
        let mut header_line = String::new();
        {
            let mut stdout = self.stdout.lock().await;
            // Read Content-Length header
            stdout.read_line(&mut header_line).await.map_err(|e| e.to_string())?;
            // Read empty line
            let mut empty = String::new();
            stdout.read_line(&mut empty).await.map_err(|e| e.to_string())?;

            // Parse content length
            let content_length: usize = header_line
                .trim()
                .strip_prefix("Content-Length: ")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);

            if content_length == 0 {
                return Err("No Content-Length in LSP response".to_string());
            }

            let mut buf = vec![0u8; content_length];
            use tokio::io::AsyncReadExt;
            stdout.read_exact(&mut buf).await.map_err(|e| e.to_string())?;

            let response: Value = serde_json::from_slice(&buf)
                .map_err(|e| format!("LSP parse error: {}", e))?;

            if let Some(err) = response.get("error") {
                return Err(format!("LSP error: {}", err));
            }

            Ok(response.get("result").cloned().unwrap_or(Value::Null))
        }
    }

    /// Send a notification (no response).
    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let body_str = serde_json::to_string(&body).map_err(|e| e.to_string())?;
        let header = format!("Content-Length: {}\r\n\r\n", body_str.len());

        let mut stdin = self.stdin.lock().await;
        stdin.write_all(header.as_bytes()).await.map_err(|e| e.to_string())?;
        stdin.write_all(body_str.as_bytes()).await.map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Go to definition of symbol at position.
    pub async fn goto_definition(&self, file: &Path, line: u32, col: u32) -> Result<DefinitionResult, String> {
        let uri = format!("file://{}", file.display());
        let result = self.request("textDocument/definition", serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": col }
        })).await?;

        let locations = parse_locations(&result);
        Ok(DefinitionResult { locations })
    }

    /// Find all references to symbol at position.
    pub async fn find_references(&self, file: &Path, line: u32, col: u32) -> Result<ReferencesResult, String> {
        let uri = format!("file://{}", file.display());
        let result = self.request("textDocument/references", serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": col },
            "context": { "includeDeclaration": true }
        })).await?;

        let locations = parse_locations(&result);
        Ok(ReferencesResult { locations })
    }

    /// Get hover information (type, docs) for symbol at position.
    pub async fn hover(&self, file: &Path, line: u32, col: u32) -> Result<Option<TypeInfo>, String> {
        let uri = format!("file://{}", file.display());
        let result = self.request("textDocument/hover", serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": col }
        })).await?;

        if result.is_null() {
            return Ok(None);
        }

        let contents = result.get("contents");
        let type_string = contents
            .and_then(|c| c.get("value"))
            .and_then(|v| v.as_str())
            .or_else(|| contents.and_then(|c| c.as_str()))
            .unwrap_or("")
            .to_string();

        Ok(Some(TypeInfo {
            symbol: String::new(),
            type_string,
            documentation: None,
        }))
    }

    /// Shut down the LSP server gracefully.
    pub async fn shutdown(&self) {
        let _ = self.request("shutdown", serde_json::json!(null)).await;
        let _ = self.notify("exit", serde_json::json!(null)).await;
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
    }
}

fn parse_locations(value: &Value) -> Vec<SourceLocation> {
    let arr = if value.is_array() {
        value.as_array().cloned().unwrap_or_default()
    } else if value.is_object() {
        vec![value.clone()]
    } else {
        return Vec::new();
    };

    arr.iter().filter_map(|loc| {
        let uri = loc.get("uri")?.as_str()?;
        let file = uri.strip_prefix("file://")
            .map(std::path::PathBuf::from)?;
        let range = loc.get("range")?;
        let start = range.get("start")?;
        let line = start.get("line")?.as_u64()? as u32;
        let col = start.get("character")?.as_u64()? as u32;
        Some(SourceLocation { file, line, column: col })
    }).collect()
}
