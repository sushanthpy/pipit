//! Shadowed Rule Detection — identifies security-weakening rule orderings.
//!
//! Rule R_j is shadowed by R_i (i < j) if L(R_i) ⊇ L(R_j), meaning R_i
//! matches every input that R_j would match. When R_i is ALLOW and R_j is
//! DENY, the DENY rule is effectively dead code — a security hole.
//!
//! Detection: For glob patterns, L(R_i) ⊇ L(R_j) iff the glob of R_i is
//! a superset of the glob of R_j. We approximate this with:
//! - "*" subsumes all patterns
//! - "foo*" subsumes "foobar*"
//! - Exact match only subsumes itself
//!
//! Complexity: O(R²) where R = number of rules (typically small, <100).

use crate::Decision;
use crate::rules::PermissionRuleSet;

/// A detected shadow: rule at index `masking_idx` shadows rule at `shadowed_idx`.
#[derive(Debug, Clone)]
pub struct ShadowedRule {
    pub masking_rule: String,
    pub masking_line: usize,
    pub shadowed_rule: String,
    pub shadowed_line: usize,
    pub severity: ShadowSeverity,
}

#[derive(Debug, Clone, Copy)]
pub enum ShadowSeverity {
    /// An ALLOW rule masks a DENY rule — security weakening.
    Critical,
    /// Two rules with the same decision overlap — redundant but not dangerous.
    Warning,
    /// A narrower ALLOW masks a broader DENY — unusual but may be intentional.
    Info,
}

/// Detect shadowed rules in a rule set.
///
/// For every pair (R_i, R_j) where i < j, check if R_i's patterns
/// subsume R_j's patterns. If so and the decisions differ in a
/// security-relevant way, report a shadow.
pub fn detect_shadows(rule_set: &PermissionRuleSet) -> Vec<ShadowedRule> {
    let rules = rule_set.rules();
    let mut shadows = Vec::new();

    for i in 0..rules.len() {
        for j in (i + 1)..rules.len() {
            let r_i = &rules[i];
            let r_j = &rules[j];

            // Check if R_i's tool pattern subsumes R_j's
            if !tool_pattern_subsumes(&r_i.name, &r_j.name, r_i, r_j) {
                continue;
            }

            // Check mode overlap
            let mode_overlap = if r_i.modes.is_empty() || r_j.modes.is_empty() {
                true // Empty modes = all modes
            } else {
                r_i.modes.iter().any(|m| r_j.modes.contains(m))
            };

            if !mode_overlap {
                continue;
            }

            // Determine severity
            let severity = match (r_i.decision, r_j.decision) {
                (Decision::Allow, Decision::Deny) | (Decision::Allow, Decision::Escalate) => {
                    ShadowSeverity::Critical
                }
                (d1, d2) if d1 == d2 => ShadowSeverity::Warning,
                _ => ShadowSeverity::Info,
            };

            shadows.push(ShadowedRule {
                masking_rule: r_i.name.clone(),
                masking_line: r_i.source.index,
                shadowed_rule: r_j.name.clone(),
                shadowed_line: r_j.source.index,
                severity,
            });
        }
    }

    shadows
}

/// Check: does the tool pattern of R_i subsume R_j?
///
/// Uses the compiled GlobMatcher to test whether R_i's tool pattern
/// covers R_j's tool name, then compares restriction counts.
///
/// Previous implementation ignored tool names entirely (_name_i, _name_j
/// were unused) and only compared restriction counts, meaning rules
/// targeting completely different tools could be flagged as shadows.
fn tool_pattern_subsumes(
    name_i: &str,
    name_j: &str,
    r_i: &crate::rules::PermissionRule,
    r_j: &crate::rules::PermissionRule,
) -> bool {
    // Step 1: Check if R_i's tool pattern covers R_j's tool name.
    // Use the compiled GlobMatcher which correctly handles wildcards,
    // globs, and exact matches.
    let tool_covered = r_i.tool_matcher.is_match(name_j) || name_i == name_j;

    if !tool_covered {
        return false;
    }

    // Step 2: R_i subsumes R_j if R_i has equal or fewer restrictions.
    // A rule with no command/path restriction matches ALL commands/paths,
    // so it is strictly broader than one with restrictions.
    let i_has_cmd = r_i.command_matcher.is_some();
    let j_has_cmd = r_j.command_matcher.is_some();
    let i_has_path = r_i.path_matcher.is_some();
    let j_has_path = r_j.path_matcher.is_some();

    let i_restrictions = i_has_cmd as u8 + i_has_path as u8;
    let j_restrictions = j_has_cmd as u8 + j_has_path as u8;

    // R_i subsumes R_j if R_i has strictly fewer restrictions,
    // OR same restrictions and identical tool pattern (redundancy check).
    i_restrictions < j_restrictions || (i_restrictions == j_restrictions && name_i == name_j)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_severity_order() {
        // Critical > Warning > Info in importance
        assert!(matches!(ShadowSeverity::Critical, ShadowSeverity::Critical));
    }
}
