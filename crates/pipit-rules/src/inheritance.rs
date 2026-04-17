//! Task #6: Rule inheritance through the lineage DAG.
//!
//! Subagent execution branches inherit rules via set intersection
//! (lattice meet), matching capability inheritance semantics.
//! A child may narrow (disable some rules) but never broaden.

use crate::rule::RuleId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The rule set inherited by an execution branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InheritedRuleSet {
    /// Active rule IDs for this branch.
    pub active_rules: BTreeSet<RuleId>,
    /// Rules explicitly disabled at this branch boundary.
    pub disabled_rules: Vec<DisabledRule>,
}

/// Record of a rule disabled at a branch boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisabledRule {
    pub rule_id: RuleId,
    pub reason: String,
}

impl InheritedRuleSet {
    /// Create the root rule set from a registry's active rules.
    pub fn from_active(active_ids: impl IntoIterator<Item = RuleId>) -> Self {
        Self {
            active_rules: active_ids.into_iter().collect(),
            disabled_rules: Vec::new(),
        }
    }

    /// Compute the child rule set via lattice meet (intersection).
    /// `child_permitted` is the set of rules the child is allowed to have.
    /// Any rule in the parent but not in the child's permitted set is recorded
    /// as disabled with the given justification.
    pub fn narrow_for_child(
        &self,
        child_permitted: &BTreeSet<RuleId>,
        justification: &str,
    ) -> Self {
        let active: BTreeSet<RuleId> = self
            .active_rules
            .intersection(child_permitted)
            .cloned()
            .collect();

        let disabled: Vec<DisabledRule> = self
            .active_rules
            .difference(&active)
            .map(|id| DisabledRule {
                rule_id: id.clone(),
                reason: justification.to_string(),
            })
            .collect();

        Self {
            active_rules: active,
            disabled_rules: disabled,
        }
    }

    /// Inherit all parent rules (no narrowing).
    pub fn inherit_all(&self) -> Self {
        Self {
            active_rules: self.active_rules.clone(),
            disabled_rules: Vec::new(),
        }
    }

    /// Check if broadening is attempted (child has rules parent doesn't).
    /// Returns the offending rule IDs.
    pub fn detect_broadening(&self, child_requested: &BTreeSet<RuleId>) -> Vec<RuleId> {
        child_requested
            .difference(&self.active_rules)
            .cloned()
            .collect()
    }

    /// Number of active rules.
    pub fn count(&self) -> usize {
        self.active_rules.len()
    }
}
