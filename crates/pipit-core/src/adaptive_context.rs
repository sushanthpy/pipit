//! Adaptive Context Controller 2.0
//!
//! Replaces threshold-based compression with a tiered memory controller:
//! - Pinned: user objective, current plan, active file context (never evicted)
//! - Active: recent edits, latest tool results, verifier findings (high priority)
//! - Historical: older turns, summarized interactions (medium priority)
//! - Exhaust: stale tool results, superseded file reads (evict first)
//!
//! Each segment has a utility score U_i / C_i where U_i is relevance and C_i is token cost.
//! Retention is greedy by descending marginal utility: O(n log n) from sorting.

use serde::{Deserialize, Serialize};

/// Memory tier for a context segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MemoryTier {
    /// Exhaust — stale readings, superseded results. Evict first.
    Exhaust = 0,
    /// Historical — older turns, stable context. Summarize.
    Historical = 1,
    /// Active — recent edits, current tool results. Keep.
    Active = 2,
    /// Pinned — user objective, plan, critical constraints. Never evict.
    Pinned = 3,
}

/// A scored segment of the context window.
#[derive(Debug, Clone)]
pub struct ContextSegment {
    /// Unique segment identifier.
    pub id: String,
    /// Memory tier classification.
    pub tier: MemoryTier,
    /// Estimated token cost.
    pub tokens: u64,
    /// Relevance score (0.0–1.0).
    pub relevance: f64,
    /// Turn number when this segment was created.
    pub created_at_turn: u32,
    /// Number of message indices this segment spans.
    pub message_range: (usize, usize),
    /// Content type for classification.
    pub content_type: SegmentContent,
}

/// What kind of content a segment contains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SegmentContent {
    /// User's original objective/request.
    UserObjective,
    /// Current execution plan.
    Plan,
    /// File content read by the agent.
    FileRead { path: String },
    /// Tool result (non-file).
    ToolResult { tool_name: String },
    /// Agent's reasoning/response.
    AgentResponse,
    /// Verification result.
    Verification,
    /// Compressed summary of older turns.
    Summary,
    /// System prompt fragment.
    System,
}

impl ContextSegment {
    /// Marginal utility: relevance / token_cost.
    pub fn marginal_utility(&self) -> f64 {
        if self.tokens == 0 {
            return f64::MAX;
        }
        self.relevance / self.tokens as f64
    }

    /// Whether this segment is pinned (never evicted).
    pub fn is_pinned(&self) -> bool {
        self.tier == MemoryTier::Pinned
    }
}

/// The adaptive context controller — decides what to keep, summarize, or evict.
pub struct AdaptiveContextController {
    /// All tracked segments.
    segments: Vec<ContextSegment>,
    /// Current turn number (for recency scoring).
    current_turn: u32,
    /// Recency decay factor (0.0–1.0). Higher = stronger recency bias.
    recency_decay: f64,
}

impl AdaptiveContextController {
    pub fn new() -> Self {
        Self {
            segments: Vec::new(),
            current_turn: 0,
            recency_decay: 0.9,
        }
    }

    /// Advance to the next turn (updates recency scoring).
    pub fn advance_turn(&mut self) {
        self.current_turn += 1;
    }

    /// Add a segment to the controller.
    pub fn add_segment(&mut self, mut segment: ContextSegment) {
        segment.created_at_turn = self.current_turn;
        self.segments.push(segment);
    }

    /// Classify a message into a memory tier based on content analysis.
    pub fn classify_tier(content_type: &SegmentContent, age_turns: u32) -> MemoryTier {
        match content_type {
            SegmentContent::UserObjective | SegmentContent::Plan | SegmentContent::System => {
                MemoryTier::Pinned
            }
            SegmentContent::Verification => {
                if age_turns <= 2 {
                    MemoryTier::Active
                } else {
                    MemoryTier::Historical
                }
            }
            SegmentContent::FileRead { .. } | SegmentContent::ToolResult { .. } => {
                if age_turns <= 1 {
                    MemoryTier::Active
                } else if age_turns <= 5 {
                    MemoryTier::Historical
                } else {
                    MemoryTier::Exhaust
                }
            }
            SegmentContent::AgentResponse => {
                if age_turns <= 2 {
                    MemoryTier::Active
                } else {
                    MemoryTier::Historical
                }
            }
            SegmentContent::Summary => MemoryTier::Historical,
        }
    }

    /// Compute the retention plan: which segments to keep, summarize, or evict.
    /// Greedy knapsack by descending marginal utility within the token budget.
    pub fn retention_plan(&self, budget: u64) -> RetentionPlan {
        let mut scored: Vec<(usize, f64)> = self
            .segments
            .iter()
            .enumerate()
            .map(|(i, seg)| {
                let age = self.current_turn.saturating_sub(seg.created_at_turn);
                let recency_factor = self.recency_decay.powi(age as i32);
                let tier_boost = match seg.tier {
                    MemoryTier::Pinned => 100.0,
                    MemoryTier::Active => 2.0,
                    MemoryTier::Historical => 1.0,
                    MemoryTier::Exhaust => 0.3,
                };
                let score =
                    seg.relevance * recency_factor * tier_boost / (seg.tokens.max(1) as f64);
                (i, score)
            })
            .collect();

        // Sort by score descending
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut keep = Vec::new();
        let mut summarize = Vec::new();
        let mut evict = Vec::new();
        let mut used = 0u64;

        for (idx, _score) in &scored {
            let seg = &self.segments[*idx];

            if seg.is_pinned() {
                keep.push(*idx);
                used += seg.tokens;
                continue;
            }

            if used + seg.tokens <= budget {
                keep.push(*idx);
                used += seg.tokens;
            } else if used + seg.tokens / 3 <= budget {
                // Can fit a summary (estimated 1/3 of original)
                summarize.push(*idx);
                used += seg.tokens / 3;
            } else {
                evict.push(*idx);
            }
        }

        RetentionPlan {
            keep,
            summarize,
            evict,
            tokens_kept: used,
            budget_remaining: budget.saturating_sub(used),
        }
    }

    /// Update segment tiers based on current turn (re-classify by age).
    pub fn reclassify_tiers(&mut self) {
        for seg in &mut self.segments {
            let age = self.current_turn.saturating_sub(seg.created_at_turn);
            let new_tier = Self::classify_tier(&seg.content_type, age);
            // Never downgrade pinned segments
            if seg.tier != MemoryTier::Pinned {
                seg.tier = new_tier;
            }
        }
    }

    /// Total tokens across all segments.
    pub fn total_tokens(&self) -> u64 {
        self.segments.iter().map(|s| s.tokens).sum()
    }

    /// Number of segments.
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// Get segments by tier.
    pub fn segments_by_tier(&self, tier: MemoryTier) -> Vec<&ContextSegment> {
        self.segments.iter().filter(|s| s.tier == tier).collect()
    }
}

impl Default for AdaptiveContextController {
    fn default() -> Self {
        Self::new()
    }
}

/// The output of the retention plan.
#[derive(Debug, Clone)]
pub struct RetentionPlan {
    /// Indices of segments to keep as-is.
    pub keep: Vec<usize>,
    /// Indices of segments to summarize (compress to ~1/3 tokens).
    pub summarize: Vec<usize>,
    /// Indices of segments to evict entirely.
    pub evict: Vec<usize>,
    /// Total tokens retained.
    pub tokens_kept: u64,
    /// Budget remaining after retention.
    pub budget_remaining: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_segments_always_retained() {
        let mut ctrl = AdaptiveContextController::new();
        ctrl.add_segment(ContextSegment {
            id: "objective".into(),
            tier: MemoryTier::Pinned,
            tokens: 100,
            relevance: 1.0,
            created_at_turn: 0,
            message_range: (0, 0),
            content_type: SegmentContent::UserObjective,
        });
        ctrl.add_segment(ContextSegment {
            id: "exhaust".into(),
            tier: MemoryTier::Exhaust,
            tokens: 5000,
            relevance: 0.1,
            created_at_turn: 0,
            message_range: (1, 1),
            content_type: SegmentContent::ToolResult {
                tool_name: "read_file".into(),
            },
        });

        // Very tight budget — pinned should still be kept
        let plan = ctrl.retention_plan(200);
        assert!(plan.keep.contains(&0)); // pinned
        assert!(plan.evict.contains(&1)); // exhaust
    }

    #[test]
    fn recency_favors_recent_segments() {
        let mut ctrl = AdaptiveContextController::new();
        ctrl.add_segment(ContextSegment {
            id: "old".into(),
            tier: MemoryTier::Active,
            tokens: 100,
            relevance: 0.8,
            created_at_turn: 0,
            message_range: (0, 0),
            content_type: SegmentContent::AgentResponse,
        });
        ctrl.current_turn = 10;
        ctrl.add_segment(ContextSegment {
            id: "new".into(),
            tier: MemoryTier::Active,
            tokens: 100,
            relevance: 0.8,
            created_at_turn: 10,
            message_range: (1, 1),
            content_type: SegmentContent::AgentResponse,
        });

        let plan = ctrl.retention_plan(150);
        // Only room for one — should keep the newer one
        assert!(plan.keep.contains(&1));
    }

    #[test]
    fn tier_reclassification() {
        let tier = AdaptiveContextController::classify_tier(
            &SegmentContent::ToolResult {
                tool_name: "read_file".into(),
            },
            0,
        );
        assert_eq!(tier, MemoryTier::Active);

        let tier = AdaptiveContextController::classify_tier(
            &SegmentContent::ToolResult {
                tool_name: "read_file".into(),
            },
            6,
        );
        assert_eq!(tier, MemoryTier::Exhaust);
    }
}
