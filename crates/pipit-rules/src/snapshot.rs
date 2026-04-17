//! Task #8: Rules in causal snapshot assembly.
//!
//! `RulesSnapshot` with source watermarks — rule state becomes a first-class
//! entry in `CausalSnapshot`.

use crate::registry::RuleRegistry;
use crate::rule::{RuleId, RuleKind, RuleTrustTier};
use pipit_core::causal_snapshot::SourceWatermark;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Snapshot of the active rule set at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RulesSnapshot {
    /// IDs of all active rules at snapshot time.
    pub active_rule_ids: Vec<RuleId>,
    /// Merkle root of the active rule set (one comparison suffices for equality).
    pub content_root: String,
    /// Count of rules per kind.
    pub kind_counts: HashMap<String, usize>,
    /// Count of rules per trust tier.
    pub trust_counts: HashMap<String, usize>,
    /// Source watermarks for each rule directory.
    pub watermarks: Vec<SourceWatermark>,
    /// Whether all rule sources were available.
    pub fully_available: bool,
}

impl RulesSnapshot {
    /// Assemble a snapshot from the current registry state.
    pub fn assemble(
        registry: &RuleRegistry,
        source_watermarks: Vec<SourceWatermark>,
    ) -> Self {
        let active = registry.active_rules();
        let active_rule_ids: Vec<RuleId> = active.iter().map(|r| r.id.clone()).collect();
        let content_root = registry.active_merkle_root();

        let mut kind_counts: HashMap<String, usize> = HashMap::new();
        let mut trust_counts: HashMap<String, usize> = HashMap::new();

        for r in &active {
            let kind_str = match r.kind {
                RuleKind::Mandate => "Mandate",
                RuleKind::Invariant => "Invariant",
                RuleKind::Procedure => "Procedure",
                RuleKind::Preference => "Preference",
            };
            *kind_counts.entry(kind_str.to_string()).or_default() += 1;

            let trust_str = match r.trust_tier {
                RuleTrustTier::Local => "Local",
                RuleTrustTier::Project => "Project",
                RuleTrustTier::Team => "Team",
                RuleTrustTier::Managed => "Managed",
            };
            *trust_counts.entry(trust_str.to_string()).or_default() += 1;
        }

        let fully_available = source_watermarks.iter().all(|w| w.available);

        Self {
            active_rule_ids,
            content_root,
            kind_counts,
            trust_counts,
            watermarks: source_watermarks,
            fully_available,
        }
    }

    /// Check if two snapshots have the same active rule set.
    pub fn same_rules(&self, other: &RulesSnapshot) -> bool {
        self.content_root == other.content_root
    }
}
