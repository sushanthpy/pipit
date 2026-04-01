//! Skill Budgeting, Bundling, and Compression Policy (Skill Task 7)
//!
//! Budget-aware skill inclusion: when context is tight, skills are
//! progressively compressed from full description → truncated → name-only.
//! Uses a knapsack-like allocation where value = trigger relevance × prior
//! utility, and weight = description token cost.

use serde::{Deserialize, Serialize};

/// A skill candidate for budget allocation.
#[derive(Debug, Clone)]
pub struct SkillBudgetCandidate {
    /// Skill identifier.
    pub skill_id: String,
    /// Full skill prompt text.
    pub full_text: String,
    /// Truncated version (first paragraph + key points).
    pub truncated_text: String,
    /// Name-only version (just the skill name and one-line description).
    pub name_only: String,
    /// Estimated tokens for full text.
    pub full_tokens: u64,
    /// Estimated tokens for truncated text.
    pub truncated_tokens: u64,
    /// Estimated tokens for name-only.
    pub name_tokens: u64,
    /// Relevance score (0.0–1.0) from trigger matching.
    pub relevance: f64,
    /// Prior utility score (0.0–1.0) from historical invocation success.
    pub prior_utility: f64,
}

impl SkillBudgetCandidate {
    /// Composite value score: relevance × (1 + prior_utility).
    pub fn value(&self) -> f64 {
        self.relevance * (1.0 + self.prior_utility)
    }

    /// Value-to-cost ratio for greedy knapsack.
    pub fn efficiency(&self, level: InclusionLevel) -> f64 {
        let cost = match level {
            InclusionLevel::Full => self.full_tokens,
            InclusionLevel::Truncated => self.truncated_tokens,
            InclusionLevel::NameOnly => self.name_tokens,
            InclusionLevel::Excluded => return 0.0,
        };
        if cost == 0 {
            return 0.0;
        }
        self.value() / cost as f64
    }
}

/// How a skill is included in the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InclusionLevel {
    /// Full skill text included.
    Full,
    /// Truncated to key points.
    Truncated,
    /// Only name and one-line description.
    NameOnly,
    /// Not included at all.
    Excluded,
}

/// The output of skill budget allocation.
#[derive(Debug, Clone)]
pub struct SkillBudgetAllocation {
    /// Skills and their inclusion levels.
    pub allocations: Vec<(String, InclusionLevel)>,
    /// Total tokens consumed by skill inclusions.
    pub total_tokens: u64,
    /// Budget remaining after allocation.
    pub budget_remaining: u64,
    /// Number of skills at each level.
    pub counts: AllocationCounts,
}

#[derive(Debug, Clone, Default)]
pub struct AllocationCounts {
    pub full: usize,
    pub truncated: usize,
    pub name_only: usize,
    pub excluded: usize,
}

/// Allocate skill-prompt budget using a greedy knapsack approach.
///
/// Algorithm:
/// 1. Sort candidates by value/cost ratio (efficiency)
/// 2. Greedily include at the highest affordable level
/// 3. When budget is tight, downgrade to truncated → name-only → excluded
///
/// Complexity: O(s log s) for s skills.
pub fn allocate_skill_budget(
    candidates: &[SkillBudgetCandidate],
    total_budget: u64,
) -> SkillBudgetAllocation {
    if candidates.is_empty() {
        return SkillBudgetAllocation {
            allocations: vec![],
            total_tokens: 0,
            budget_remaining: total_budget,
            counts: AllocationCounts::default(),
        };
    }

    // Sort by efficiency (value/cost) at full level, descending
    let mut indexed: Vec<(usize, f64)> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| (i, c.efficiency(InclusionLevel::Full)))
        .collect();
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut remaining = total_budget;
    let mut allocations = Vec::with_capacity(candidates.len());
    let mut counts = AllocationCounts::default();

    for (idx, _) in &indexed {
        let candidate = &candidates[*idx];

        // Try full inclusion first
        let level = if candidate.full_tokens <= remaining {
            remaining -= candidate.full_tokens;
            counts.full += 1;
            InclusionLevel::Full
        } else if candidate.truncated_tokens <= remaining {
            remaining -= candidate.truncated_tokens;
            counts.truncated += 1;
            InclusionLevel::Truncated
        } else if candidate.name_tokens <= remaining {
            remaining -= candidate.name_tokens;
            counts.name_only += 1;
            InclusionLevel::NameOnly
        } else {
            counts.excluded += 1;
            InclusionLevel::Excluded
        };

        allocations.push((candidate.skill_id.clone(), level));
    }

    // Sort allocations to match original order
    allocations.sort_by_key(|(id, _)| {
        candidates
            .iter()
            .position(|c| c.skill_id == *id)
            .unwrap_or(usize::MAX)
    });

    SkillBudgetAllocation {
        allocations,
        total_tokens: total_budget - remaining,
        budget_remaining: remaining,
        counts,
    }
}

/// Generate the prompt text for a skill at a given inclusion level.
pub fn render_skill_for_prompt(candidate: &SkillBudgetCandidate, level: InclusionLevel) -> String {
    match level {
        InclusionLevel::Full => candidate.full_text.clone(),
        InclusionLevel::Truncated => candidate.truncated_text.clone(),
        InclusionLevel::NameOnly => candidate.name_only.clone(),
        InclusionLevel::Excluded => String::new(),
    }
}

/// Estimate token count for text using the standard heuristic.
pub fn estimate_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    let len = text.len();
    let punct = text.bytes().filter(|b| b.is_ascii_punctuation()).count();
    let ratio = punct as f64 / len as f64;
    let divisor = if ratio > 0.15 { 3.0 } else { 4.0 };
    (len as f64 / divisor) as u64
}

/// Create truncated and name-only versions of a skill text.
pub fn create_budget_variants(
    skill_id: &str,
    name: &str,
    full_text: &str,
    relevance: f64,
) -> SkillBudgetCandidate {
    // Truncated: first paragraph + section headers
    let truncated = truncate_to_key_points(full_text, 500);
    let name_only = format!("- {}: {}", skill_id, first_sentence(full_text));

    SkillBudgetCandidate {
        skill_id: skill_id.to_string(),
        full_text: full_text.to_string(),
        truncated_text: truncated.clone(),
        name_only: name_only.clone(),
        full_tokens: estimate_tokens(full_text),
        truncated_tokens: estimate_tokens(&truncated),
        name_tokens: estimate_tokens(&name_only),
        relevance,
        prior_utility: 0.0,
    }
}

fn truncate_to_key_points(text: &str, max_chars: usize) -> String {
    let mut result = String::new();
    let mut chars_used = 0;

    for line in text.lines() {
        if chars_used + line.len() > max_chars {
            break;
        }
        result.push_str(line);
        result.push('\n');
        chars_used += line.len();

        // Always include headers
        if chars_used >= max_chars && !line.starts_with('#') {
            break;
        }
    }

    if chars_used >= max_chars {
        result.push_str("\n[...truncated for budget]\n");
    }
    result
}

fn first_sentence(text: &str) -> &str {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let line = line.trim_start_matches('#').trim();
    match line.find(|c: char| c == '.' || c == '\n') {
        Some(pos) => &line[..=pos],
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_allocation_respects_limit() {
        let candidates = vec![
            SkillBudgetCandidate {
                skill_id: "a".into(),
                full_text: "x".repeat(400),
                truncated_text: "x".repeat(100),
                name_only: "a: desc".into(),
                full_tokens: 100,
                truncated_tokens: 25,
                name_tokens: 3,
                relevance: 0.9,
                prior_utility: 0.5,
            },
            SkillBudgetCandidate {
                skill_id: "b".into(),
                full_text: "y".repeat(400),
                truncated_text: "y".repeat(100),
                name_only: "b: desc".into(),
                full_tokens: 100,
                truncated_tokens: 25,
                name_tokens: 3,
                relevance: 0.5,
                prior_utility: 0.2,
            },
        ];

        // Budget for one full + nothing else
        let alloc = allocate_skill_budget(&candidates, 105);
        assert!(alloc.total_tokens <= 105);
        assert_eq!(alloc.counts.full, 1); // Most valuable gets full
        // The other gets truncated or name-only
        assert!(alloc.counts.truncated + alloc.counts.name_only >= 1);
    }

    #[test]
    fn zero_budget_excludes_all() {
        let candidates = vec![SkillBudgetCandidate {
            skill_id: "a".into(),
            full_text: "text".into(),
            truncated_text: "t".into(),
            name_only: "a".into(),
            full_tokens: 10,
            truncated_tokens: 5,
            name_tokens: 2,
            relevance: 1.0,
            prior_utility: 0.0,
        }];

        let alloc = allocate_skill_budget(&candidates, 0);
        assert_eq!(alloc.counts.excluded, 1);
    }
}
