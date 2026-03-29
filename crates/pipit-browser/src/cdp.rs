//! Chrome DevTools Protocol (CDP) client.
//!
//! Manages a headless Chrome instance and communicates via WebSocket.

use crate::BrowserError;
use std::path::PathBuf;
use std::process::Stdio;

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

    // Check CHROME_PATH env var first
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
        // Try `which` for non-absolute paths
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
    let resp = reqwest::get(&url).await
        .map_err(|e| BrowserError::ConnectionFailed(format!("Chrome not responding: {}", e)))?;
    let data: serde_json::Value = resp.json().await
        .map_err(|e| BrowserError::ConnectionFailed(format!("Invalid response: {}", e)))?;
    let ws_url = data.get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .ok_or_else(|| BrowserError::ConnectionFailed("No WebSocket URL".to_string()))?;
    Ok(ws_url.to_string())
}
