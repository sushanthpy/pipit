//! Isolated Browser Twin
//!
//! Capability-bounded browser runtime with session isolation,
//! origin scoping, and DOM/action provenance. Each task gets
//! an ephemeral profile with per-task cookies, origin allowlists,
//! and an append-only action log.
//!
//! Authority: Cap = {origins, methods, storage_scope, download_policy}.
//! Each DOM/API call checks op ∈ allowed_methods(origin) in O(1).

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ─── Browser Capability Model ───────────────────────────────────────────

/// What a browser twin is allowed to do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserCapability {
    /// Allowed origin patterns (e.g., "github.com", "*.example.com").
    pub allowed_origins: Vec<String>,
    /// Allowed DOM/API methods per origin.
    pub allowed_methods: HashMap<String, HashSet<BrowserMethod>>,
    /// Storage scope for cookies and local storage.
    pub storage_scope: StorageScope,
    /// Download policy.
    pub download_policy: DownloadPolicy,
    /// Maximum navigation count per session.
    pub max_navigations: u32,
    /// Maximum page load timeout (seconds).
    pub page_timeout_secs: u32,
    /// Whether screenshots are allowed.
    pub allow_screenshots: bool,
    /// Whether JavaScript execution is allowed.
    pub allow_javascript: bool,
}

impl Default for BrowserCapability {
    fn default() -> Self {
        Self {
            allowed_origins: Vec::new(),
            allowed_methods: HashMap::new(),
            storage_scope: StorageScope::Ephemeral,
            download_policy: DownloadPolicy::Deny,
            max_navigations: 20,
            page_timeout_secs: 30,
            allow_screenshots: true,
            allow_javascript: true,
        }
    }
}

/// DOM/API methods that can be individually gated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BrowserMethod {
    /// Navigate to a URL.
    Navigate,
    /// Read DOM content (querySelector, textContent).
    DomRead,
    /// Modify DOM (click, type, setAttribute).
    DomWrite,
    /// Take screenshots.
    Screenshot,
    /// Execute arbitrary JavaScript.
    ExecuteJs,
    /// Read cookies.
    ReadCookies,
    /// Write cookies.
    WriteCookies,
    /// Download files.
    Download,
    /// Read network responses.
    ReadNetwork,
    /// Read console output.
    ReadConsole,
}

/// How session storage is scoped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageScope {
    /// Ephemeral: all storage destroyed when session ends.
    Ephemeral,
    /// Per-task: storage persists within a task but not across tasks.
    PerTask,
    /// Shared: storage shared across tasks (requires explicit delegation).
    Shared,
}

/// Whether file downloads are allowed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DownloadPolicy {
    /// No downloads allowed.
    Deny,
    /// Downloads allowed to a sandboxed directory.
    Sandboxed { path: PathBuf, max_bytes: u64 },
    /// Downloads allowed to project directory.
    ProjectDir,
}

// ─── Browser Session ────────────────────────────────────────────────────

/// An isolated browser session with append-only action log.
#[derive(Debug)]
pub struct BrowserTwin {
    /// Unique session identifier.
    pub session_id: String,
    /// Task this session is scoped to.
    pub task_id: String,
    /// Capability constraints for this session.
    pub capabilities: BrowserCapability,
    /// Ephemeral profile directory (deleted on drop).
    pub profile_dir: Option<PathBuf>,
    /// Append-only action log for audit/replay.
    pub action_log: Vec<BrowserAction>,
    /// Navigation count for rate limiting.
    pub navigation_count: u32,
    /// Whether the session is currently active.
    pub active: bool,
}

/// A recorded browser action for audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserAction {
    pub timestamp_ms: u64,
    pub action_type: BrowserActionType,
    pub origin: String,
    pub success: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BrowserActionType {
    Navigate { url: String },
    DomRead { selector: String },
    DomClick { selector: String },
    DomType { selector: String, text_len: usize },
    Screenshot { width: u32, height: u32 },
    ExecuteJs { script_len: usize },
    Download { filename: String, bytes: u64 },
    ConsoleRead { count: usize },
}

impl BrowserTwin {
    /// Create a new isolated browser twin for a task.
    pub fn new(task_id: &str, capabilities: BrowserCapability) -> Self {
        let session_id = format!(
            "twin-{}-{}",
            task_id,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        Self {
            session_id,
            task_id: task_id.to_string(),
            capabilities,
            profile_dir: None,
            action_log: Vec::new(),
            navigation_count: 0,
            active: false,
        }
    }

    /// Check if an operation is allowed for the given origin.
    /// Cost: O(1) via HashSet lookup.
    pub fn check_permission(&self, origin: &str, method: BrowserMethod) -> Result<(), String> {
        // Check origin allowlist
        if !self.capabilities.allowed_origins.is_empty()
            && !self.capabilities.allowed_origins.iter().any(|allowed| {
                origin == *allowed || (allowed.starts_with("*.") && origin.ends_with(&allowed[1..]))
            })
        {
            return Err(format!(
                "origin '{}' not in allowed list: {:?}",
                origin, self.capabilities.allowed_origins
            ));
        }

        // Check method allowlist for this origin (if configured)
        if let Some(methods) = self.capabilities.allowed_methods.get(origin) {
            if !methods.contains(&method) {
                return Err(format!(
                    "method {:?} not allowed for origin '{}'",
                    method, origin
                ));
            }
        }

        // Check global method restrictions
        match method {
            BrowserMethod::Screenshot if !self.capabilities.allow_screenshots => {
                return Err("screenshots not allowed".to_string());
            }
            BrowserMethod::ExecuteJs if !self.capabilities.allow_javascript => {
                return Err("JavaScript execution not allowed".to_string());
            }
            BrowserMethod::Download => match &self.capabilities.download_policy {
                DownloadPolicy::Deny => return Err("downloads not allowed".to_string()),
                _ => {}
            },
            BrowserMethod::Navigate => {
                if self.navigation_count >= self.capabilities.max_navigations {
                    return Err(format!(
                        "navigation limit reached ({})",
                        self.capabilities.max_navigations
                    ));
                }
            }
            _ => {}
        }

        Ok(())
    }

    /// Record a browser action to the append-only log.
    /// Audit/replay cost: O(m) where m is log length.
    pub fn record_action(
        &mut self,
        action_type: BrowserActionType,
        origin: &str,
        success: bool,
        detail: &str,
    ) {
        self.action_log.push(BrowserAction {
            timestamp_ms: now_ms(),
            action_type,
            origin: origin.to_string(),
            success,
            detail: detail.to_string(),
        });
    }

    /// Get the action log for audit purposes.
    pub fn audit_log(&self) -> &[BrowserAction] {
        &self.action_log
    }

    /// Number of actions recorded.
    pub fn action_count(&self) -> usize {
        self.action_log.len()
    }
}

impl Drop for BrowserTwin {
    fn drop(&mut self) {
        // Clean up ephemeral profile directory
        if let Some(ref dir) = self.profile_dir {
            if self.capabilities.storage_scope == StorageScope::Ephemeral {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }
}

/// Create a restrictive capability set for automated testing.
pub fn testing_capabilities(allowed_origins: Vec<String>) -> BrowserCapability {
    let mut methods = HashMap::new();
    for origin in &allowed_origins {
        methods.insert(
            origin.clone(),
            [
                BrowserMethod::Navigate,
                BrowserMethod::DomRead,
                BrowserMethod::DomWrite,
                BrowserMethod::Screenshot,
                BrowserMethod::ReadConsole,
            ]
            .into_iter()
            .collect(),
        );
    }

    BrowserCapability {
        allowed_origins,
        allowed_methods: methods,
        storage_scope: StorageScope::Ephemeral,
        download_policy: DownloadPolicy::Deny,
        max_navigations: 50,
        page_timeout_secs: 30,
        allow_screenshots: true,
        allow_javascript: true,
    }
}
