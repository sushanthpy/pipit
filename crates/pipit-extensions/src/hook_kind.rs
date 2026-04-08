//! Algebraic Hook Kind — closed sum type for hook execution mediums.
//!
//! HookKind forms a coproduct Command + Prompt + Http + Agent + Wasm
//! in the category of Rust types. The compiler's exhaustiveness check
//! discharges the universal-property obligation: adding a new medium
//! is a type error until every runtime handles it.
//!
//! Dispatch is O(1) jump-table. No runtime string matching.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Closed algebraic type for hook execution mediums.
///
/// Each variant carries its own typed payload — no shared fields
/// that some variants ignore. Adding a new variant forces updates
/// at every match site, which is the correct failure mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookKind {
    /// Shell command execution (bash/sh/zsh).
    /// Inherits CWD, receives hook input on stdin, output on stdout.
    Command {
        command: String,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u64,
        /// Shell to use (default: sh).
        #[serde(default = "default_shell")]
        shell: String,
        /// Environment variables to inject.
        #[serde(default)]
        env: HashMap<String, String>,
    },

    /// LLM prompt hook — sends the event context to a model.
    /// The model's response text is the hook decision.
    Prompt {
        /// System prompt for the hook model.
        system: String,
        /// Model override (default: cheapest available).
        model: Option<String>,
        /// Provider override (default: auto-select cheapest).
        provider: Option<String>,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u64,
        /// Maximum tokens for the response.
        #[serde(default = "default_max_tokens")]
        max_tokens: u32,
    },

    /// HTTP webhook — POST event JSON to a URL, response body is the decision.
    Http {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u64,
        /// HTTP method (default: POST).
        #[serde(default = "default_http_method")]
        method: String,
    },

    /// Agent hook — spawns an ephemeral sub-agent loop.
    /// The agent observes session state (read-only) and returns a decision.
    Agent {
        /// Task prompt for the ephemeral agent.
        task: String,
        /// Tool allowlist (empty = inherit parent tools minus mutating).
        #[serde(default)]
        allowed_tools: Vec<String>,
        /// Maximum turns for the ephemeral agent.
        #[serde(default = "default_agent_max_turns")]
        max_turns: u32,
        /// Cost budget fraction of parent turn (0.0–1.0).
        #[serde(default = "default_cost_fraction")]
        cost_fraction: f64,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u64,
    },

    /// WASM-sandboxed hook — content-addressed, fuel-bounded, deterministic.
    /// The guest receives hook input JSON on stdin and returns decision JSON on stdout.
    /// Execution has a hard CPU bound (fuel), a hard memory bound, and no ambient authority.
    Wasm {
        /// Path to the .wasm module (relative to project root).
        module_path: String,
        /// Content-hash of the module (SHA-256). Verified at load time.
        /// If absent, computed on first load and stored.
        module_hash: Option<String>,
        /// Fuel limit (instruction budget). Default: 10M instructions.
        #[serde(default = "default_fuel_limit")]
        fuel_limit: u64,
        /// Maximum linear memory in bytes. Default: 16MB.
        #[serde(default = "default_memory_limit")]
        memory_limit_bytes: u64,
    },
}

fn default_timeout_ms() -> u64 {
    30_000
}
fn default_shell() -> String {
    "sh".into()
}
fn default_http_method() -> String {
    "POST".into()
}
fn default_max_tokens() -> u32 {
    256
}
fn default_agent_max_turns() -> u32 {
    1
}
fn default_cost_fraction() -> f64 {
    0.1
}
fn default_fuel_limit() -> u64 {
    10_000_000
}
fn default_memory_limit() -> u64 {
    16 * 1024 * 1024
}

impl HookKind {
    /// The execution timeout for this hook kind.
    pub fn timeout(&self) -> Duration {
        let ms = match self {
            Self::Command { timeout_ms, .. } => *timeout_ms,
            Self::Prompt { timeout_ms, .. } => *timeout_ms,
            Self::Http { timeout_ms, .. } => *timeout_ms,
            Self::Agent { timeout_ms, .. } => *timeout_ms,
            Self::Wasm { .. } => 30_000, // fuel-bounded, not time-bounded
        };
        Duration::from_millis(ms)
    }

    /// Whether this hook kind is deterministic (same input → same output).
    pub fn is_deterministic(&self) -> bool {
        match self {
            Self::Wasm { .. } => true,     // WASM execution is deterministic
            Self::Command { .. } => false, // shell can read clock, network, etc.
            Self::Prompt { .. } => false,  // LLM output is stochastic
            Self::Http { .. } => false,    // remote server state varies
            Self::Agent { .. } => false,   // agent loop is stochastic
        }
    }

    /// Whether this hook kind runs in a sandbox (no ambient authority).
    pub fn is_sandboxed(&self) -> bool {
        matches!(self, Self::Wasm { .. })
    }

    /// Human-readable kind label for logging/telemetry.
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Command { .. } => "command",
            Self::Prompt { .. } => "prompt",
            Self::Http { .. } => "http",
            Self::Agent { .. } => "agent",
            Self::Wasm { .. } => "wasm",
        }
    }
}

/// Typed hook manifest — replaces the stringly-typed HookManifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedHookManifest {
    /// Which events this hook subscribes to.
    pub events: Vec<String>,
    /// Tool name matcher (glob pattern: "*", "bash", "Edit|Write").
    #[serde(default = "default_matcher")]
    pub matcher: String,
    /// Human-readable description.
    pub description: Option<String>,
    /// The execution medium — algebraically closed.
    pub kind: HookKind,
    /// Whether this hook runs asynchronously (fire-and-forget).
    #[serde(default)]
    pub async_hook: bool,
}

fn default_matcher() -> String {
    "*".into()
}

impl TypedHookManifest {
    /// Check if this hook's matcher matches a tool name.
    pub fn matches_tool(&self, tool_name: &str) -> bool {
        if self.matcher == "*" {
            return true;
        }
        self.matcher.split('|').any(|m| m.trim() == tool_name)
    }
}

// ═══════════════════════════════════════════════════════════════
//  HOOK RUNTIME TRAIT (one impl per variant)
// ═══════════════════════════════════════════════════════════════

/// Context provided to hook execution.
#[derive(Debug, Clone, Serialize)]
pub struct HookContext {
    /// The event that triggered this hook.
    pub event: String,
    /// Tool name (if applicable).
    pub tool_name: Option<String>,
    /// Tool arguments (if applicable).
    pub tool_args: Option<serde_json::Value>,
    /// Tool result (if applicable, for post-tool hooks).
    pub tool_result: Option<String>,
    /// Project root path.
    pub project_root: PathBuf,
    /// Session ID for replay correlation.
    pub session_id: String,
    /// Whether we're in replay mode.
    pub replay_mode: ReplayMode,
}

/// Replay mode for hook execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ReplayMode {
    /// Normal execution — hook runs live.
    Live,
    /// Replay — hook returns cached decision from the kernel.
    Replay,
}

/// Decision returned by a hook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookDecision {
    /// Whether the hook allows the action to proceed.
    pub allow: bool,
    /// Optional message to surface to the agent.
    pub message: Option<String>,
    /// Optional transformed arguments (for pre-tool hooks).
    pub transformed_args: Option<serde_json::Value>,
    /// Execution duration in microseconds (for telemetry).
    pub duration_us: u64,
}

impl Default for HookDecision {
    fn default() -> Self {
        Self {
            allow: true,
            message: None,
            transformed_args: None,
            duration_us: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_kind_deserializes_command() {
        let json = r#"{"type": "command", "command": "echo hello"}"#;
        let kind: HookKind = serde_json::from_str(json).unwrap();
        assert!(matches!(kind, HookKind::Command { .. }));
        assert_eq!(kind.kind_label(), "command");
        assert!(!kind.is_deterministic());
    }

    #[test]
    fn hook_kind_deserializes_wasm() {
        let json = r#"{"type": "wasm", "module_path": "hooks/lint.wasm"}"#;
        let kind: HookKind = serde_json::from_str(json).unwrap();
        assert!(matches!(kind, HookKind::Wasm { .. }));
        assert!(kind.is_deterministic());
        assert!(kind.is_sandboxed());
    }

    #[test]
    fn hook_kind_deserializes_prompt() {
        let json = r#"{"type": "prompt", "system": "You are a code reviewer."}"#;
        let kind: HookKind = serde_json::from_str(json).unwrap();
        assert!(matches!(kind, HookKind::Prompt { .. }));
    }

    #[test]
    fn hook_kind_deserializes_http() {
        let json = r#"{"type": "http", "url": "https://hooks.example.com/review"}"#;
        let kind: HookKind = serde_json::from_str(json).unwrap();
        assert!(matches!(kind, HookKind::Http { .. }));
    }

    #[test]
    fn hook_kind_deserializes_agent() {
        let json = r#"{"type": "agent", "task": "Review this edit for security issues"}"#;
        let kind: HookKind = serde_json::from_str(json).unwrap();
        assert!(matches!(kind, HookKind::Agent { .. }));
    }

    #[test]
    fn typed_manifest_matcher() {
        let manifest = TypedHookManifest {
            events: vec!["PreToolUse".into()],
            matcher: "bash|edit_file".into(),
            description: None,
            kind: HookKind::Command {
                command: "echo".into(),
                timeout_ms: 5000,
                shell: "sh".into(),
                env: HashMap::new(),
            },
            async_hook: false,
        };
        assert!(manifest.matches_tool("bash"));
        assert!(manifest.matches_tool("edit_file"));
        assert!(!manifest.matches_tool("read_file"));
    }

    #[test]
    fn exhaustiveness_enforced() {
        // This test exists to ensure the match is exhaustive.
        // If you add a new HookKind variant, this won't compile
        // until you add the arm — which is the point.
        let kind = HookKind::Command {
            command: "x".into(),
            timeout_ms: 1000,
            shell: "sh".into(),
            env: HashMap::new(),
        };
        let _label = kind.kind_label(); // exhaustive match inside
        let _det = kind.is_deterministic();
        let _sand = kind.is_sandboxed();
        let _timeout = kind.timeout();
    }
}
