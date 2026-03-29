//! Cross-Project Knowledge Injection — Task 6.2
//!
//! Pre-task knowledge injection phase: queries the longitudinal knowledge
//! store and injects relevant past experiences into the context window.
//!
//! Allocation: system prompt (fixed) + knowledge (K tokens) + repo map + history.
//! Selection: score(u) = cos_sim(q, embed(u)) · e^{-λ·age_days(u)}
//! where λ = 0.001 (half-life ≈ 693 days — knowledge decays slowly).

use serde::{Deserialize, Serialize};

/// Default knowledge injection budget in tokens.
pub const DEFAULT_KNOWLEDGE_BUDGET_TOKENS: u64 = 2048;

/// Recency decay for knowledge: λ = 0.001 → half-life ≈ 693 days.
const KNOWLEDGE_DECAY_LAMBDA: f64 = 0.001;

/// A knowledge unit selected for injection into the context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InjectedKnowledge {
    pub concept: String,
    pub approach: String,
    pub outcome: String,
    pub source_project: String,
    pub relevance_score: f64,
    pub estimated_tokens: u64,
}

/// Format knowledge units for injection into the system prompt.
pub fn format_knowledge_preamble(units: &[InjectedKnowledge], budget_tokens: u64) -> String {
    if units.is_empty() {
        return String::new();
    }

    let mut preamble = String::from("\n## Relevant Past Experience\n\n");
    preamble.push_str("The following solutions from previous tasks may be relevant:\n\n");

    let mut used_tokens: u64 = estimate_tokens(&preamble);

    for unit in units {
        let entry = format!(
            "**{}** (from project `{}`)\n- Approach: {}\n- Outcome: {}\n\n",
            unit.concept, unit.source_project, unit.approach, unit.outcome
        );
        let entry_tokens = estimate_tokens(&entry);

        if used_tokens + entry_tokens > budget_tokens {
            break;
        }

        preamble.push_str(&entry);
        used_tokens += entry_tokens;
    }

    preamble
}

/// Score a knowledge unit for relevance: cosine_similarity × recency_decay.
pub fn score_knowledge_unit(cosine_similarity: f64, age_days: f64) -> f64 {
    let recency = (-KNOWLEDGE_DECAY_LAMBDA * age_days).exp();
    cosine_similarity * recency
}

/// Rough token estimate (~4 chars per token).
fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64) / 4
}

/// Select top-k knowledge units from scored candidates within budget.
/// O(N log k) via sorting.
pub fn select_knowledge_units(
    candidates: Vec<(InjectedKnowledge, f64)>, // (unit, combined_score)
    budget_tokens: u64,
) -> Vec<InjectedKnowledge> {
    let mut scored: Vec<_> = candidates;
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut selected = Vec::new();
    let mut used = 0u64;

    for (unit, _score) in scored {
        if used + unit.estimated_tokens > budget_tokens {
            continue; // Skip this one, try smaller units
        }
        used += unit.estimated_tokens;
        selected.push(unit);
    }

    selected
}

/// Extract knowledge units from a completed task conversation.
///
/// Scans for knowledge anchors: tool calls that produced results followed
/// by assistant messages summarizing the outcome. Pattern:
/// [ToolCall → ToolResult{success} → Text{summary}]
///
/// Returns 3-10 knowledge units per conversation.
pub fn extract_knowledge_units(
    messages: &[serde_json::Value],
    project: &str,
    task_id: &str,
) -> Vec<InjectedKnowledge> {
    let mut units = Vec::new();

    // Scan for patterns: tool execution followed by assistant summary
    let mut i = 0;
    while i < messages.len() {
        let msg = &messages[i];
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");

        if role == "assistant" {
            // Check if this assistant message contains tool calls followed by a summary
            let content = msg.get("content")
                .and_then(|c| {
                    if let Some(s) = c.as_str() {
                        Some(s.to_string())
                    } else if let Some(arr) = c.as_array() {
                        // Content blocks — extract text
                        let texts: Vec<&str> = arr.iter()
                            .filter_map(|block| {
                                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                                    block.get("text").and_then(|t| t.as_str())
                                } else {
                                    None
                                }
                            })
                            .collect();
                        if texts.is_empty() { None } else { Some(texts.join("\n")) }
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

            // Look for knowledge indicators in assistant text
            if content.len() > 100 && contains_knowledge_signal(&content) {
                let concept = extract_concept(&content);
                let approach = extract_approach(&content);
                let outcome = extract_outcome(&content);

                if !concept.is_empty() && !approach.is_empty() {
                    let tokens = estimate_tokens(&format!("{} {} {}", concept, approach, outcome));
                    units.push(InjectedKnowledge {
                        concept,
                        approach,
                        outcome,
                        source_project: project.to_string(),
                        relevance_score: 0.0, // Will be scored later via embedding
                        estimated_tokens: tokens,
                    });
                }
            }
        }
        i += 1;
    }

    // Cap at 10 units per conversation
    units.truncate(10);
    units
}

/// Check if text contains signals of extractable knowledge.
fn contains_knowledge_signal(text: &str) -> bool {
    let lower = text.to_lowercase();
    let signals = [
        "fixed", "solved", "the issue was", "the bug was", "root cause",
        "the solution", "i found", "the problem", "this works because",
        "the fix is", "resolved by", "approach:", "technique:",
        "pattern:", "best practice", "lesson learned",
        "key insight", "important to note", "the trick is",
    ];
    signals.iter().any(|s| lower.contains(s))
}

/// Extract a concept name from the text (first sentence or key phrase).
fn extract_concept(text: &str) -> String {
    // Take the first meaningful sentence
    for sentence in text.split(['.', '\n']) {
        let trimmed = sentence.trim();
        if trimmed.len() > 20 && trimmed.len() < 200 {
            return trimmed.to_string();
        }
    }
    text.chars().take(150).collect::<String>().trim().to_string()
}

/// Extract the approach/method from the text.
fn extract_approach(text: &str) -> String {
    let lower = text.to_lowercase();
    // Look for "fixed by", "solved by", "the fix is", etc.
    for marker in &["fixed by ", "solved by ", "the fix is ", "the solution is ",
                     "resolved by ", "approach: ", "i changed ", "updated "] {
        if let Some(pos) = lower.find(marker) {
            let start = pos + marker.len();
            let snippet: String = text[start..].chars().take(200).collect();
            let end = snippet.find(['.', '\n']).unwrap_or(snippet.len());
            return snippet[..end].trim().to_string();
        }
    }
    // Fallback: second sentence
    let sentences: Vec<&str> = text.split('.').collect();
    if sentences.len() > 1 {
        return sentences[1].trim().chars().take(200).collect();
    }
    String::new()
}

/// Extract the outcome from the text.
fn extract_outcome(text: &str) -> String {
    let lower = text.to_lowercase();
    for marker in &["all tests pass", "tests pass", "verified", "confirmed",
                     "works correctly", "issue resolved", "bug fixed"] {
        if lower.contains(marker) {
            return marker.to_string();
        }
    }
    "completed".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_knowledge_scoring() {
        // High similarity, recent → high score
        let score1 = score_knowledge_unit(0.9, 10.0);
        // High similarity, old → lower score
        let score2 = score_knowledge_unit(0.9, 500.0);
        // Low similarity, recent → low score
        let score3 = score_knowledge_unit(0.2, 10.0);

        assert!(score1 > score2, "Recent should beat old: {} vs {}", score1, score2);
        assert!(score1 > score3, "High sim should beat low: {} vs {}", score1, score3);
        assert!(score2 > score3, "Old+high-sim should beat recent+low-sim: {} vs {}", score2, score3);
    }

    #[test]
    fn test_budget_selection() {
        let candidates = vec![
            (InjectedKnowledge {
                concept: "Retry pattern".into(),
                approach: "Exponential backoff".into(),
                outcome: "Fixed timeout issues".into(),
                source_project: "api-server".into(),
                relevance_score: 0.9,
                estimated_tokens: 50,
            }, 0.85),
            (InjectedKnowledge {
                concept: "Cache invalidation".into(),
                approach: "TTL with write-through".into(),
                outcome: "Reduced latency 10x".into(),
                source_project: "data-layer".into(),
                relevance_score: 0.7,
                estimated_tokens: 60,
            }, 0.65),
        ];

        let selected = select_knowledge_units(candidates, 100);
        assert_eq!(selected.len(), 1, "Budget of 100 tokens should fit 1 unit (50+60>100)");
        assert_eq!(selected[0].concept, "Retry pattern", "Should pick highest scored");
    }

    #[test]
    fn test_preamble_formatting() {
        let units = vec![InjectedKnowledge {
            concept: "Retry pattern".into(),
            approach: "Exponential backoff with jitter".into(),
            outcome: "Reduced failed requests by 95%".into(),
            source_project: "payment-api".into(),
            relevance_score: 0.9,
            estimated_tokens: 50,
        }];

        let preamble = format_knowledge_preamble(&units, 2048);
        assert!(preamble.contains("Retry pattern"));
        assert!(preamble.contains("payment-api"));
        assert!(preamble.contains("Exponential backoff"));
    }
}
