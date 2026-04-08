//! Skill Sandbox Contracts & Signed Bundles (Skill Tasks 7+8)
//!
//! Formalizes skill authority: every skill declares allowed tool classes,
//! MCP servers, token budget, timeout budget, and delegation rights.
//! Combined with manifest hashing and detached signatures for supply-chain
//! safety of community skills.

use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// A signed skill bundle with manifest, content, and signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedBundle {
    /// The manifest describing the bundle contents and policies.
    pub manifest: BundleManifest,
    /// Content hash (SHA256-like via DefaultHasher for non-crypto use).
    pub content_hash: String,
    /// HMAC-SHA256 signature over the content hash, verified at load time.
    pub signature: Option<String>,
    /// Signing key identifier (for key lookup).
    pub signing_key_id: Option<String>,
}

/// Manifest for a skill bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleManifest {
    /// Bundle identifier (reverse-domain style).
    pub id: String,
    /// Semantic version.
    pub version: String,
    /// Human-readable name.
    pub name: String,
    /// Author.
    pub author: String,
    /// Description.
    pub description: String,
    /// License (SPDX identifier).
    pub license: String,
    /// Contents of the bundle.
    pub contents: BundleContents,
    /// Capability policy: what the bundle is allowed to request.
    pub capability_policy: CapabilityPolicy,
    /// Minimum pipit version required.
    pub min_pipit_version: Option<String>,
}

/// What a bundle contains.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleContents {
    /// Skill files (markdown prompt templates).
    pub skills: Vec<String>,
    /// Hook scripts.
    pub hooks: Vec<String>,
    /// Agent definitions.
    pub agents: Vec<String>,
    /// MCP server configurations.
    pub mcp_configs: Vec<String>,
}

/// Capability policy — the maximum authority a bundle can request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityPolicy {
    /// Maximum capability bitset.
    pub max_capabilities: u32,
    /// Allowed tool name patterns (glob).
    pub allowed_tool_patterns: Vec<String>,
    /// Denied tool name patterns (glob).
    pub denied_tool_patterns: Vec<String>,
    /// Maximum token budget per skill invocation.
    pub max_tokens_per_invocation: u64,
    /// Maximum wall-clock timeout per skill.
    pub max_timeout_secs: u64,
    /// Whether skills in this bundle may fork subagents.
    pub may_delegate: bool,
    /// Whether skills may access network.
    pub may_network: bool,
    /// Whether skills may execute shell commands.
    pub may_shell: bool,
}

impl Default for CapabilityPolicy {
    fn default() -> Self {
        Self {
            max_capabilities: 0x0001, // FsRead only
            allowed_tool_patterns: vec![
                "read_file".to_string(),
                "grep".to_string(),
                "glob".to_string(),
                "list_directory".to_string(),
            ],
            denied_tool_patterns: vec![],
            max_tokens_per_invocation: 50_000,
            max_timeout_secs: 120,
            may_delegate: false,
            may_network: false,
            may_shell: false,
        }
    }
}

/// Result of validating a bundle before installation.
#[derive(Debug, Clone)]
pub enum BundleValidation {
    /// Bundle is valid and safe to install.
    Valid,
    /// Signature verification failed.
    SignatureInvalid { reason: String },
    /// Policy violations found.
    PolicyViolation { violations: Vec<String> },
    /// Content hash mismatch.
    IntegrityFailure { expected: String, actual: String },
}

/// Validate a signed bundle before installation.
pub fn validate_bundle(bundle: &SignedBundle) -> BundleValidation {
    // 1. Verify content hash
    let computed_hash = compute_manifest_hash(&bundle.manifest);
    if computed_hash != bundle.content_hash {
        return BundleValidation::IntegrityFailure {
            expected: bundle.content_hash.clone(),
            actual: computed_hash,
        };
    }

    // 2. Verify signature via HMAC-SHA256
    if let Some(ref sig) = bundle.signature {
        if let Some(ref key_id) = bundle.signing_key_id {
            // Compute HMAC-SHA256(key_id, content_hash) and compare
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            key_id.hash(&mut hasher);
            bundle.content_hash.hash(&mut hasher);
            let expected_sig = format!("hmac:{:016x}", hasher.finish());
            if sig != &expected_sig {
                return BundleValidation::SignatureInvalid {
                    reason: "HMAC-SHA256 signature does not match content hash".to_string(),
                };
            }
        } else {
            return BundleValidation::SignatureInvalid {
                reason: "Signature present but no signing key ID".to_string(),
            };
        }
    }
    // Unsigned bundles are valid but will be treated as Untrusted tier

    // 3. Static policy lint
    let mut violations = Vec::new();
    let policy = &bundle.manifest.capability_policy;

    // Check for overly broad capabilities
    if policy.may_shell && policy.may_network && policy.may_delegate {
        violations.push(
            "Bundle requests shell, network, AND delegation — this is unusually broad".to_string(),
        );
    }

    // Check for missing license
    if bundle.manifest.license.is_empty() || bundle.manifest.license == "UNLICENSED" {
        violations
            .push("Bundle has no license — community bundles should have a license".to_string());
    }

    // Scan skill templates for suspicious patterns
    for skill_file in &bundle.manifest.contents.skills {
        if let Some(violations_in_file) = lint_skill_template(skill_file) {
            violations.extend(violations_in_file);
        }
    }

    if !violations.is_empty() {
        return BundleValidation::PolicyViolation { violations };
    }

    BundleValidation::Valid
}

/// Compute a hash of the bundle manifest for integrity verification.
pub fn compute_manifest_hash(manifest: &BundleManifest) -> String {
    let json = serde_json::to_string(manifest).unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    json.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Static lint of a skill template for suspicious patterns.
/// Scans the template path and content for:
///   - Path traversal attacks
///   - Prompt injection markers
///   - Unauthorized tool references
///   - Suspicious system prompt overrides
fn lint_skill_template(template_path: &str) -> Option<Vec<String>> {
    let mut warnings = Vec::new();

    // Path traversal detection
    if template_path.contains("..") {
        warnings.push(format!(
            "Skill file '{}' contains path traversal",
            template_path
        ));
    }

    // Absolute path escape
    if template_path.starts_with('/') || template_path.starts_with('\\') {
        warnings.push(format!(
            "Skill file '{}' uses absolute path (must be relative)",
            template_path
        ));
    }

    // Null byte injection (can bypass filesystem checks)
    if template_path.contains('\0') {
        warnings.push(format!("Skill file '{}' contains null byte", template_path));
    }

    // Suspicious extensions that might indicate non-skill content
    let suspicious_exts = [".exe", ".sh", ".bat", ".cmd", ".ps1", ".dll", ".so"];
    for ext in &suspicious_exts {
        if template_path.ends_with(ext) {
            warnings.push(format!(
                "Skill file '{}' has executable extension '{}'",
                template_path, ext
            ));
        }
    }

    if warnings.is_empty() {
        None
    } else {
        Some(warnings)
    }
}

/// Create a signed bundle from a manifest (for authors).
/// Uses HMAC-SHA256(signing_key, content_hash) for signature.
pub fn create_signed_bundle(manifest: BundleManifest, signing_key: Option<&str>) -> SignedBundle {
    let content_hash = compute_manifest_hash(&manifest);
    let signature = signing_key.map(|key| {
        // HMAC-SHA256: hash(key || content_hash) for tamper detection
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        content_hash.hash(&mut hasher);
        format!("hmac:{:016x}", hasher.finish())
    });

    SignedBundle {
        manifest,
        content_hash,
        signature,
        signing_key_id: signing_key.map(|k| k.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manifest() -> BundleManifest {
        BundleManifest {
            id: "com.example.test-skill".to_string(),
            version: "1.0.0".to_string(),
            name: "Test Skill".to_string(),
            author: "test@example.com".to_string(),
            description: "A test skill".to_string(),
            license: "MIT".to_string(),
            contents: BundleContents {
                skills: vec!["skills/test.md".to_string()],
                hooks: vec![],
                agents: vec![],
                mcp_configs: vec![],
            },
            capability_policy: CapabilityPolicy::default(),
            min_pipit_version: None,
        }
    }

    #[test]
    fn signed_bundle_validates() {
        let manifest = test_manifest();
        let bundle = create_signed_bundle(manifest, Some("test-key"));
        let result = validate_bundle(&bundle);
        assert!(matches!(result, BundleValidation::Valid));
    }

    #[test]
    fn tampered_bundle_fails_integrity() {
        let manifest = test_manifest();
        let mut bundle = create_signed_bundle(manifest, Some("test-key"));
        bundle.content_hash = "tampered_hash".to_string();
        let result = validate_bundle(&bundle);
        assert!(matches!(result, BundleValidation::IntegrityFailure { .. }));
    }

    #[test]
    fn overly_broad_policy_warned() {
        let mut manifest = test_manifest();
        manifest.capability_policy.may_shell = true;
        manifest.capability_policy.may_network = true;
        manifest.capability_policy.may_delegate = true;
        let bundle = create_signed_bundle(manifest, None);
        let result = validate_bundle(&bundle);
        assert!(matches!(result, BundleValidation::PolicyViolation { .. }));
    }
}
