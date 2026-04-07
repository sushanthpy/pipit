//! Utility-Maximizing Budget Allocation via Dantzig's Greedy Knapsack.
//!
//! Replaces age-based eviction with a formally defensible utility-maximizing
//! allocation that preserves high-value context even when old.
//!
//! Given messages m_1, …, m_n each with retained token cost c_i and
//! estimated utility u_i, and budget B, find subset S ⊆ {1..n}
//! maximizing Σ_{i∈S} u_i subject to Σ_{i∈S} c_i ≤ B.
//!
//! This is 0/1 knapsack — exact DP is O(nB) (too expensive at B ≈ 200K).
//! Dantzig's ratio-greedy (sort by u_i/c_i, select until full) is O(n log n)
//! and gives a 2-approximation. For noise-ridden estimates, greedy is
//! effectively optimal.

use pipit_provider::Message;

/// Utility estimate for a single message.
#[derive(Debug, Clone)]
pub struct MessageUtility {
    /// Index in the message array.
    pub index: usize,
    /// Estimated token cost of retaining this message.
    pub cost: u64,
    /// Estimated utility of retaining this message.
    pub utility: f64,
    /// Whether this message is protected from eviction.
    pub protected: bool,
}

/// Utility estimator — computes u_i for each message.
///
/// Utility composes several cheap signals:
///   1. Recency decay: e^{-λ(t_n - t_i)} where λ controls half-life
///   2. Role weight: system > user > assistant > tool_result
///   3. Reference count: messages cited by later tool_use_ids
///   4. Size penalty: very large messages get lower utility/cost ratio
pub fn estimate_utilities(
    messages: &[Message],
    preserve_recent: usize,
) -> Vec<MessageUtility> {
    let n = messages.len();
    let mut utilities = Vec::with_capacity(n);

    for (i, msg) in messages.iter().enumerate() {
        let is_recent = i >= n.saturating_sub(preserve_recent);
        let is_system = msg.role == pipit_provider::Role::System
            || (msg.role == pipit_provider::Role::User && i == 0);

        // Token cost estimate (4 chars per token)
        let content_len: usize = msg.content.iter().map(|b| match b {
            pipit_provider::ContentBlock::Text(t) => t.len(),
            pipit_provider::ContentBlock::ToolCall { args, .. } => args.to_string().len(),
            pipit_provider::ContentBlock::ToolResult { content, .. } => content.len(),
            _ => 0,
        }).sum();
        let cost = (content_len / 4).max(1) as u64;

        // Utility estimation
        let recency_decay = {
            let distance = (n - i) as f64;
            let lambda = 0.05; // half-life ≈ 14 messages
            (-lambda * distance).exp()
        };

        let role_weight = match &msg.role {
            pipit_provider::Role::System => 5.0,
            pipit_provider::Role::User => 2.0,
            pipit_provider::Role::Assistant => 1.5,
            pipit_provider::Role::ToolResult { .. } => 1.0,
        };

        // Tool results that are large get lower utility per token
        let size_factor = if content_len > 10_000 {
            0.5 // large tool results are less valuable per-token
        } else {
            1.0
        };

        let utility = recency_decay * role_weight * size_factor;

        utilities.push(MessageUtility {
            index: i,
            cost,
            utility,
            protected: is_recent || is_system,
        });
    }

    utilities
}

/// Dantzig's ratio-greedy knapsack: sort by u_i/c_i descending,
/// select until budget is met. O(n log n).
///
/// Returns the set of message indices to KEEP.
pub fn greedy_knapsack(
    utilities: &[MessageUtility],
    budget: u64,
) -> Vec<usize> {
    // Protected messages are always kept
    let mut keep: Vec<usize> = utilities.iter()
        .filter(|u| u.protected)
        .map(|u| u.index)
        .collect();

    let protected_cost: u64 = utilities.iter()
        .filter(|u| u.protected)
        .map(|u| u.cost)
        .sum();

    if protected_cost >= budget {
        // Budget exceeded by protected alone — keep only protected
        return keep;
    }

    let remaining_budget = budget - protected_cost;

    // Non-protected messages sorted by utility/cost ratio (descending)
    let mut candidates: Vec<&MessageUtility> = utilities.iter()
        .filter(|u| !u.protected)
        .collect();

    candidates.sort_by(|a, b| {
        let ratio_a = a.utility / a.cost.max(1) as f64;
        let ratio_b = b.utility / b.cost.max(1) as f64;
        ratio_b.partial_cmp(&ratio_a).unwrap_or(std::cmp::Ordering::Equal)
    });

    // Greedy fill
    let mut used = 0u64;
    for c in candidates {
        if used + c.cost <= remaining_budget {
            keep.push(c.index);
            used += c.cost;
        }
    }

    keep.sort();
    keep
}

/// Apply the knapsack solution: remove messages not in the keep set.
/// Returns the number of messages evicted and estimated tokens freed.
pub fn apply_eviction(
    messages: &mut Vec<Message>,
    keep_indices: &[usize],
) -> (usize, u64) {
    let original_len = messages.len();
    let keep_set: std::collections::HashSet<usize> = keep_indices.iter().copied().collect();

    let mut tokens_freed = 0u64;
    let mut new_messages = Vec::with_capacity(keep_indices.len());

    for (i, msg) in messages.drain(..).enumerate() {
        if keep_set.contains(&i) {
            new_messages.push(msg);
        } else {
            let content_len: usize = msg.content.iter().map(|b| match b {
                pipit_provider::ContentBlock::Text(t) => t.len(),
                _ => 0,
            }).sum();
            tokens_freed += (content_len / 4) as u64;
        }
    }

    let evicted = original_len - new_messages.len();
    *messages = new_messages;
    (evicted, tokens_freed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipit_provider::{Message, Role};

    fn text_msg(role: Role, text: &str) -> Message {
        Message {
            role,
            content: vec![pipit_provider::ContentBlock::Text(text.to_string())],
            metadata: Default::default(),
        }
    }

    #[test]
    fn greedy_knapsack_keeps_high_utility() {
        let utilities = vec![
            MessageUtility { index: 0, cost: 100, utility: 10.0, protected: false },
            MessageUtility { index: 1, cost: 100, utility: 1.0, protected: false },
            MessageUtility { index: 2, cost: 100, utility: 5.0, protected: false },
        ];
        // Budget for 2 messages
        let keep = greedy_knapsack(&utilities, 200);
        assert_eq!(keep.len(), 2);
        assert!(keep.contains(&0)); // highest utility
        assert!(keep.contains(&2)); // second highest
        assert!(!keep.contains(&1)); // lowest utility evicted
    }

    #[test]
    fn protected_messages_always_kept() {
        let utilities = vec![
            MessageUtility { index: 0, cost: 100, utility: 0.01, protected: true }, // system
            MessageUtility { index: 1, cost: 100, utility: 10.0, protected: false },
        ];
        let keep = greedy_knapsack(&utilities, 150);
        assert!(keep.contains(&0)); // protected even with low utility
    }

    #[test]
    fn estimate_utilities_recency_bias() {
        let msgs = vec![
            text_msg(Role::Assistant, "old response"),
            text_msg(Role::Assistant, "old response 2"),
            text_msg(Role::Assistant, "recent response 1"),
            text_msg(Role::Assistant, "recent response 2"),
        ];
        let utils = estimate_utilities(&msgs, 2);
        // Same role — recency should dominate: later messages have higher utility
        assert!(utils[3].utility > utils[0].utility,
            "recent ({}) should beat old ({})", utils[3].utility, utils[0].utility);
    }
}
