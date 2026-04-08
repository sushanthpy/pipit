//! pipit-browser: Headless browser integration for autonomous web app testing.
//!
//! Connects to a headless Chromium instance via Chrome DevTools Protocol (CDP).
//! Provides tools for navigation, screenshots, console capture, Lighthouse audits,
//! and visual regression testing.
//!
//! ## CDP Communication
//! Uses WebSocket to connect to Chrome's debugging port.
//! Chrome must be started with `--remote-debugging-port=9222`.

pub mod cdp;
pub mod extension_bridge;
pub mod tools;
pub mod twin;
pub mod visual_diff;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BrowserError {
    #[error("Chrome not found. Install Chrome or set CHROME_PATH")]
    ChromeNotFound,
    #[error("Failed to connect to Chrome: {0}")]
    ConnectionFailed(String),
    #[error("CDP error: {0}")]
    CdpError(String),
    #[error("Navigation failed: {0}")]
    NavigationFailed(String),
    #[error("Screenshot failed: {0}")]
    ScreenshotFailed(String),
    #[error("Timeout: {0}")]
    Timeout(String),
}

/// A captured console message from the browser.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleMessage {
    pub level: String, // log, warn, error, info
    pub text: String,
    pub url: Option<String>,
    pub line: Option<u32>,
}

/// A failed network request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedRequest {
    pub url: String,
    pub status: u16,
    pub method: String,
}

/// Lighthouse audit scores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LighthouseScores {
    pub performance: f64,
    pub accessibility: f64,
    pub best_practices: f64,
    pub seo: f64,
}

/// Result of a visual diff between two screenshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualDiff {
    pub ssim_score: f64,
    pub changed_pixels: usize,
    pub total_pixels: usize,
    pub significant: bool,
}
