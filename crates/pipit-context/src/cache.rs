//! Prompt Cache Breakpoint System
//!
//! Cache-aware message construction that maximizes cache reuse across turns.
//! Strategically places `cache_control: { type: "ephemeral" }` breakpoints
//! at boundaries of stable message segments so the API can cache them.
//!
//! The optimal breakpoint placement is a weighted interval scheduling problem:
//! place breakpoints at the boundary of the k most stable segments.

use pipit_provider::{ContentBlock, Message, UsageMetadata};
use std::collections::VecDeque;

/// Maximum number of cache breakpoints per request.
/// Anthropic's API allows up to 4 ephemeral cache breakpoints.
const MAX_BREAKPOINTS: usize = 4;

/// Cache efficiency metrics tracked per turn.
#[derive(Debug, Clone, Default)]
pub struct CacheMetrics {
    /// Tokens read from cache (free / reduced cost).
    pub cache_read_tokens: u64,
    /// Tokens written to cache (creation cost).
    pub cache_creation_tokens: u64,
    /// Total input tokens for the request.
    pub total_input_tokens: u64,
    /// Cache hit ratio: cache_read / total_input.
    pub hit_ratio: f64,
}

impl CacheMetrics {
    pub fn from_usage(usage: &UsageMetadata) -> Self {
        let cache_read = usage.cache_read_tokens.unwrap_or(0);
        let total = usage.input_tokens;
        Self {
            cache_read_tokens: cache_read,
            cache_creation_tokens: usage.cache_creation_tokens.unwrap_or(0),
            total_input_tokens: total,
            hit_ratio: if total > 0 {
                cache_read as f64 / total as f64
            } else {
                0.0
            },
        }
    }
}

/// Stability classification for message segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SegmentStability {
    /// System prompt — never changes within a session.
    Immutable = 4,
    /// Conversation summaries — rarely change.
    Summary = 3,
    /// Old user/assistant messages — stable after compaction.
    Historical = 2,
    /// Recent messages — may change on next compaction.
    Recent = 1,
    /// Current turn — always changes.
    Current = 0,
}

/// A message segment with stability weight for cache placement decisions.
#[derive(Debug, Clone)]
pub struct CacheSegment {
    /// Index range in the message array [start, end).
    pub range: std::ops::Range<usize>,
    /// Stability classification.
    pub stability: SegmentStability,
    /// Estimated token count for this segment.
    pub token_count: u64,
}

/// The cache breakpoint planner.
pub struct CacheBreakpointPlanner {
    /// Historical cache metrics for efficiency tracking.
    history: VecDeque<CacheMetrics>,
    /// Maximum history entries to retain.
    max_history: usize,
}

impl CacheBreakpointPlanner {
    pub fn new() -> Self {
        Self {
            history: VecDeque::with_capacity(100),
            max_history: 100,
        }
    }

    /// Analyze messages and determine optimal cache breakpoint positions.
    /// Returns indices where `CacheBreakpoint` content blocks should be inserted.
    pub fn plan_breakpoints(
        &self,
        messages: &[Message],
        system_prompt_tokens: u64,
        preserve_recent: usize,
    ) -> Vec<usize> {
        if messages.is_empty() {
            return vec![];
        }

        // Classify message segments by stability
        let segments = self.classify_segments(messages, preserve_recent);

        // Greedy placement: put breakpoints after the most stable, largest segments
        // Sort by (stability DESC, token_count DESC)
        let mut ranked: Vec<(usize, &CacheSegment)> = segments.iter().enumerate().collect();
        ranked.sort_by(|a, b| {
            b.1.stability
                .cmp(&a.1.stability)
                .then(b.1.token_count.cmp(&a.1.token_count))
        });

        // Select top-k breakpoint positions (end of each selected segment)
        let mut breakpoint_indices: Vec<usize> = ranked
            .iter()
            .take(MAX_BREAKPOINTS)
            .filter(|(_, seg)| seg.stability >= SegmentStability::Historical)
            .map(|(_, seg)| seg.range.end.saturating_sub(1))
            .collect();

        // Sort by position (ascending) for insertion order
        breakpoint_indices.sort();
        breakpoint_indices.dedup();
        breakpoint_indices
    }

    /// Insert cache breakpoint markers into messages at the planned positions.
    pub fn insert_breakpoints(messages: &mut Vec<Message>, positions: &[usize]) {
        // Insert in reverse order to avoid index invalidation
        for &pos in positions.iter().rev() {
            if pos < messages.len() {
                messages[pos].content.push(ContentBlock::CacheBreakpoint);
            }
        }
    }

    /// Record cache metrics from an API response.
    pub fn record_metrics(&mut self, usage: &UsageMetadata) {
        let metrics = CacheMetrics::from_usage(usage);
        if self.history.len() >= self.max_history {
            self.history.pop_front();
        }
        self.history.push_back(metrics);
    }

    /// Get the average cache hit ratio over recent turns.
    pub fn average_hit_ratio(&self) -> f64 {
        if self.history.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.history.iter().map(|m| m.hit_ratio).sum();
        sum / self.history.len() as f64
    }

    /// Get estimated cost savings from caching (as a fraction).
    /// Cached reads typically cost 90% less than uncached.
    pub fn estimated_savings_ratio(&self) -> f64 {
        if self.history.is_empty() {
            return 0.0;
        }
        let avg_hit = self.average_hit_ratio();
        // Anthropic charges 10% for cached reads
        avg_hit * 0.9
    }

    /// Get the most recent cache metrics.
    pub fn last_metrics(&self) -> Option<&CacheMetrics> {
        self.history.back()
    }

    fn classify_segments(&self, messages: &[Message], preserve_recent: usize) -> Vec<CacheSegment> {
        let mut segments = Vec::new();
        let total = messages.len();
        let recent_boundary = total.saturating_sub(preserve_recent);

        let mut seg_start = 0;
        while seg_start < total {
            let stability = if messages[seg_start].metadata.is_summary {
                SegmentStability::Summary
            } else if seg_start >= recent_boundary {
                SegmentStability::Recent
            } else {
                SegmentStability::Historical
            };

            // Extend segment while same stability class
            let mut seg_end = seg_start + 1;
            while seg_end < total {
                let next_stability = if messages[seg_end].metadata.is_summary {
                    SegmentStability::Summary
                } else if seg_end >= recent_boundary {
                    SegmentStability::Recent
                } else {
                    SegmentStability::Historical
                };

                if next_stability != stability {
                    break;
                }
                seg_end += 1;
            }

            let token_count: u64 = messages[seg_start..seg_end]
                .iter()
                .map(|m| m.estimated_tokens())
                .sum();

            segments.push(CacheSegment {
                range: seg_start..seg_end,
                stability,
                token_count,
            });

            seg_start = seg_end;
        }

        segments
    }
}

impl Default for CacheBreakpointPlanner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakpoints_placed_at_stable_segments() {
        let planner = CacheBreakpointPlanner::new();

        let mut messages = vec![
            {
                let mut m = Message::system("[Summary] Files modified: ...");
                m.metadata.is_summary = true;
                m
            },
            Message::user("Fix the bug in auth.rs"),
            Message::assistant("I'll look at auth.rs"),
            Message::user("Also update tests"),
            Message::assistant("Done"),
        ];

        let positions = planner.plan_breakpoints(&messages, 1000, 2);
        // Should place breakpoints after summary and historical segments
        assert!(!positions.is_empty());
        // Summary segment ends at index 0, so breakpoint at 0
        assert!(positions.contains(&0));
    }

    #[test]
    fn cache_metrics_tracking() {
        let mut planner = CacheBreakpointPlanner::new();

        planner.record_metrics(&UsageMetadata {
            input_tokens: 1000,
            output_tokens: 200,
            cache_read_tokens: Some(800),
            cache_creation_tokens: Some(200),
        });

        let metrics = planner.last_metrics().unwrap();
        assert_eq!(metrics.cache_read_tokens, 800);
        assert!((metrics.hit_ratio - 0.8).abs() < 0.01);
        assert!((planner.estimated_savings_ratio() - 0.72).abs() < 0.01);
    }
}
