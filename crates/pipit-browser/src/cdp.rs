//! Chrome DevTools Protocol (CDP) client.
//!
//! Manages a headless Chrome instance and communicates via WebSocket.
//! Supports sending CDP commands and receiving events/responses.

use crate::BrowserError;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};

/// Find the Chrome executable on the system.
pub fn find_chrome() -> Result<PathBuf, BrowserError> {
    let candidates = if cfg!(target_os = "macos") {
        vec![
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ]
    } else if cfg!(target_os = "linux") {
        vec![
            "google-chrome",
            "google-chrome-stable",
            "chromium-browser",
            "chromium",
        ]
    } else {
        vec!["chrome.exe"]
    };

    if let Ok(path) = std::env::var("CHROME_PATH") {
        let p = PathBuf::from(&path);
        if p.exists() {
            return Ok(p);
        }
    }

    for candidate in candidates {
        let path = PathBuf::from(candidate);
        if path.exists() {
            return Ok(path);
        }
        if !candidate.starts_with('/') {
            if let Ok(output) = std::process::Command::new("which").arg(candidate).output() {
                if output.status.success() {
                    let p = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !p.is_empty() {
                        return Ok(PathBuf::from(p));
                    }
                }
            }
        }
    }

    Err(BrowserError::ChromeNotFound)
}

/// Launch a headless Chrome instance.
pub async fn launch_headless(port: u16) -> Result<tokio::process::Child, BrowserError> {
    let chrome = find_chrome()?;

    let child = tokio::process::Command::new(chrome)
        .args([
            "--headless=new",
            &format!("--remote-debugging-port={}", port),
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-gpu",
            "--disable-extensions",
            "--disable-popup-blocking",
            "--window-size=1280,720",
            "about:blank",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| BrowserError::ConnectionFailed(format!("Failed to launch Chrome: {}", e)))?;

    // Wait for Chrome to start
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    Ok(child)
}

/// Get the WebSocket debugger URL from Chrome's HTTP endpoint.
pub async fn get_ws_url(port: u16) -> Result<String, BrowserError> {
    let url = format!("http://127.0.0.1:{}/json/version", port);
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| BrowserError::ConnectionFailed(format!("Chrome not responding: {}", e)))?;
    let data: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| BrowserError::ConnectionFailed(format!("Invalid response: {}", e)))?;
    let ws_url = data
        .get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .ok_or_else(|| BrowserError::ConnectionFailed("No WebSocket URL".to_string()))?;
    Ok(ws_url.to_string())
}

/// Get the WebSocket URL for the first page/tab.
pub async fn get_page_ws_url(port: u16) -> Result<String, BrowserError> {
    let url = format!("http://127.0.0.1:{}/json/list", port);
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| BrowserError::ConnectionFailed(format!("Chrome not responding: {}", e)))?;
    let data: Vec<serde_json::Value> = resp
        .json()
        .await
        .map_err(|e| BrowserError::ConnectionFailed(format!("Invalid response: {}", e)))?;
    let page = data
        .iter()
        .find(|entry| entry.get("type").and_then(|t| t.as_str()) == Some("page"))
        .ok_or_else(|| BrowserError::ConnectionFailed("No page target found".to_string()))?;
    let ws_url = page
        .get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .ok_or_else(|| BrowserError::ConnectionFailed("No WebSocket URL for page".to_string()))?;
    Ok(ws_url.to_string())
}

/// A CDP command message sent to Chrome.
#[derive(Debug, Serialize)]
struct CdpCommand {
    id: u64,
    method: String,
    params: serde_json::Value,
}

/// A CDP response message from Chrome.
#[derive(Debug, Deserialize)]
struct CdpResponse {
    id: Option<u64>,
    result: Option<serde_json::Value>,
    error: Option<CdpError>,
    method: Option<String>,
    params: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct CdpError {
    code: i64,
    message: String,
}

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<serde_json::Value, BrowserError>>>>>;
type EventTx = mpsc::UnboundedSender<CdpEvent>;

/// A CDP event received from Chrome.
#[derive(Debug, Clone)]
pub struct CdpEvent {
    pub method: String,
    pub params: serde_json::Value,
}

/// CDP WebSocket client — sends commands and receives responses/events.
pub struct CdpClient {
    write_tx: mpsc::UnboundedSender<String>,
    next_id: AtomicU64,
    pending: PendingMap,
    event_rx: Arc<Mutex<mpsc::UnboundedReceiver<CdpEvent>>>,
    _reader_handle: tokio::task::JoinHandle<()>,
}

impl CdpClient {
    /// Connect to a Chrome instance via WebSocket.
    pub async fn connect(ws_url: &str) -> Result<Self, BrowserError> {
        let (ws_stream, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| BrowserError::ConnectionFailed(format!("WebSocket connect failed: {}", e)))?;

        let (mut ws_write, mut ws_read) = ws_stream.split();
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, event_rx): (EventTx, _) = mpsc::unbounded_channel();
        let (write_tx, mut write_rx) = mpsc::unbounded_channel::<String>();

        // Writer task: forwards queued messages to WebSocket
        tokio::spawn(async move {
            while let Some(msg) = write_rx.recv().await {
                if ws_write
                    .send(tokio_tungstenite::tungstenite::Message::Text(msg))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        // Reader task: routes responses to pending futures, events to event channel
        let pending_clone = pending.clone();
        let reader_handle = tokio::spawn(async move {
            while let Some(Ok(msg)) = ws_read.next().await {
                let text = match msg {
                    tokio_tungstenite::tungstenite::Message::Text(t) => t.to_string(),
                    _ => continue,
                };
                let parsed: CdpResponse = match serde_json::from_str(&text) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                if let Some(id) = parsed.id {
                    // This is a response to a command
                    let mut map = pending_clone.lock().await;
                    if let Some(tx) = map.remove(&id) {
                        let result = if let Some(err) = parsed.error {
                            Err(BrowserError::CdpError(format!(
                                "[{}] {}",
                                err.code, err.message
                            )))
                        } else {
                            Ok(parsed.result.unwrap_or(serde_json::Value::Null))
                        };
                        let _ = tx.send(result);
                    }
                } else if let Some(method) = parsed.method {
                    // This is an event
                    let _ = event_tx.send(CdpEvent {
                        method,
                        params: parsed.params.unwrap_or(serde_json::Value::Null),
                    });
                }
            }
        });

        Ok(Self {
            write_tx,
            next_id: AtomicU64::new(1),
            pending,
            event_rx: Arc::new(Mutex::new(event_rx)),
            _reader_handle: reader_handle,
        })
    }

    /// Send a CDP command and wait for the response.
    pub async fn send_command(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, BrowserError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let cmd = CdpCommand {
            id,
            method: method.to_string(),
            params,
        };
        let json = serde_json::to_string(&cmd)
            .map_err(|e| BrowserError::CdpError(format!("Serialize failed: {}", e)))?;

        let (tx, rx) = oneshot::channel();
        {
            let mut map = self.pending.lock().await;
            map.insert(id, tx);
        }

        self.write_tx
            .send(json)
            .map_err(|_| BrowserError::ConnectionFailed("WebSocket write channel closed".into()))?;

        let result = tokio::time::timeout(std::time::Duration::from_secs(30), rx)
            .await
            .map_err(|_| BrowserError::Timeout("CDP command timed out after 30s".into()))?
            .map_err(|_| BrowserError::ConnectionFailed("CDP response channel dropped".into()))?;

        result
    }

    /// Wait for a specific CDP event (by method name) with timeout.
    pub async fn wait_for_event(
        &self,
        method: &str,
        timeout: std::time::Duration,
    ) -> Result<CdpEvent, BrowserError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut rx = self.event_rx.lock().await;
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(event)) if event.method == method => return Ok(event),
                Ok(Some(_)) => continue, // Not the event we want
                Ok(None) => {
                    return Err(BrowserError::ConnectionFailed(
                        "Event channel closed".into(),
                    ))
                }
                Err(_) => {
                    return Err(BrowserError::Timeout(format!(
                        "Timed out waiting for event: {}",
                        method
                    )))
                }
            }
        }
    }

    /// Enable a CDP domain (e.g., "Page", "Runtime", "Network", "Console").
    pub async fn enable_domain(&self, domain: &str) -> Result<(), BrowserError> {
        self.send_command(&format!("{}.enable", domain), serde_json::json!({}))
            .await?;
        Ok(())
    }
}

/// Convenience: launch Chrome, connect CDP, enable standard domains.
pub async fn connect_or_launch(port: u16) -> Result<CdpClient, BrowserError> {
    // Try connecting to existing Chrome first
    match get_page_ws_url(port).await {
        Ok(ws_url) => {
            let client = CdpClient::connect(&ws_url).await?;
            client.enable_domain("Page").await?;
            client.enable_domain("Runtime").await?;
            client.enable_domain("Network").await?;
            client.enable_domain("Console").await?;
            return Ok(client);
        }
        Err(_) => {
            tracing::info!("No Chrome at port {port}, launching headless...");
        }
    }

    let _child = launch_headless(port).await?;
    // Retry connecting after launch
    for _ in 0..5 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if let Ok(ws_url) = get_page_ws_url(port).await {
            let client = CdpClient::connect(&ws_url).await?;
            client.enable_domain("Page").await?;
            client.enable_domain("Runtime").await?;
            client.enable_domain("Network").await?;
            client.enable_domain("Console").await?;
            return Ok(client);
        }
    }

    Err(BrowserError::ConnectionFailed(
        "Failed to connect after launching Chrome".into(),
    ))
}
