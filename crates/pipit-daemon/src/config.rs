//! Daemon configuration with `${ENV_VAR}` expansion.
//!
//! Single-pass O(N) environment variable expansion supporting:
//! - `${VAR}` — value or empty string
//! - `${VAR:-default}` — value or fallback
//! - `$$` — literal dollar sign

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Top-level config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    #[serde(default)]
    pub server: ServerConfig,

    #[serde(default)]
    pub auth: AuthConfig,

    #[serde(default)]
    pub daemon: DaemonLifecycleConfig,

    #[serde(default)]
    pub projects: HashMap<String, ProjectConfig>,

    #[serde(default)]
    pub channels: HashMap<String, ChannelConfig>,

    #[serde(default)]
    pub schedules: HashMap<String, ScheduleConfig>,
}

impl DaemonConfig {
    /// Parse TOML with environment variable expansion.
    pub fn from_toml_str(raw: &str) -> Result<Self> {
        let expanded = expand_env_vars(raw);
        toml::from_str(&expanded).map_err(|e| anyhow!("config parse error: {e}"))
    }

    /// Validate all config invariants. Returns structured error listing all failures.
    pub fn validate(&self) -> Result<()> {
        let mut errors: Vec<String> = Vec::new();

        // Validate projects
        for (name, project) in &self.projects {
            if !project.root.exists() {
                errors.push(format!(
                    "project '{}': root path does not exist: {}",
                    name,
                    project.root.display()
                ));
            }
            if project.model.is_empty() {
                errors.push(format!("project '{}': model must not be empty", name));
            }
        }

        // Validate channels
        for (name, channel) in &self.channels {
            match channel {
                ChannelConfig::Telegram(tg) => {
                    if tg.bot_token.is_empty() {
                        errors.push(format!(
                            "channel '{}': telegram bot_token must not be empty",
                            name
                        ));
                    }
                }
                ChannelConfig::Discord(dc) => {
                    if dc.bot_token.is_empty() {
                        errors.push(format!(
                            "channel '{}': discord bot_token must not be empty",
                            name
                        ));
                    }
                }
                ChannelConfig::Webhook(wh) => {
                    if wh.secret.is_empty() {
                        errors.push(format!(
                            "channel '{}': webhook secret must not be empty",
                            name
                        ));
                    }
                }
            }
        }

        // Validate schedules reference valid projects
        for (name, schedule) in &self.schedules {
            if !self.projects.contains_key(&schedule.project) {
                errors.push(format!(
                    "schedule '{}': references unknown project '{}'",
                    name, schedule.project
                ));
            }
            if schedule.cron.is_empty() {
                errors.push(format!("schedule '{}': cron expression must not be empty", name));
            }
            if schedule.prompt.is_empty() {
                errors.push(format!("schedule '{}': prompt must not be empty", name));
            }
        }

        // Validate auth tokens aren't empty
        for (name, token) in &self.auth.tokens {
            if token.secret.is_empty() {
                errors.push(format!("auth token '{}': secret must not be empty", name));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(
                "configuration validation failed:\n  - {}",
                errors.join("\n  - ")
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Section configs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,

    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            port: default_port(),
        }
    }
}

fn default_bind() -> String {
    "127.0.0.1".to_string()
}
fn default_port() -> u16 {
    3100
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthConfig {
    #[serde(default)]
    pub tokens: HashMap<String, AuthToken>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthToken {
    pub secret: String,
    #[serde(default)]
    pub permissions: Vec<AuthPermission>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthPermission {
    Submit,
    Status,
    Cancel,
    Steer,
}

impl AuthToken {
    pub fn has_permission(&self, perm: AuthPermission) -> bool {
        self.permissions.is_empty() || self.permissions.contains(&perm)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonLifecycleConfig {
    #[serde(default = "default_drain_timeout")]
    pub drain_timeout_secs: u64,

    #[serde(default = "default_pid_path")]
    pub pid_path: PathBuf,

    #[serde(default = "default_store_path")]
    pub store_path: PathBuf,

    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,

    #[serde(default = "default_max_queue_depth")]
    pub max_queue_depth: usize,
}

impl Default for DaemonLifecycleConfig {
    fn default() -> Self {
        Self {
            drain_timeout_secs: default_drain_timeout(),
            pid_path: default_pid_path(),
            store_path: default_store_path(),
            max_concurrent: default_max_concurrent(),
            max_queue_depth: default_max_queue_depth(),
        }
    }
}

fn default_drain_timeout() -> u64 {
    30
}
fn default_pid_path() -> PathBuf {
    dirs_path(".pipit/pipitd.pid")
}
fn default_store_path() -> PathBuf {
    dirs_path(".pipit/daemon.db")
}
fn default_max_concurrent() -> usize {
    2
}
fn default_max_queue_depth() -> usize {
    20
}

fn dirs_path(suffix: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(suffix)
}

// ---------------------------------------------------------------------------
// Project config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub root: PathBuf,

    #[serde(default = "default_provider")]
    pub provider: String,

    #[serde(default = "default_model")]
    pub model: String,

    #[serde(default = "default_mode")]
    pub mode: String,

    #[serde(default)]
    pub auto_commit: bool,

    #[serde(default)]
    pub auto_push: bool,

    #[serde(default = "default_branch_prefix")]
    pub branch_prefix: String,

    #[serde(default)]
    pub protected_paths: Vec<String>,

    #[serde(default = "default_max_turns")]
    pub max_turns: u32,

    #[serde(default)]
    pub allowed_tools: Vec<String>,

    #[serde(default)]
    pub test_command: Option<String>,

    #[serde(default)]
    pub lint_command: Option<String>,
}

fn default_provider() -> String {
    "anthropic".to_string()
}
fn default_model() -> String {
    "claude-sonnet-4-20250514".to_string()
}
fn default_mode() -> String {
    "balanced".to_string()
}
fn default_branch_prefix() -> String {
    "pipit/".to_string()
}
fn default_max_turns() -> u32 {
    100
}

// ---------------------------------------------------------------------------
// Channel configs — tagged enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChannelConfig {
    Telegram(TelegramConfig),
    Discord(DiscordConfig),
    Webhook(WebhookConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    pub bot_token: String,

    #[serde(default)]
    pub allowed_users: Vec<i64>,

    #[serde(default)]
    pub default_project: Option<String>,

    /// Debounce interval for edit-in-place streaming (ms).
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordConfig {
    pub bot_token: String,

    #[serde(default)]
    pub allowed_guilds: Vec<u64>,

    #[serde(default)]
    pub default_project: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    pub secret: String,

    #[serde(default)]
    pub default_project: Option<String>,
}

fn default_debounce_ms() -> u64 {
    800
}

// ---------------------------------------------------------------------------
// Schedule config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleConfig {
    pub project: String,
    pub cron: String,
    pub prompt: String,

    #[serde(default)]
    pub priority: Option<pipit_channel::TaskPriority>,

    /// Channel to notify with results (references a channel name).
    #[serde(default)]
    pub notify_channel: Option<String>,
}

// ---------------------------------------------------------------------------
// Config path resolution
// ---------------------------------------------------------------------------

pub fn resolve_config_path() -> PathBuf {
    if let Ok(path) = std::env::var("PIPIT_DAEMON_CONFIG") {
        return PathBuf::from(path);
    }

    let candidates = [
        PathBuf::from("daemon.toml"),
        PathBuf::from(".pipit/daemon.toml"),
        dirs_path(".config/pipit/daemon.toml"),
    ];

    for candidate in &candidates {
        if candidate.exists() {
            return candidate.clone();
        }
    }

    // Default: expect daemon.toml in cwd
    PathBuf::from("daemon.toml")
}

// ---------------------------------------------------------------------------
// Environment variable expansion — O(N) single-pass state machine
// ---------------------------------------------------------------------------

/// Expand `${VAR}`, `${VAR:-default}`, and `$$` in a string.
/// 3-state machine: Normal, InVarName, InDefault.
pub fn expand_env_vars(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        if chars[i] == '$' {
            if i + 1 < len && chars[i + 1] == '$' {
                // Escaped dollar
                result.push('$');
                i += 2;
            } else if i + 1 < len && chars[i + 1] == '{' {
                // Start of ${...}
                i += 2; // skip "${"
                let mut var_name = String::new();
                let mut default_val = String::new();
                let mut in_default = false;

                while i < len && chars[i] != '}' {
                    if !in_default && chars[i] == ':' && i + 1 < len && chars[i + 1] == '-' {
                        in_default = true;
                        i += 2; // skip ":-"
                        continue;
                    }
                    if in_default {
                        default_val.push(chars[i]);
                    } else {
                        var_name.push(chars[i]);
                    }
                    i += 1;
                }

                if i < len {
                    i += 1; // skip '}'
                }

                match std::env::var(&var_name) {
                    Ok(val) if !val.is_empty() => result.push_str(&val),
                    _ => {
                        if in_default {
                            result.push_str(&default_val);
                        }
                        // If no default and var not set, emit empty string
                    }
                }
            } else {
                result.push('$');
                i += 1;
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_env_vars_basic() {
        std::env::set_var("PIPIT_TEST_VAR", "hello");
        assert_eq!(expand_env_vars("${PIPIT_TEST_VAR}"), "hello");
        std::env::remove_var("PIPIT_TEST_VAR");
    }

    #[test]
    fn test_expand_env_vars_default() {
        std::env::remove_var("PIPIT_MISSING_VAR");
        assert_eq!(
            expand_env_vars("${PIPIT_MISSING_VAR:-fallback}"),
            "fallback"
        );
    }

    #[test]
    fn test_expand_env_vars_escaped_dollar() {
        assert_eq!(expand_env_vars("price is $$5"), "price is $5");
    }

    #[test]
    fn test_expand_env_vars_no_expansion() {
        assert_eq!(expand_env_vars("no vars here"), "no vars here");
    }

    #[test]
    fn test_minimal_config() {
        let toml_str = r#"
[server]
port = 3200

[projects.myapp]
root = "/tmp/myapp"
model = "claude-sonnet-4-20250514"
"#;
        let config = DaemonConfig::from_toml_str(toml_str).unwrap();
        assert_eq!(config.server.port, 3200);
        assert!(config.projects.contains_key("myapp"));
    }
}
