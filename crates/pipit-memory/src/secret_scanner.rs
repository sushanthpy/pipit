
//! Secret Scanner — Prevents sensitive data from leaking into memory files.
//!
//! Uses pattern matching to detect:
//!   - API keys (AWS, GCP, Azure, GitHub, Anthropic, OpenAI, Stripe, etc.)
//!   - Tokens (JWT, OAuth, bearer)
//!   - Passwords (password= assignments, connection strings)
//!   - Private keys (PEM headers, SSH keys)
//!
//! Complexity: O(n · p) where n = text length, p = number of patterns (~30).
//! Each pattern is a compiled regex, applied sequentially.

use std::borrow::Cow;

/// A detected secret in the text.
#[derive(Debug, Clone)]
pub struct SecretFinding {
    /// The pattern that matched.
    pub pattern_name: &'static str,
    /// Byte offset of the match start.
    pub offset: usize,
    /// Length of the match.
    pub length: usize,
    /// The matched text (redacted to first/last 4 chars).
    pub redacted: String,
    /// Severity: how likely this is a real secret.
    pub severity: SecretSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretSeverity {
    /// Definitely a secret (private key, connection string with password).
    Critical,
    /// Very likely a secret (API key matching known format).
    High,
    /// Possibly a secret (generic token-like string).
    Medium,
}

/// Secret pattern definition.
struct SecretPattern {
    name: &'static str,
    pattern: &'static str,
    severity: SecretSeverity,
}

/// All secret patterns to scan for.
const PATTERNS: &[SecretPattern] = &[
    // AWS
    SecretPattern { name: "aws_access_key", pattern: r"AKIA[0-9A-Z]{16}", severity: SecretSeverity::Critical },
    SecretPattern { name: "aws_secret_key", pattern: r"(?i)aws_secret_access_key\s*[=:]\s*[A-Za-z0-9/+=]{40}", severity: SecretSeverity::Critical },
    // GitHub
    SecretPattern { name: "github_token", pattern: r"gh[ps]_[A-Za-z0-9_]{36,}", severity: SecretSeverity::High },
    SecretPattern { name: "github_fine_grained", pattern: r"github_pat_[A-Za-z0-9_]{22,}", severity: SecretSeverity::High },
    // Anthropic
    SecretPattern { name: "anthropic_api_key", pattern: r"sk-ant-[A-Za-z0-9_-]{40,}", severity: SecretSeverity::Critical },
    // OpenAI
    SecretPattern { name: "openai_api_key", pattern: r"sk-[A-Za-z0-9]{48,}", severity: SecretSeverity::Critical },
    // Stripe
    SecretPattern { name: "stripe_secret", pattern: r"sk_(live|test)_[A-Za-z0-9]{24,}", severity: SecretSeverity::Critical },
    SecretPattern { name: "stripe_restricted", pattern: r"rk_(live|test)_[A-Za-z0-9]{24,}", severity: SecretSeverity::High },
    // Google
    SecretPattern { name: "gcp_api_key", pattern: r"AIza[0-9A-Za-z_-]{35}", severity: SecretSeverity::High },
    SecretPattern { name: "gcp_service_account", pattern: r#""type"\s*:\s*"service_account""#, severity: SecretSeverity::High },
    // Azure
    SecretPattern { name: "azure_storage_key", pattern: r"DefaultEndpointsProtocol=https;AccountName=[^;]+;AccountKey=[^;]+", severity: SecretSeverity::Critical },
    // Generic tokens
    SecretPattern { name: "bearer_token", pattern: r"(?i)bearer\s+[A-Za-z0-9_.~+/=-]{20,}", severity: SecretSeverity::Medium },
    SecretPattern { name: "basic_auth", pattern: r"(?i)basic\s+[A-Za-z0-9+/=]{20,}", severity: SecretSeverity::Medium },
    // Private keys
    SecretPattern { name: "private_key_pem", pattern: r"-----BEGIN (?:RSA |EC |DSA |OPENSSH )?PRIVATE KEY-----", severity: SecretSeverity::Critical },
    // Connection strings
    SecretPattern { name: "connection_string", pattern: r"(?i)(?:mongodb|postgres|mysql|redis|amqp)://[^\s]+:[^\s]+@", severity: SecretSeverity::Critical },
    // Password assignments
    SecretPattern { name: "password_assignment", pattern: r#"(?i)(?:password|passwd|pwd|secret|token)\s*[=:]\s*["'][^"']{8,}["']"#, severity: SecretSeverity::High },
    // Slack
    SecretPattern { name: "slack_token", pattern: r"xox[bprs]-[0-9]{10,13}-[0-9]{10,13}-[a-zA-Z0-9]{24,34}", severity: SecretSeverity::High },
    // Twilio
    SecretPattern { name: "twilio_api_key", pattern: r"SK[0-9a-fA-F]{32}", severity: SecretSeverity::High },
    // SendGrid
    SecretPattern { name: "sendgrid_api_key", pattern: r"SG\.[A-Za-z0-9_-]{22}\.[A-Za-z0-9_-]{43}", severity: SecretSeverity::High },
    // npm
    SecretPattern { name: "npm_token", pattern: r"npm_[A-Za-z0-9]{36}", severity: SecretSeverity::High },
    // PyPI
    SecretPattern { name: "pypi_token", pattern: r"pypi-[A-Za-z0-9_-]{100,}", severity: SecretSeverity::High },
];

/// Scan text for secrets. Returns all findings.
pub fn scan(text: &str) -> Vec<SecretFinding> {
    let mut findings = Vec::new();

    for pattern_def in PATTERNS {
        // Compile regex (in production, these would be pre-compiled with lazy_static)
        let re = match regex::Regex::new(pattern_def.pattern) {
            Ok(r) => r,
            Err(_) => continue,
        };

        for mat in re.find_iter(text) {
            let matched = mat.as_str();
            let redacted = redact(matched);

            findings.push(SecretFinding {
                pattern_name: pattern_def.name,
                offset: mat.start(),
                length: mat.len(),
                redacted,
                severity: pattern_def.severity,
            });
        }
    }

    findings
}

/// Redact a secret, showing only first 4 and last 4 characters.
fn redact(secret: &str) -> String {
    if secret.len() <= 12 {
        return "*".repeat(secret.len());
    }
    let first = &secret[..4];
    let last = &secret[secret.len() - 4..];
    let stars = "*".repeat(secret.len() - 8);
    format!("{first}{stars}{last}")
}

/// Scan and strip secrets from text, replacing with redacted versions.
pub fn sanitize(text: &str) -> (String, Vec<SecretFinding>) {
    let findings = scan(text);
    if findings.is_empty() {
        return (text.to_string(), findings);
    }

    let mut result = text.to_string();
    // Apply replacements in reverse order to preserve offsets
    let mut sorted_findings = findings.clone();
    sorted_findings.sort_by(|a, b| b.offset.cmp(&a.offset));

    for finding in &sorted_findings {
        let end = finding.offset + finding.length;
        if end <= result.len() {
            result.replace_range(finding.offset..end, &finding.redacted);
        }
    }

    (result, findings)
}

/// Check if text contains any secrets. Quick boolean check.
pub fn contains_secrets(text: &str) -> bool {
    for pattern_def in PATTERNS {
        if let Ok(re) = regex::Regex::new(pattern_def.pattern) {
            if re.is_match(text) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_aws_key() {
        let text = "aws_key = AKIAIOSFODNN7EXAMPLE";
        let findings = scan(text);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].pattern_name, "aws_access_key");
    }

    #[test]
    fn detects_github_token() {
        let text = "token: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij";
        let findings = scan(text);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].pattern_name, "github_token");
    }

    #[test]
    fn detects_private_key() {
        let text = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpA...";
        let findings = scan(text);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].severity, SecretSeverity::Critical);
    }

    #[test]
    fn sanitize_replaces_secrets() {
        let text = "my key is AKIAIOSFODNN7EXAMPLE ok";
        let (sanitized, findings) = sanitize(text);
        assert!(!findings.is_empty());
        assert!(!sanitized.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(sanitized.contains("AKIA")); // first 4 chars preserved
    }

    #[test]
    fn no_false_positives_on_normal_text() {
        let text = "This is a normal comment about project architecture.\nNo secrets here.";
        let findings = scan(text);
        assert!(findings.is_empty());
    }

    #[test]
    fn detects_connection_string() {
        let text = "DATABASE_URL=postgres://admin:supersecret@db.example.com:5432/mydb";
        assert!(contains_secrets(text));
    }
}

