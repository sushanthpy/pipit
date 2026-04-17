//! Task #13: Rule conflict detection and resolution.
//!
//! When two rules with overlapping capabilities and activation produce
//! contradictory guidance, detect the conflict at load time and cache
//! the resolution.

use crate::rule::{Rule, RuleId, RuleKind};
use pipit_core::capability::CapabilitySet;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A detected conflict between two rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleConflict {
    pub rule_a: RuleId,
    pub rule_b: RuleId,
    pub capability_overlap: u32, // CapabilitySet bits
    pub detail: String,
}

/// Resolution of a rule conflict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictResolution {
    pub conflict: RuleConflict,
    /// The winning rule.
    pub winner: RuleId,
    /// Reason for the resolution.
    pub reasoning: String,
    /// Confidence in the resolution.
    pub confidence: f32,
}

/// Cache of resolved conflicts.
/// Key = (rule_id_a, rule_id_b) where a < b lexicographically.
#[derive(Debug, Clone, Default)]
pub struct ConflictCache {
    resolutions: HashMap<(RuleId, RuleId), ConflictResolution>,
}

impl ConflictCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store a resolution.
    pub fn store(&mut self, resolution: ConflictResolution) {
        let key = canonical_key(
            &resolution.conflict.rule_a,
            &resolution.conflict.rule_b,
        );
        self.resolutions.insert(key, resolution);
    }

    /// Look up a cached resolution.
    pub fn lookup(&self, rule_a: &RuleId, rule_b: &RuleId) -> Option<&ConflictResolution> {
        let key = canonical_key(rule_a, rule_b);
        self.resolutions.get(&key)
    }

    /// Invalidate resolutions involving a specific rule (e.g., after rule content change).
    pub fn invalidate_rule(&mut self, rule_id: &RuleId) {
        self.resolutions.retain(|k, _| k.0 != *rule_id && k.1 != *rule_id);
    }

    /// All cached resolutions.
    pub fn all_resolutions(&self) -> impl Iterator<Item = &ConflictResolution> {
        self.resolutions.values()
    }
}

fn canonical_key(a: &RuleId, b: &RuleId) -> (RuleId, RuleId) {
    if a <= b {
        (a.clone(), b.clone())
    } else {
        (b.clone(), a.clone())
    }
}

/// Detect potential conflicts among active rules.
///
/// Two rules conflict if they:
/// 1. Have overlapping capability sets
/// 2. Are both active for the same activation scope
/// 3. Have different kinds where one is hard and the other soft on
///    the same topic (heuristic: same capability + different kind)
///
/// O(R²) naive, but R is typically small per capability bucket.
pub fn detect_conflicts(rules: &[&Rule]) -> Vec<RuleConflict> {
    let mut conflicts = Vec::new();

    for (i, a) in rules.iter().enumerate() {
        for b in rules.iter().skip(i + 1) {
            // Check capability overlap.
            let overlap = a.required_capabilities.bits() & b.required_capabilities.bits();
            if overlap == 0
                && a.required_capabilities != CapabilitySet::EMPTY
                && b.required_capabilities != CapabilitySet::EMPTY
            {
                continue; // No capability overlap — can't conflict.
            }

            // Heuristic: different kinds on overlapping capabilities suggest tension.
            if a.kind != b.kind && (a.kind.is_hard() || b.kind.is_hard()) {
                conflicts.push(RuleConflict {
                    rule_a: a.id.clone(),
                    rule_b: b.id.clone(),
                    capability_overlap: overlap,
                    detail: format!(
                        "'{name_a}' ({kind_a:?}) vs '{name_b}' ({kind_b:?}) on overlapping capabilities",
                        name_a = a.name,
                        kind_a = a.kind,
                        name_b = b.name,
                        kind_b = b.kind,
                    ),
                });
            }
        }
    }

    conflicts
}

/// Auto-resolve a conflict using scope precedence.
/// Higher scope wins. If equal, hard constraint wins.
pub fn auto_resolve(conflict: &RuleConflict, rule_a: &Rule, rule_b: &Rule) -> ConflictResolution {
    let (winner, reasoning) = if rule_a.scope.precedence() > rule_b.scope.precedence() {
        (
            rule_a.id.clone(),
            format!(
                "'{}' wins: higher scope precedence ({:?} > {:?})",
                rule_a.name, rule_a.scope, rule_b.scope
            ),
        )
    } else if rule_b.scope.precedence() > rule_a.scope.precedence() {
        (
            rule_b.id.clone(),
            format!(
                "'{}' wins: higher scope precedence ({:?} > {:?})",
                rule_b.name, rule_b.scope, rule_a.scope
            ),
        )
    } else if rule_a.kind.is_hard() && !rule_b.kind.is_hard() {
        (
            rule_a.id.clone(),
            format!("'{}' wins: hard constraint overrides soft", rule_a.name),
        )
    } else if rule_b.kind.is_hard() && !rule_a.kind.is_hard() {
        (
            rule_b.id.clone(),
            format!("'{}' wins: hard constraint overrides soft", rule_b.name),
        )
    } else {
        // Same scope, same hardness — pick lexicographically first for stability.
        let winner = if rule_a.id <= rule_b.id {
            rule_a.id.clone()
        } else {
            rule_b.id.clone()
        };
        (
            winner,
            "Tie-broken by lexicographic rule ID order".to_string(),
        )
    };

    ConflictResolution {
        conflict: conflict.clone(),
        winner,
        reasoning,
        confidence: 0.7, // Auto-resolution gets lower confidence than deliberation.
    }
}
