//! Task #12: Rule trust tier governs capability grant authorization.
//!
//! A rule at trust tier Managed can declare scoped-capability grants without
//! user approval; Project requires confirmation; Local cannot grant.

use crate::rule::{GrantDeclaration, Rule, RuleTrustTier};
use serde::{Deserialize, Serialize};

/// Result of evaluating a rule's capability grants against its trust tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GrantEvaluation {
    /// Grant is auto-approved (Managed tier).
    AutoApproved {
        rule_name: String,
        grant: GrantDeclaration,
    },
    /// Grant requires user confirmation (Project/Team tier).
    RequiresConfirmation {
        rule_name: String,
        grant: GrantDeclaration,
    },
    /// Grant is rejected (Local tier cannot grant).
    Rejected {
        rule_name: String,
        grant: GrantDeclaration,
        reason: String,
    },
}

/// Evaluate all grants declared by a rule against its trust tier.
pub fn evaluate_grants(rule: &Rule) -> Vec<GrantEvaluation> {
    rule.grants
        .iter()
        .map(|grant| match rule.trust_tier {
            RuleTrustTier::Managed => GrantEvaluation::AutoApproved {
                rule_name: rule.name.clone(),
                grant: grant.clone(),
            },
            RuleTrustTier::Project | RuleTrustTier::Team => {
                GrantEvaluation::RequiresConfirmation {
                    rule_name: rule.name.clone(),
                    grant: grant.clone(),
                }
            }
            RuleTrustTier::Local => GrantEvaluation::Rejected {
                rule_name: rule.name.clone(),
                grant: grant.clone(),
                reason: "Local rules cannot declare capability grants".to_string(),
            },
        })
        .collect()
}

/// Evaluate all grants from all active rules.
pub fn evaluate_all_grants(
    rules: &[&Rule],
) -> (Vec<GrantEvaluation>, Vec<GrantEvaluation>, Vec<GrantEvaluation>) {
    let mut auto = Vec::new();
    let mut confirm = Vec::new();
    let mut rejected = Vec::new();

    for rule in rules {
        for eval in evaluate_grants(rule) {
            match &eval {
                GrantEvaluation::AutoApproved { .. } => auto.push(eval),
                GrantEvaluation::RequiresConfirmation { .. } => confirm.push(eval),
                GrantEvaluation::Rejected { .. } => rejected.push(eval),
            }
        }
    }

    (auto, confirm, rejected)
}
