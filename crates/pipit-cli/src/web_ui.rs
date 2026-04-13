//! Web UI Stub — lightweight HTTP server for browser-based pipit interface.
//!
//! Serves a single-page app that connects over WebSocket for real-time
//! agent event streaming. This is a foundation for Task 12 (Web UI).
//!
//! Current state: serves a health endpoint and a placeholder SPA.
//! Future: full bidirectional WebSocket with agent loop integration.

use anyhow::Result;
use std::net::SocketAddr;

/// Configuration for the web UI server.
#[derive(Debug, Clone)]
pub struct WebUiConfig {
    /// Address to bind to (default: 127.0.0.1:9090).
    pub bind_addr: SocketAddr,
    /// Enable CORS for development.
    pub cors: bool,
}

impl Default for WebUiConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:9090".parse().unwrap(),
            cors: false,
        }
    }
}

/// Start the web UI HTTP server.
///
/// This is a stub — it serves basic health and version endpoints.
/// Full SPA serving and WebSocket integration will be added incrementally.
pub async fn start_web_ui(config: WebUiConfig) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(config.bind_addr).await?;
    tracing::info!("Web UI listening on http://{}", config.bind_addr);

    loop {
        let (mut stream, _addr) = listener.accept().await?;
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                return;
            }

            let request = String::from_utf8_lossy(&buf[..n]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");

            let (status, content_type, body) = match path {
                "/api/health" => (
                    "200 OK",
                    "application/json",
                    format!(
                        r#"{{"status":"ok","version":"{}"}}"#,
                        env!("CARGO_PKG_VERSION")
                    ),
                ),
                "/api/version" => (
                    "200 OK",
                    "application/json",
                    format!(
                        r#"{{"name":"pipit","version":"{}"}}"#,
                        env!("CARGO_PKG_VERSION")
                    ),
                ),
                _ => (
                    "200 OK",
                    "text/html",
                    PLACEHOLDER_HTML.to_string(),
                ),
            };

            let response = format!(
                "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                content_type,
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
    }
}

const PLACEHOLDER_HTML: &str = r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>pipit — Web UI</title>
<style>
  body { font-family: system-ui; display: flex; justify-content: center; align-items: center; height: 100vh; margin: 0; background: #0d1117; color: #c9d1d9; }
  .container { text-align: center; }
  h1 { font-size: 2.5em; margin-bottom: 0.2em; }
  p { color: #8b949e; }
  code { background: #161b22; padding: 0.2em 0.5em; border-radius: 4px; }
</style>
</head>
<body>
<div class="container">
  <h1>🐦 pipit</h1>
  <p>Web UI is under construction.</p>
  <p>Use <code>pipit --classic</code> for the terminal interface.</p>
  <p><a href="/api/health" style="color: #58a6ff;">/api/health</a></p>
</div>
</body>
</html>"#;
