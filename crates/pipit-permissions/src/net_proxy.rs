//! # Network Proxy Allowlist (A2)
//!
//! Per-domain network allowlist with mid-session permission escalation.
//! All outbound connections from sandboxed commands are validated against
//! an allowlist. Unknown domains trigger an interactive permission ask
//! (in supervised mode) or are denied (in auto mode).
//!
//! ## Architecture
//!
//! ```text
//! sandboxed command → NetProxy::check_domain(host)
//!   → AllowedPermanently | AllowedSession | Denied | NeedsAsk
//! ```
//!
//! The proxy maintains three tiers:
//! 1. **Permanent allowlist** — configured in .pipit/network.toml
//! 2. **Session allowlist** — domains approved mid-session by the user
//! 3. **Deny list** — explicitly blocked domains

use std::collections::HashSet;
use std::sync::Mutex;

/// Decision from the network proxy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetDecision {
    /// Domain is on the permanent allowlist.
    AllowedPermanent,
    /// Domain was approved during this session.
    AllowedSession,
    /// Domain is explicitly denied.
    Denied(String),
    /// Domain is unknown — needs user approval.
    NeedsAsk,
}

/// Reason a domain is blocked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyReason {
    ExplicitDeny,
    NotInAllowlist,
    UserDenied,
    SuspiciousPattern,
}

/// Network proxy configuration.
#[derive(Debug, Clone)]
pub struct NetProxyConfig {
    /// Permanently allowed domains (from .pipit/network.toml).
    pub allowed_domains: Vec<String>,
    /// Explicitly denied domains.
    pub denied_domains: Vec<String>,
    /// If true, unknown domains are denied without asking (full_auto mode).
    pub auto_deny_unknown: bool,
    /// If true, all network access is blocked.
    pub block_all: bool,
    /// Maximum number of session-approved domains (prevent runaway approvals).
    pub max_session_approvals: usize,
}

impl Default for NetProxyConfig {
    fn default() -> Self {
        Self {
            allowed_domains: vec![
                // Package registries
                "github.com".into(),
                "api.github.com".into(),
                "npmjs.com".into(),
                "registry.npmjs.org".into(),
                "pypi.org".into(),
                "files.pythonhosted.org".into(),
                "crates.io".into(),
                "static.crates.io".into(),
                "rubygems.org".into(),
                "packagist.org".into(),
                "repo.maven.apache.org".into(),
                // CDNs commonly used by dev tools
                "objects.githubusercontent.com".into(),
                "raw.githubusercontent.com".into(),
                // Git hosts
                "gitlab.com".into(),
                "bitbucket.org".into(),
            ],
            denied_domains: vec![
                // Exfiltration endpoints
                "ngrok.io".into(),
                "requestbin.com".into(),
                "webhook.site".into(),
                "burpcollaborator.net".into(),
                "interact.sh".into(),
                "oastify.com".into(),
            ],
            auto_deny_unknown: false,
            block_all: false,
            max_session_approvals: 20,
        }
    }
}

/// The network proxy — validates outbound connections against the allowlist.
pub struct NetProxy {
    config: NetProxyConfig,
    /// Domains approved during this session.
    session_approved: Mutex<HashSet<String>>,
    /// Domains denied during this session (user said no).
    session_denied: Mutex<HashSet<String>>,
    /// Connection log: (domain, timestamp_epoch_s, allowed).
    connection_log: Mutex<Vec<ConnectionEvent>>,
}

/// A logged connection event.
#[derive(Debug, Clone)]
pub struct ConnectionEvent {
    pub domain: String,
    pub timestamp: u64,
    pub allowed: bool,
    pub reason: String,
}

impl NetProxy {
    pub fn new(config: NetProxyConfig) -> Self {
        Self {
            config,
            session_approved: Mutex::new(HashSet::new()),
            session_denied: Mutex::new(HashSet::new()),
            connection_log: Mutex::new(Vec::new()),
        }
    }

    /// Check if a domain is allowed for outbound connections.
    pub fn check_domain(&self, host: &str) -> NetDecision {
        if self.config.block_all {
            return NetDecision::Denied("all network access blocked".into());
        }

        let normalized = normalize_domain(host);

        // Check explicit deny list first (deny > allow)
        if self.is_denied(&normalized) {
            self.log_event(&normalized, false, "explicit deny list");
            return NetDecision::Denied(format!("domain '{}' is explicitly denied", normalized));
        }

        // Check session denials
        if let Ok(denied) = self.session_denied.lock() {
            if denied.contains(&normalized) {
                self.log_event(&normalized, false, "user denied this session");
                return NetDecision::Denied("user denied this domain during session".into());
            }
        }

        // Check permanent allowlist
        if self.is_allowed_permanent(&normalized) {
            self.log_event(&normalized, true, "permanent allowlist");
            return NetDecision::AllowedPermanent;
        }

        // Check session approvals
        if let Ok(approved) = self.session_approved.lock() {
            if approved.contains(&normalized) {
                self.log_event(&normalized, true, "session approval");
                return NetDecision::AllowedSession;
            }
        }

        // Unknown domain
        if self.config.auto_deny_unknown {
            self.log_event(&normalized, false, "auto-deny (full_auto mode)");
            return NetDecision::Denied("domain not in allowlist (auto mode)".into());
        }

        NetDecision::NeedsAsk
    }

    /// Record user's approval for a domain during this session.
    pub fn approve_session(&self, host: &str) -> Result<(), String> {
        let normalized = normalize_domain(host);
        if let Ok(mut approved) = self.session_approved.lock() {
            if approved.len() >= self.config.max_session_approvals {
                return Err(format!(
                    "maximum session approvals ({}) reached",
                    self.config.max_session_approvals
                ));
            }
            approved.insert(normalized.clone());
            self.log_event(&normalized, true, "user approved this session");
            Ok(())
        } else {
            Err("lock poisoned".into())
        }
    }

    /// Record user's denial for a domain during this session.
    pub fn deny_session(&self, host: &str) {
        let normalized = normalize_domain(host);
        if let Ok(mut denied) = self.session_denied.lock() {
            denied.insert(normalized.clone());
            self.log_event(&normalized, false, "user explicitly denied");
        }
    }

    /// Get all connection events for this session.
    pub fn connection_log(&self) -> Vec<ConnectionEvent> {
        self.connection_log
            .lock()
            .map(|log| log.clone())
            .unwrap_or_default()
    }

    /// Get the count of session-approved domains.
    pub fn session_approval_count(&self) -> usize {
        self.session_approved
            .lock()
            .map(|s| s.len())
            .unwrap_or(0)
    }

    /// Check if a URL's host is allowed.
    pub fn check_url(&self, url: &str) -> NetDecision {
        match extract_host(url) {
            Some(host) => self.check_domain(&host),
            None => NetDecision::Denied("could not parse host from URL".into()),
        }
    }

    fn is_denied(&self, domain: &str) -> bool {
        self.config
            .denied_domains
            .iter()
            .any(|d| domain == d || domain.ends_with(&format!(".{}", d)))
    }

    fn is_allowed_permanent(&self, domain: &str) -> bool {
        self.config
            .allowed_domains
            .iter()
            .any(|d| domain == d || domain.ends_with(&format!(".{}", d)))
    }

    fn log_event(&self, domain: &str, allowed: bool, reason: &str) {
        if let Ok(mut log) = self.connection_log.lock() {
            log.push(ConnectionEvent {
                domain: domain.to_string(),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                allowed,
                reason: reason.to_string(),
            });
        }
    }
}

/// Normalize a domain: lowercase, strip port, strip trailing dot.
fn normalize_domain(host: &str) -> String {
    let h = host.to_lowercase();
    let h = h.split(':').next().unwrap_or(&h);
    h.trim_end_matches('.').to_string()
}

/// Extract host from a URL string.
fn extract_host(url: &str) -> Option<String> {
    // Handle scheme://host:port/path
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host_port = without_scheme.split('/').next()?;
    let host = host_port.split(':').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permanent_allowlist_match() {
        let proxy = NetProxy::new(NetProxyConfig::default());
        assert_eq!(proxy.check_domain("github.com"), NetDecision::AllowedPermanent);
        assert_eq!(proxy.check_domain("api.github.com"), NetDecision::AllowedPermanent);
        assert_eq!(proxy.check_domain("registry.npmjs.org"), NetDecision::AllowedPermanent);
    }

    #[test]
    fn explicit_deny_overrides_everything() {
        let proxy = NetProxy::new(NetProxyConfig::default());
        assert!(matches!(proxy.check_domain("ngrok.io"), NetDecision::Denied(_)));
        assert!(matches!(proxy.check_domain("evil.ngrok.io"), NetDecision::Denied(_)));
    }

    #[test]
    fn unknown_domain_needs_ask() {
        let proxy = NetProxy::new(NetProxyConfig::default());
        assert_eq!(proxy.check_domain("example.com"), NetDecision::NeedsAsk);
    }

    #[test]
    fn auto_deny_in_full_auto_mode() {
        let config = NetProxyConfig {
            auto_deny_unknown: true,
            ..Default::default()
        };
        let proxy = NetProxy::new(config);
        assert!(matches!(proxy.check_domain("example.com"), NetDecision::Denied(_)));
    }

    #[test]
    fn session_approval_flow() {
        let proxy = NetProxy::new(NetProxyConfig::default());
        assert_eq!(proxy.check_domain("custom-api.example.com"), NetDecision::NeedsAsk);

        proxy.approve_session("custom-api.example.com").unwrap();
        assert_eq!(
            proxy.check_domain("custom-api.example.com"),
            NetDecision::AllowedSession
        );
    }

    #[test]
    fn session_denial_flow() {
        let proxy = NetProxy::new(NetProxyConfig::default());
        proxy.deny_session("evil.example.com");
        assert!(matches!(
            proxy.check_domain("evil.example.com"),
            NetDecision::Denied(_)
        ));
    }

    #[test]
    fn max_session_approvals_enforced() {
        let config = NetProxyConfig {
            max_session_approvals: 2,
            ..Default::default()
        };
        let proxy = NetProxy::new(config);
        proxy.approve_session("a.com").unwrap();
        proxy.approve_session("b.com").unwrap();
        assert!(proxy.approve_session("c.com").is_err());
    }

    #[test]
    fn block_all_denies_everything() {
        let config = NetProxyConfig {
            block_all: true,
            ..Default::default()
        };
        let proxy = NetProxy::new(config);
        assert!(matches!(proxy.check_domain("github.com"), NetDecision::Denied(_)));
    }

    #[test]
    fn url_host_extraction() {
        let proxy = NetProxy::new(NetProxyConfig::default());
        assert_eq!(
            proxy.check_url("https://github.com/user/repo"),
            NetDecision::AllowedPermanent
        );
        assert!(matches!(
            proxy.check_url("https://ngrok.io/tunnel"),
            NetDecision::Denied(_)
        ));
    }

    #[test]
    fn normalize_strips_port_and_trailing_dot() {
        assert_eq!(normalize_domain("GitHub.COM:443."), "github.com");
        assert_eq!(normalize_domain("API.Example.COM"), "api.example.com");
    }

    #[test]
    fn connection_log_recorded() {
        let proxy = NetProxy::new(NetProxyConfig::default());
        proxy.check_domain("github.com");
        proxy.check_domain("evil.ngrok.io");
        let log = proxy.connection_log();
        assert_eq!(log.len(), 2);
        assert!(log[0].allowed);
        assert!(!log[1].allowed);
    }
}
