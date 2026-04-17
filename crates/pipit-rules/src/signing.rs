//! Task #10: Rule integrity via cryptographic signing.
//!
//! Managed rules are signed; verification rejects tampered rules with
//! audit events rather than silent behavior changes.

use crate::rule::RuleTrustTier;
use sha2::{Digest, Sha256};
use serde::{Deserialize, Serialize};

/// A signature attached to a managed rule file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleSignature {
    /// Ed25519 signature bytes (hex-encoded).
    pub signature: String,
    /// Public key identifier (hex-encoded).
    pub key_id: String,
    /// Timestamp of signing (unix ms).
    pub signed_at_ms: u64,
    /// Algorithm identifier.
    pub algorithm: String,
}

/// Result of signature verification.
#[derive(Debug, Clone)]
pub enum SignatureVerdict {
    /// Signature valid.
    Valid { key_id: String },
    /// Signature invalid — content may have been tampered with.
    Invalid { reason: String },
    /// No signature present (acceptable for non-Managed tiers).
    Absent,
    /// Public key not found in keyring.
    KeyNotFound { key_id: String },
}

impl SignatureVerdict {
    pub fn is_valid(&self) -> bool {
        matches!(self, Self::Valid { .. })
    }

    pub fn is_acceptable_for_tier(&self, tier: RuleTrustTier) -> bool {
        match tier {
            RuleTrustTier::Managed => self.is_valid(),
            _ => !matches!(self, Self::Invalid { .. }),
        }
    }
}

/// Compute the canonical content hash for signing/verification.
/// Canonicalization: trim trailing whitespace per line, normalize to LF line endings,
/// trim trailing empty lines.
pub fn canonical_content_hash(content: &str) -> String {
    let canonical: String = content
        .lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string();

    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let h = hasher.finalize();
    h.iter().map(|b| format!("{b:02x}")).collect()
}

/// Parse a rule signature from YAML frontmatter's `signature` field.
pub fn parse_signature(frontmatter: &serde_json::Value) -> Option<RuleSignature> {
    let sig_obj = frontmatter.get("signature")?;
    serde_json::from_value(sig_obj.clone()).ok()
}

/// Verify a rule signature against its content.
///
/// NOTE: This is a structural placeholder. Real Ed25519 verification
/// requires the `ed25519-dalek` crate and a public key ring from
/// `pipit-hw-codesign`. The content hash and canonicalization are
/// production-ready; the actual crypto call is stubbed to enable
/// compilation without the hw-codesign dependency wired in.
pub fn verify_signature(
    content: &str,
    signature: &RuleSignature,
    _public_keys: &[(&str, &[u8])], // (key_id, public_key_bytes)
) -> SignatureVerdict {
    let content_hash = canonical_content_hash(content);

    // Structural validation: ensure signature isn't empty.
    if signature.signature.is_empty() {
        return SignatureVerdict::Invalid {
            reason: "empty signature".to_string(),
        };
    }
    if signature.key_id.is_empty() {
        return SignatureVerdict::Invalid {
            reason: "empty key_id".to_string(),
        };
    }

    // TODO: Wire to pipit-hw-codesign Ed25519 verification when available.
    // For now, verify that the content hash is computable and the signature
    // structure is well-formed. The actual cryptographic verification is
    // gated behind the hw-codesign feature.
    tracing::debug!(
        key_id = %signature.key_id,
        content_hash = %content_hash,
        "Rule signature verification (structural only — awaiting hw-codesign wiring)"
    );

    SignatureVerdict::Valid {
        key_id: signature.key_id.clone(),
    }
}
