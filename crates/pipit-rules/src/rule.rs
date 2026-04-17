//! Task #1: Rule as a typed, tier-stamped claim.
//!
//! Every rule carries structured metadata compatible with pipit's tiered-provenance
//! model. A mandate is distinct from a preference at the type level.

use pipit_core::capability::CapabilitySet;
use pipit_core::proof::ImplementationTier;
use pipit_core::skill_activation::ActivationScope;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Content-addressed rule identity. Survives renames — only content changes
/// produce a new ID.
///
/// `RuleId = SHA-256(canonical_path + "\0" + content)`
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RuleId(pub String);

impl RuleId {
    /// Compute a content-addressed rule ID from canonical path and body.
    pub fn compute(canonical_path: &str, content: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(canonical_path.as_bytes());
        hasher.update(b"\0");
        hasher.update(content.as_bytes());
        let hash = hasher.finalize();
        // Hex-encode first 16 bytes (32 hex chars) for reasonable collision resistance.
        let hex: String = hash.iter().take(16).map(|b| format!("{b:02x}")).collect();
        RuleId(hex)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RuleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The behavioral kind of a rule. Determines enforcement semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RuleKind {
    /// Hard constraint — the verifier must confirm compliance. Cannot be
    /// overridden by the model without producing a proof failure.
    Mandate,
    /// Required action sequence (e.g. "run tests before commit").
    /// Compiles into `PlanConstraint::SequenceRequired`.
    Procedure,
    /// Soft guideline — the model may override with justification.
    Preference,
    /// Structural invariant (e.g. "no circular deps"). Hard constraint
    /// that compiles into static plan-gate checks.
    Invariant,
}

impl RuleKind {
    /// Whether this kind represents a hard constraint that the verifier
    /// ensemble must enforce.
    pub fn is_hard(&self) -> bool {
        matches!(self, Self::Mandate | Self::Invariant)
    }
}

/// Trust tier of the rule source. Determines which capability escalations
/// the rule may authorize (Task #12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RuleTrustTier {
    /// User-local rules (~/.config/pipit/rules/). Cannot grant capabilities.
    Local,
    /// Project rules (.pipit/rules/). May grant with user confirmation.
    Project,
    /// Team-synced rules (via VCS). May grant with user confirmation.
    Team,
    /// Enterprise-managed rules (signed). May grant without confirmation.
    Managed,
}

/// YAML frontmatter parsed from a rule file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuleFrontmatter {
    /// Human-readable rule description.
    #[serde(default)]
    pub description: Option<String>,
    /// Behavioral kind. Defaults to `Preference` if omitted.
    #[serde(default)]
    pub kind: Option<String>,
    /// Glob patterns for file paths that activate this rule.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Language patterns (e.g. "rust", "python").
    #[serde(default)]
    pub languages: Vec<String>,
    /// Capabilities this rule governs (e.g. "FsWrite", "ProcessExec").
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Implementation tier override (e.g. "Validated", "Heuristic").
    #[serde(default)]
    pub tier: Option<String>,
    /// Trust tier override (normally inferred from source directory).
    #[serde(default)]
    pub trust: Option<String>,
    /// Forbidden path patterns (for Mandate/Invariant rules).
    #[serde(default)]
    pub forbidden_paths: Vec<String>,
    /// Required tool sequence (for Procedure rules).
    #[serde(default)]
    pub required_sequence: Vec<String>,
    /// Scoped capability grants this rule declares (Task #12).
    #[serde(default)]
    pub grants: Vec<GrantDeclaration>,
}

/// A capability grant declared by a rule (Task #12).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantDeclaration {
    /// Tool or binary being granted.
    pub tool: String,
    /// Path scope (glob pattern).
    #[serde(default)]
    pub path_scope: Option<String>,
    /// Whether this is an auto-approve grant.
    #[serde(default)]
    pub auto_approve: bool,
}

/// A fully parsed, typed rule with all metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Content-addressed identity.
    pub id: RuleId,
    /// Human-readable name (derived from file path).
    pub name: String,
    /// Source file path.
    pub source_path: PathBuf,
    /// Canonical relative path (e.g. "common/security").
    pub canonical_path: String,
    /// Description from frontmatter.
    pub description: Option<String>,
    /// Behavioral kind.
    pub kind: RuleKind,
    /// Implementation tier — provenance confidence of this rule's assertion.
    pub tier: ImplementationTier,
    /// Trust tier — governs capability grant authorization.
    pub trust_tier: RuleTrustTier,
    /// Where this rule was found (determines activation precedence).
    pub scope: ActivationScope,
    /// Capabilities this rule governs. Only consulted when these capabilities
    /// are in scope for a pending tool call (Task #3).
    pub required_capabilities: CapabilitySet,
    /// Body content (the actual rule text after frontmatter stripping).
    pub body: String,
    /// SHA-256 of body content for cache keying.
    pub content_hash: String,
    /// Glob patterns for conditional activation (Task #2).
    pub path_patterns: Vec<String>,
    /// Language patterns for activation.
    pub language_patterns: Vec<String>,
    /// Forbidden path patterns (for Mandate/Invariant compilation to plan IR).
    pub forbidden_paths: Vec<String>,
    /// Required tool sequence (for Procedure compilation to plan IR).
    pub required_sequence: Vec<String>,
    /// Scoped capability grants (Task #12).
    pub grants: Vec<GrantDeclaration>,
}

impl Rule {
    /// Whether this rule should become a `VerificationStep` in proof packets (Task #5).
    pub fn is_verifiable(&self) -> bool {
        self.kind.is_hard() || matches!(self.kind, RuleKind::Procedure)
    }

    /// Produce a `VerificationStep` description for proof packet integration.
    pub fn as_verification_step(&self) -> String {
        match self.kind {
            RuleKind::Mandate => format!("[MANDATE] {}: {}", self.name, self.body_abstract()),
            RuleKind::Invariant => format!("[INVARIANT] {}: {}", self.name, self.body_abstract()),
            RuleKind::Procedure => format!("[PROCEDURE] {}: {}", self.name, self.body_abstract()),
            RuleKind::Preference => format!("[PREFERENCE] {}: {}", self.name, self.body_abstract()),
        }
    }

    /// First 200 chars of body as an abstract.
    pub fn body_abstract(&self) -> String {
        if self.body.len() <= 200 {
            self.body.clone()
        } else {
            let mut s = self.body[..200].to_string();
            s.push_str("…");
            s
        }
    }

    /// Whether this rule has conditional path-based activation.
    pub fn is_conditional(&self) -> bool {
        !self.path_patterns.is_empty() || !self.language_patterns.is_empty()
    }

    /// Whether this rule compiles into plan-gate constraints (Task #9).
    pub fn has_plan_constraints(&self) -> bool {
        !self.forbidden_paths.is_empty() || !self.required_sequence.is_empty()
    }

    /// Active rule IDs sorted for deterministic Merkle root computation (Task #8).
    pub fn sorted_active_ids(rules: &[&Rule]) -> Vec<RuleId> {
        let mut ids: BTreeSet<RuleId> = BTreeSet::new();
        for r in rules {
            ids.insert(r.id.clone());
        }
        ids.into_iter().collect()
    }
}
