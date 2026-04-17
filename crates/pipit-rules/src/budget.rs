//! Task #7: Rule budget with adaptive context allocation.
//!
//! Renders rules within a token budget, using tier-aware priority:
//! mandates/invariants get highest priority, preferences lowest.

use crate::registry::RuleRegistry;
use crate::rule::RuleKind;

/// Tier weights for budget allocation.
const WEIGHT_MANDATE: f64 = 1.0;
const WEIGHT_INVARIANT: f64 = 1.0;
const WEIGHT_PROCEDURE: f64 = 0.6;
const WEIGHT_PREFERENCE: f64 = 0.3;

/// Budget render mode, determined by available space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// Full rule bodies injected.
    Full,
    /// Preferences and procedures truncated to abstracts; mandates/invariants full.
    Compact,
    /// Listing only: rule IDs and descriptions, no bodies.
    ListingOnly,
}

/// Render the active rule set within a character budget.
/// Returns (rendered_text, mode_used, rules_included, rules_truncated).
pub fn render_within_budget(
    registry: &RuleRegistry,
    budget_chars: usize,
) -> (String, RenderMode, usize, usize) {
    let active = registry.active_rules();
    if active.is_empty() {
        return (String::new(), RenderMode::Full, 0, 0);
    }

    let (mandates_size, procedures_size, preferences_size) = registry.budget_estimate();
    let total = mandates_size + procedures_size + preferences_size;

    // Try full mode first.
    if total <= budget_chars {
        let text = render_full(&active);
        return (text, RenderMode::Full, active.len(), 0);
    }

    // Try compact mode: full mandates/invariants, abstracts for rest.
    let compact_estimate = mandates_size + (procedures_size / 4) + (preferences_size / 8);
    if compact_estimate <= budget_chars {
        let (text, truncated) = render_compact(&active, budget_chars);
        return (text, RenderMode::Compact, active.len(), truncated);
    }

    // Fallback: listing only.
    let (text, included) = render_listing(&active, budget_chars);
    let truncated = active.len().saturating_sub(included);
    (text, RenderMode::ListingOnly, included, truncated)
}

fn render_full(rules: &[&crate::rule::Rule]) -> String {
    let mut out = String::new();
    for r in rules {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        let kind_tag = kind_tag(r.kind);
        out.push_str(&format!("### {kind_tag} Rule: {}\n", r.name));
        out.push_str(&r.body);
    }
    out
}

fn render_compact(rules: &[&crate::rule::Rule], budget: usize) -> (String, usize) {
    let mut out = String::new();
    let mut truncated = 0;

    // Sort: hard constraints first, then procedures, then preferences.
    let mut sorted: Vec<&&crate::rule::Rule> = rules.iter().collect();
    sorted.sort_by_key(|r| match r.kind {
        RuleKind::Mandate | RuleKind::Invariant => 0,
        RuleKind::Procedure => 1,
        RuleKind::Preference => 2,
    });

    for r in sorted {
        let section = if r.kind.is_hard() {
            let kind_tag = kind_tag(r.kind);
            format!("### {kind_tag} Rule: {}\n{}", r.name, r.body)
        } else {
            truncated += 1;
            let kind_tag = kind_tag(r.kind);
            format!("### {kind_tag} Rule: {}\n{}", r.name, r.body_abstract())
        };

        if out.len() + section.len() + 2 > budget {
            truncated += 1;
            continue;
        }

        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&section);
    }

    (out, truncated)
}

fn render_listing(rules: &[&crate::rule::Rule], budget: usize) -> (String, usize) {
    let mut out = String::from("Active rules (use `/rule show <name>` for details):\n");
    let mut included = 0;

    // Sort by kind priority.
    let mut sorted: Vec<&&crate::rule::Rule> = rules.iter().collect();
    sorted.sort_by_key(|r| match r.kind {
        RuleKind::Mandate | RuleKind::Invariant => 0,
        RuleKind::Procedure => 1,
        RuleKind::Preference => 2,
    });

    for r in sorted {
        let kind_tag = kind_tag(r.kind);
        let desc = r
            .description
            .as_deref()
            .unwrap_or("[no description]");
        let line = format!("- [{kind_tag}] **{}**: {}\n", r.name, desc);
        if out.len() + line.len() > budget {
            break;
        }
        out.push_str(&line);
        included += 1;
    }

    (out, included)
}

fn kind_tag(kind: RuleKind) -> &'static str {
    match kind {
        RuleKind::Mandate => "MANDATE",
        RuleKind::Invariant => "INVARIANT",
        RuleKind::Procedure => "PROCEDURE",
        RuleKind::Preference => "PREFERENCE",
    }
}

/// Compute the recommended budget slice for rules given the model's
/// context window size. Returns character count.
///
/// Formula: `(context_window / 100) * 4` chars (≈1% of tokens × 4 chars/token),
/// minimum 800 chars.
pub fn recommended_budget(context_window_tokens: u64) -> usize {
    let budget = ((context_window_tokens / 100) * 4) as usize;
    budget.max(800)
}

/// Compute per-tier budget weights for a given rule set.
pub fn tier_weights(registry: &RuleRegistry) -> Vec<(RuleKind, f64)> {
    let active = registry.active_rules();
    let mut weights = Vec::new();
    for r in &active {
        let w = match r.kind {
            RuleKind::Mandate => WEIGHT_MANDATE,
            RuleKind::Invariant => WEIGHT_INVARIANT,
            RuleKind::Procedure => WEIGHT_PROCEDURE,
            RuleKind::Preference => WEIGHT_PREFERENCE,
        };
        weights.push((r.kind, w));
    }
    weights
}
