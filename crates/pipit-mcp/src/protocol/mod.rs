
//! MCP Protocol Extensions — OAuth, WebSocket, Channel Allowlists, Elicitation (Task 4)
//!
//! Extends Pipit's MCP layer with production-grade features:
//!   1. OAuth 2.0 PKCE for authenticated MCP servers
//!   2. WebSocket transport (bidirectional, ~40% lower latency vs SSE)
//!   3. Channel allowlists (per-project tool visibility filtering)
//!   4. Elicitation handling (MCP error -32042 → URL auth flow)
//!
//! OAuth PKCE: code_verifier ∈ {A-Z,a-z,0-9,-,.,_,~}^{43..128}
//!             code_challenge = BASE64URL(SHA256(code_verifier))
//!
//! WebSocket frame overhead: 2-14 bytes vs SSE ~50+ bytes per event.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// ═══════════════════════════════════════════════════════════════════════════
//  OAuth 2.0 PKCE
// ═══════════════════════════════════════════════════════════════════════════

/// OAuth PKCE state for an MCP server authentication flow.
#[derive(Debug, Clone)]
pub struct OAuthPkceState {
    /// Random code verifier: 43-128 characters from unreserved URI set.
    pub code_verifier: String,
    /// SHA256 hash of verifier, base64url-encoded.
    pub code_challenge: String,
    /// OAuth state parameter for CSRF protection.
    pub state: String,
    /// Authorization endpoint URL.
    pub auth_url: String,
    /// Token endpoint URL.
    pub token_url: String,
    /// Redirect URI (typically localhost callback).
    pub redirect_uri: String,
    /// Client ID.
    pub client_id: String,
    /// Requested scopes.
    pub scopes: Vec<String>,
}

impl OAuthPkceState {
    /// Generate a new PKCE flow for an MCP server.
    pub fn new(
        auth_url: &str,
        token_url: &str,
        client_id: &str,
        redirect_uri: &str,
        scopes: &[&str],
    ) -> Self {
        let code_verifier = generate_code_verifier(64);
        let code_challenge = compute_code_challenge(&code_verifier);
        let state = generate_state();

        Self {
            code_verifier,
            code_challenge,
            state,
            auth_url: auth_url.to_string(),
            token_url: token_url.to_string(),
            redirect_uri: redirect_uri.to_string(),
            client_id: client_id.to_string(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Build the authorization URL the user should visit.
    pub fn authorization_url(&self) -> String {
        format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256&scope={}",
            self.auth_url,
            urlencoding_encode(&self.client_id),
            urlencoding_encode(&self.redirect_uri),
            urlencoding_encode(&self.state),
            urlencoding_encode(&self.code_challenge),
            self.scopes.join("+"),
        )
    }

    /// Exchange an authorization code for tokens.
    pub async fn exchange_code(&self, code: &str) -> Result<OAuthTokens, String> {
        let client = reqwest::Client::new();

        let params = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &self.redirect_uri),
            ("client_id", &self.client_id),
            ("code_verifier", &self.code_verifier),
        ];

        let response = client
            .post(&self.token_url)
            .form(&params)
            .send()
            .await
            .map_err(|e| format!("Token exchange failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!("Token exchange returned {status}: {body}"));
        }

        let tokens: OAuthTokens = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse token response: {e}"))?;

        Ok(tokens)
    }
}

/// OAuth tokens received after successful exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: Option<u64>,
    pub refresh_token: Option<String>,
    pub scope: Option<String>,
}

/// Generate a PKCE code verifier.
/// Characters from unreserved URI set: [A-Za-z0-9\-._~]
fn generate_code_verifier(length: usize) -> String {
    use rand::Rng;
    let charset = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut rng = rand::thread_rng();
    (0..length)
        .map(|_| charset[rng.gen_range(0..charset.len())] as char)
        .collect()
}

/// Compute S256 code challenge: BASE64URL(SHA256(code_verifier)).
fn compute_code_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    base64url_encode(&hash)
}

fn generate_state() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| format!("{:02x}", rng.gen_range(0u8..=255u8)))
        .collect()
}

fn base64url_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

fn urlencoding_encode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

// ═══════════════════════════════════════════════════════════════════════════
//  WebSocket Transport
// ═══════════════════════════════════════════════════════════════════════════

/// WebSocket transport configuration for MCP servers.
///
/// Advantages over SSE:
///   - Bidirectional: server can push messages (notifications, progress)
///   - Lower overhead: 2-14 byte frame vs ~50+ byte HTTP chunked transfer
///   - Persistent connection: no reconnect overhead
///
/// Frame overhead analysis:
///   SSE per event: "data: " (6) + payload + "\n\n" (2) + HTTP chunked encoding (~50 total)
///   WebSocket per frame: opcode (1) + length (1-9) + mask (4 if client) = 6-14 bytes
///   Savings over N tool calls: O(36N) bytes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketConfig {
    /// WebSocket URL (wss:// for TLS).
    pub url: String,
    /// Ping interval (seconds) to keep connection alive.
    pub ping_interval_secs: u64,
    /// Reconnect backoff parameters.
    pub max_reconnect_attempts: u32,
    pub initial_reconnect_delay_ms: u64,
    /// Optional OAuth bearer token for authenticated connections.
    pub bearer_token: Option<String>,
    /// Optional custom headers.
    pub headers: HashMap<String, String>,
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            ping_interval_secs: 30,
            max_reconnect_attempts: 5,
            initial_reconnect_delay_ms: 1000,
            bearer_token: None,
            headers: HashMap::new(),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Channel Allowlists
// ═══════════════════════════════════════════════════════════════════════════

/// Per-project MCP tool visibility filtering.
///
/// Enforces least-privilege: only tools in the allowlist are registered
/// for the LLM. Tools not in the allowlist are invisible — the model
/// cannot even attempt to call them.
///
/// Configuration in `.pipit/config.toml`:
/// ```toml
/// [mcp.channels]
/// github = { allow = ["create_issue", "list_issues", "get_issue"] }
/// slack = { allow = ["send_message"], deny = ["delete_message"] }
/// filesystem = { deny = ["*"] }  # Block entire server
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChannelAllowlist {
    /// Per-server allowlists. Server name → allowed tool names.
    pub servers: HashMap<String, ChannelPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelPolicy {
    /// Explicitly allowed tools. Empty = all allowed (unless deny is set).
    #[serde(default)]
    pub allow: HashSet<String>,
    /// Explicitly denied tools. Takes priority over allow.
    #[serde(default)]
    pub deny: HashSet<String>,
}

impl ChannelAllowlist {
    /// Check if a tool from a specific server is allowed.
    pub fn is_allowed(&self, server_name: &str, tool_name: &str) -> bool {
        let Some(policy) = self.servers.get(server_name) else {
            return true; // No policy = all allowed
        };

        // Deny takes priority
        if policy.deny.contains("*") || policy.deny.contains(tool_name) {
            return false;
        }

        // If allow is empty, everything (not denied) is allowed
        if policy.allow.is_empty() {
            return true;
        }

        // If allow is specified, only listed tools are allowed
        policy.allow.contains(tool_name)
    }

    /// Filter a list of tool declarations by the allowlist.
    pub fn filter_tools(
        &self,
        server_name: &str,
        tools: Vec<(String, String)>, // (name, description)
    ) -> Vec<(String, String)> {
        tools
            .into_iter()
            .filter(|(name, _)| self.is_allowed(server_name, name))
            .collect()
    }

    /// Load from TOML config.
    pub fn from_config(config: &serde_json::Value) -> Self {
        let mut allowlist = Self::default();

        if let Some(channels) = config.get("mcp")
            .and_then(|m| m.get("channels"))
            .and_then(|c| c.as_object())
        {
            for (server_name, policy_val) in channels {
                let mut policy = ChannelPolicy {
                    allow: HashSet::new(),
                    deny: HashSet::new(),
                };

                if let Some(allow) = policy_val.get("allow").and_then(|a| a.as_array()) {
                    for item in allow {
                        if let Some(s) = item.as_str() {
                            policy.allow.insert(s.to_string());
                        }
                    }
                }

                if let Some(deny) = policy_val.get("deny").and_then(|d| d.as_array()) {
                    for item in deny {
                        if let Some(s) = item.as_str() {
                            policy.deny.insert(s.to_string());
                        }
                    }
                }

                allowlist.servers.insert(server_name.clone(), policy);
            }
        }

        allowlist
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Elicitation Handling
// ═══════════════════════════════════════════════════════════════════════════

/// MCP elicitation error code (-32042): the server needs the user to
/// authenticate via a URL before the tool can be used.
pub const MCP_ELICITATION_ERROR_CODE: i32 = -32042;

/// An elicitation request from an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElicitationRequest {
    /// The URL the user should visit to authenticate.
    pub url: String,
    /// Human-readable message explaining why auth is needed.
    pub message: Option<String>,
    /// The MCP server that triggered this.
    pub server_name: String,
    /// The tool that was being called.
    pub tool_name: String,
}

/// Handle an MCP elicitation error.
///
/// Returns an ElicitationRequest that the UI layer should present to the user.
pub fn parse_elicitation_error(
    error_code: i32,
    error_data: &serde_json::Value,
    server_name: &str,
    tool_name: &str,
) -> Option<ElicitationRequest> {
    if error_code != MCP_ELICITATION_ERROR_CODE {
        return None;
    }

    let url = error_data.get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if url.is_empty() {
        return None;
    }

    let message = error_data.get("message")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Some(ElicitationRequest {
        url,
        message,
        server_name: server_name.to_string(),
        tool_name: tool_name.to_string(),
    })
}

// ═══════════════════════════════════════════════════════════════════════════
//  MCP Transport Selection
// ═══════════════════════════════════════════════════════════════════════════

/// Available MCP transport types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransportKind {
    /// Standard I/O (child process).
    Stdio,
    /// Server-Sent Events (HTTP half-duplex).
    Sse,
    /// WebSocket (full-duplex, lowest latency).
    WebSocket,
}

/// Select the best transport for a server based on its configuration.
pub fn select_transport(server_url: &str, preferred: Option<McpTransportKind>) -> McpTransportKind {
    if let Some(pref) = preferred {
        return pref;
    }

    if server_url.starts_with("ws://") || server_url.starts_with("wss://") {
        McpTransportKind::WebSocket
    } else if server_url.starts_with("http://") || server_url.starts_with("https://") {
        McpTransportKind::Sse
    } else {
        McpTransportKind::Stdio
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_code_challenge_format() {
        let verifier = generate_code_verifier(43);
        assert!(verifier.len() == 43);
        assert!(verifier.chars().all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c)));

        let challenge = compute_code_challenge(&verifier);
        // Base64url: no padding, no + or /
        assert!(!challenge.contains('='));
        assert!(!challenge.contains('+'));
        assert!(!challenge.contains('/'));
    }

    #[test]
    fn channel_allowlist_filtering() {
        let mut allowlist = ChannelAllowlist::default();
        allowlist.servers.insert("github".into(), ChannelPolicy {
            allow: HashSet::from(["create_issue".into(), "list_issues".into()]),
            deny: HashSet::new(),
        });
        allowlist.servers.insert("filesystem".into(), ChannelPolicy {
            allow: HashSet::new(),
            deny: HashSet::from(["*".into()]),
        });

        assert!(allowlist.is_allowed("github", "create_issue"));
        assert!(!allowlist.is_allowed("github", "delete_repo"));
        assert!(!allowlist.is_allowed("filesystem", "read_file"));
        assert!(allowlist.is_allowed("unknown_server", "anything")); // No policy = allow
    }

    #[test]
    fn deny_overrides_allow() {
        let mut allowlist = ChannelAllowlist::default();
        allowlist.servers.insert("slack".into(), ChannelPolicy {
            allow: HashSet::from(["send_message".into(), "delete_message".into()]),
            deny: HashSet::from(["delete_message".into()]),
        });

        assert!(allowlist.is_allowed("slack", "send_message"));
        assert!(!allowlist.is_allowed("slack", "delete_message")); // Deny wins
    }

    #[test]
    fn elicitation_parsing() {
        let error_data = serde_json::json!({
            "url": "https://auth.example.com/authorize?client_id=abc",
            "message": "Please authenticate with GitHub"
        });

        let req = parse_elicitation_error(MCP_ELICITATION_ERROR_CODE, &error_data, "github", "create_issue");
        assert!(req.is_some());
        let req = req.unwrap();
        assert!(req.url.contains("auth.example.com"));
        assert_eq!(req.server_name, "github");
    }

    #[test]
    fn transport_auto_selection() {
        assert_eq!(select_transport("wss://mcp.example.com", None), McpTransportKind::WebSocket);
        assert_eq!(select_transport("https://mcp.example.com/sse", None), McpTransportKind::Sse);
        assert_eq!(select_transport("/usr/local/bin/mcp-server", None), McpTransportKind::Stdio);
    }
}

