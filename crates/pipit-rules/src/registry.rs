//! Central rule registry. Stores typed rules indexed by RuleId,
//! supports capability-filtered queries, and provides the active rule
//! set for prompt assembly, proof packets, and plan gating.

use crate::rule::{Rule, RuleId, RuleKind};
use pipit_core::capability::CapabilitySet;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// The rule registry — ordered by RuleId for deterministic iteration.
#[derive(Debug, Clone, Default)]
pub struct RuleRegistry {
    /// All loaded rules, keyed by content-addressed ID.
    rules: BTreeMap<RuleId, Rule>,
    /// Subset of rules that are currently active (after conditional activation).
    active: BTreeMap<RuleId, ()>,
}

impl RuleRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a rule. If a rule with the same ID exists, it is replaced.
    pub fn register(&mut self, rule: Rule) {
        let id = rule.id.clone();
        self.rules.insert(id.clone(), rule);
        // Unconditional rules are immediately active.
        if !self.rules[&id].is_conditional() {
            self.active.insert(id, ());
        }
    }

    /// Activate a conditional rule (called after path-based activation check).
    pub fn activate(&mut self, id: &RuleId) -> bool {
        if self.rules.contains_key(id) {
            self.active.insert(id.clone(), ());
            true
        } else {
            false
        }
    }

    /// Deactivate a rule.
    pub fn deactivate(&mut self, id: &RuleId) {
        self.active.remove(id);
    }

    /// Get a rule by ID.
    pub fn get(&self, id: &RuleId) -> Option<&Rule> {
        self.rules.get(id)
    }

    /// All registered rules.
    pub fn all_rules(&self) -> impl Iterator<Item = &Rule> {
        self.rules.values()
    }

    /// Currently active rules.
    pub fn active_rules(&self) -> Vec<&Rule> {
        self.active
            .keys()
            .filter_map(|id| self.rules.get(id))
            .collect()
    }

    /// Active rules filtered by capability intersection (Task #3).
    /// Only returns rules whose governed capabilities intersect the tool's
    /// declared capabilities. O(A) scan with O(1) bit-AND per rule.
    pub fn rules_for_capabilities(&self, tool_caps: CapabilitySet) -> Vec<&Rule> {
        self.active_rules()
            .into_iter()
            .filter(|r| {
                // Rules with EMPTY capabilities are universal (always consulted).
                r.required_capabilities == CapabilitySet::EMPTY
                    || CapabilitySet::from_bits(
                        r.required_capabilities.bits() & tool_caps.bits(),
                    ) != CapabilitySet::EMPTY
            })
            .collect()
    }

    /// Active mandates and invariants (hard constraints).
    pub fn hard_constraints(&self) -> Vec<&Rule> {
        self.active_rules()
            .into_iter()
            .filter(|r| r.kind.is_hard())
            .collect()
    }

    /// Active rules that produce verification steps (Task #5).
    pub fn verifiable_rules(&self) -> Vec<&Rule> {
        self.active_rules()
            .into_iter()
            .filter(|r| r.is_verifiable())
            .collect()
    }

    /// Active rules that compile into plan-gate constraints (Task #9).
    pub fn plan_constraining_rules(&self) -> Vec<&Rule> {
        self.active_rules()
            .into_iter()
            .filter(|r| r.has_plan_constraints())
            .collect()
    }

    /// Conditional (not yet active) rules.
    pub fn conditional_rules(&self) -> Vec<&Rule> {
        self.rules
            .values()
            .filter(|r| r.is_conditional() && !self.active.contains_key(&r.id))
            .collect()
    }

    /// Total rule count.
    pub fn total_count(&self) -> usize {
        self.rules.len()
    }

    /// Active rule count.
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Merkle root of active rule set (Task #8, #14).
    /// Sorted RuleIds → SHA-256 tree → single root hash.
    /// Deterministic: same active set ⇒ same root.
    pub fn active_merkle_root(&self) -> String {
        let hashes: Vec<String> = self
            .active
            .keys()
            .map(|id| {
                self.rules
                    .get(id)
                    .map(|r| r.content_hash.clone())
                    .unwrap_or_default()
            })
            .collect();

        if hashes.is_empty() {
            return String::from("empty");
        }

        // Simple binary Merkle: iteratively hash pairs.
        let mut level: Vec<String> = hashes;
        while level.len() > 1 {
            let mut next = Vec::new();
            let mut i = 0;
            while i < level.len() {
                let left = &level[i];
                let right = if i + 1 < level.len() {
                    &level[i + 1]
                } else {
                    left // Duplicate last for odd count.
                };
                let mut hasher = Sha256::new();
                hasher.update(left.as_bytes());
                hasher.update(right.as_bytes());
                let h = hasher.finalize();
                next.push(h.iter().take(16).map(|b| format!("{b:02x}")).collect());
                i += 2;
            }
            level = next;
        }

        level.into_iter().next().unwrap_or_default()
    }

    /// Replace a rule atomically (for reactive watcher — Task #11).
    pub fn replace(&mut self, rule: Rule) {
        let was_active = self.active.contains_key(&rule.id);
        let id = rule.id.clone();
        self.rules.insert(id.clone(), rule);
        if was_active {
            self.active.insert(id, ());
        }
    }

    /// Remove a rule.
    pub fn remove(&mut self, id: &RuleId) -> Option<Rule> {
        self.active.remove(id);
        self.rules.remove(id)
    }

    /// Drain conditional rules into a separate collection (mirrors skill pattern).
    pub fn drain_conditional(&mut self) -> Vec<Rule> {
        let conditional_ids: Vec<RuleId> = self
            .rules
            .values()
            .filter(|r| r.is_conditional())
            .map(|r| r.id.clone())
            .collect();
        let mut drained = Vec::new();
        for id in &conditional_ids {
            self.active.remove(id);
            if let Some(rule) = self.rules.remove(id) {
                drained.push(rule);
            }
        }
        drained
    }

    /// Tier-weighted budget estimate for active rules (Task #7).
    /// Returns (mandates_chars, procedures_chars, preferences_chars).
    pub fn budget_estimate(&self) -> (usize, usize, usize) {
        let mut mandates = 0usize;
        let mut procedures = 0usize;
        let mut preferences = 0usize;
        for r in self.active_rules() {
            let size = r.body.len();
            match r.kind {
                RuleKind::Mandate | RuleKind::Invariant => mandates += size,
                RuleKind::Procedure => procedures += size,
                RuleKind::Preference => preferences += size,
            }
        }
        (mandates, procedures, preferences)
    }
}
