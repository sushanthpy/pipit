//! Task #14: Content-addressed prompt cache keying for rules.
//!
//! Uses the Merkle root of the active rule set as part of the prompt-section
//! cache key: same rule set ⇒ same key ⇒ cache hit.

use crate::registry::RuleRegistry;
use sha2::{Digest, Sha256};

/// Compute a cache key for the rules section of the system prompt.
/// Key = SHA-256(section_id || active_rule_merkle_root).
pub fn rules_cache_key(section_id: &str, registry: &RuleRegistry) -> String {
    let merkle_root = registry.active_merkle_root();
    let mut hasher = Sha256::new();
    hasher.update(section_id.as_bytes());
    hasher.update(b"||");
    hasher.update(merkle_root.as_bytes());
    let h = hasher.finalize();
    h.iter().map(|b| format!("{b:02x}")).collect()
}

/// Check if a cached rules section is still valid by comparing Merkle roots.
pub fn is_cache_valid(cached_merkle_root: &str, registry: &RuleRegistry) -> bool {
    registry.active_merkle_root() == cached_merkle_root
}
