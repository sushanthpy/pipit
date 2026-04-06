
//! Prompt Cache Optimizer — Strategic cache breakpoint placement (Task 6).
//!
//! Anthropic pricing: cache write = 1.25× input, cache read = 0.1× input.
//! Break-even: k > write_cost / read_cost ≈ k > 3 reuses.
//!
//! Strategy: Place cache breakpoints at the longest stable prefix boundary
//! (system prompt + memory + tool declarations). These don't change between
//! turns, so they get high reuse counts.
//!
//! Algorithm:
//!   1. Compute LCP(req_i, req_{i+1}) — longest common prefix of consecutive requests
//!   2. If LCP length ≥ threshold → mark as cache breakpoint
//!   3. Monitor cache hit rate and adjust placement
//!
//! Complexity: LCP computation O(min(|req_i|, |req_{i+1}|)).

use serde::{Deserialize, Serialize};

/// Cache breakpoint placement in the prompt.
#[derive(Debug, Clone, Serialize)]
pub struct CacheBreakpoint {
    /// Byte offset in the serialized prompt where the cache boundary falls.
    pub offset: usize,
    /// Estimated token count up to this point.
    pub token_estimate: u64,
    /// What kind of content precedes this breakpoint.
    pub content_type: CacheContentType,
    /// Expected reuse count based on historical data.
    pub expected_reuse: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum CacheContentType {
    /// System prompt (highest reuse — same across entire session).
    SystemPrompt,
    /// Memory + preferences (high reuse — changes rarely within a session).
    Memory,
    /// Tool declarations (high reuse — stable unless MCP tools change).
    ToolDeclarations,
    /// Conversation history (decreasing reuse — grows each turn).
    ConversationPrefix,
}

/// Metrics for cache performance monitoring.
#[derive(Debug, Clone, Default)]
pub struct CacheMetrics {
    pub total_requests: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub total_cached_tokens: u64,
    pub total_uncached_tokens: u64,
    pub estimated_savings_usd: f64,
}

impl CacheMetrics {
    pub fn hit_rate(&self) -> f64 {
        if self.total_requests == 0 {
            return 0.0;
        }
        self.cache_hits as f64 / self.total_requests as f64
    }

    pub fn record_hit(&mut self, cached_tokens: u64) {
        self.total_requests += 1;
        self.cache_hits += 1;
        self.total_cached_tokens += cached_tokens;
        // Savings: (full_input_cost - cache_read_cost) * cached_tokens
        // Anthropic: input = $3/M, cache_read = $0.30/M → savings = $2.70/M
        self.estimated_savings_usd += cached_tokens as f64 * 2.70 / 1_000_000.0;
    }

    pub fn record_miss(&mut self, uncached_tokens: u64) {
        self.total_requests += 1;
        self.cache_misses += 1;
        self.total_uncached_tokens += uncached_tokens;
    }
}

/// The cache optimizer. Tracks prompt structure and recommends breakpoints.
pub struct CacheOptimizer {
    /// Previous request's serialized prefix (for LCP computation).
    previous_prefix: Option<Vec<u8>>,
    /// Detected stable prefix length (bytes).
    stable_prefix_len: usize,
    /// Number of consecutive turns with the same prefix.
    prefix_stability_count: u32,
    /// Cache metrics for this session.
    pub metrics: CacheMetrics,
    /// Minimum stability count before we trust a prefix.
    stability_threshold: u32,
}

impl CacheOptimizer {
    pub fn new() -> Self {
        Self {
            previous_prefix: None,
            stable_prefix_len: 0,
            prefix_stability_count: 0,
            metrics: CacheMetrics::default(),
            stability_threshold: 2, // Need 2 consecutive matches to trust
        }
    }

    /// Analyze a new request and compute cache breakpoints.
    ///
    /// Call this before sending each request to the LLM.
    /// Returns recommended breakpoints for the `cache_control` API parameter.
    pub fn analyze_request(&mut self, prompt_sections: &[PromptSection]) -> Vec<CacheBreakpoint> {
        let serialized = serialize_sections(prompt_sections);
        let mut breakpoints = Vec::new();

        // Compute LCP with previous request
        if let Some(ref prev) = self.previous_prefix {
            let lcp_len = longest_common_prefix(prev, &serialized);

            if lcp_len >= self.stable_prefix_len.saturating_sub(100) {
                // Prefix is stable (within 100 bytes tolerance)
                self.prefix_stability_count += 1;
            } else if lcp_len > self.stable_prefix_len {
                // Prefix grew (new content added but old prefix unchanged)
                self.stable_prefix_len = lcp_len;
                self.prefix_stability_count = 1;
            } else {
                // Prefix changed significantly — reset
                self.stable_prefix_len = lcp_len;
                self.prefix_stability_count = 0;
            }
        } else {
            // First request — set baseline
            self.stable_prefix_len = serialized.len();
            self.prefix_stability_count = 0;
        }

        self.previous_prefix = Some(serialized.clone());

        // Place breakpoints at section boundaries within the stable prefix
        let mut offset = 0;
        for section in prompt_sections {
            let section_end = offset + section.content.len();

            if section_end <= self.stable_prefix_len
                && self.prefix_stability_count >= self.stability_threshold
            {
                // This section is within the stable prefix — good cache candidate
                let expected_reuse = estimate_reuse(section.content_type, self.prefix_stability_count);

                if expected_reuse >= 3 {
                    // Break-even: need ≥3 reuses to justify cache write cost
                    breakpoints.push(CacheBreakpoint {
                        offset: section_end,
                        token_estimate: (section.content.len() as u64) / 4,
                        content_type: section.content_type,
                        expected_reuse,
                    });
                }
            }

            offset = section_end;
        }

        breakpoints
    }

    /// Record whether the LLM response indicates cache was used.
    pub fn record_response(&mut self, cache_creation_tokens: u64, cache_read_tokens: u64) {
        if cache_read_tokens > 0 {
            self.metrics.record_hit(cache_read_tokens);
        }
        if cache_creation_tokens > 0 {
            self.metrics.record_miss(cache_creation_tokens);
        }
    }

    /// Check if a tool declaration change would break the cache.
    ///
    /// Returns true if the tool set has changed since the last request.
    /// The caller should consider reordering tools to maintain the prefix.
    pub fn would_break_cache(&self, new_tool_names: &[String], prev_tool_names: &[String]) -> bool {
        if new_tool_names.len() != prev_tool_names.len() {
            return true;
        }
        // Order matters for caching — same tools in different order = cache miss
        new_tool_names != prev_tool_names
    }
}

/// A section of the prompt (for breakpoint analysis).
#[derive(Debug, Clone)]
pub struct PromptSection {
    pub content_type: CacheContentType,
    pub content: String,
}

fn serialize_sections(sections: &[PromptSection]) -> Vec<u8> {
    let mut buf = Vec::new();
    for section in sections {
        buf.extend_from_slice(section.content.as_bytes());
    }
    buf
}

/// Compute the longest common prefix of two byte slices.
///
/// O(min(|a|, |b|))
fn longest_common_prefix(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
}

/// Estimate reuse count based on content type and historical stability.
fn estimate_reuse(content_type: CacheContentType, stability_count: u32) -> u32 {
    let base = match content_type {
        CacheContentType::SystemPrompt => 100,       // Stable for entire session
        CacheContentType::Memory => 50,               // Changes rarely
        CacheContentType::ToolDeclarations => 80,     // Stable unless MCP changes
        CacheContentType::ConversationPrefix => 5,    // Decreasing reuse
    };
    // Scale by observed stability
    base.min(stability_count * 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcp_computation() {
        assert_eq!(longest_common_prefix(b"hello world", b"hello there"), 6);
        assert_eq!(longest_common_prefix(b"abc", b"abc"), 3);
        assert_eq!(longest_common_prefix(b"abc", b"xyz"), 0);
        assert_eq!(longest_common_prefix(b"", b"abc"), 0);
    }

    #[test]
    fn cache_metrics_tracking() {
        let mut m = CacheMetrics::default();
        m.record_hit(1000);
        m.record_hit(1000);
        m.record_miss(500);

        assert_eq!(m.total_requests, 3);
        assert_eq!(m.cache_hits, 2);
        assert!((m.hit_rate() - 0.6667).abs() < 0.01);
        assert!(m.estimated_savings_usd > 0.0);
    }

    #[test]
    fn breakpoint_detection_after_stability() {
        let mut optimizer = CacheOptimizer::new();

        let sections = vec![
            PromptSection {
                content_type: CacheContentType::SystemPrompt,
                content: "You are a helpful assistant.".into(),
            },
            PromptSection {
                content_type: CacheContentType::Memory,
                content: "User prefers Rust.".into(),
            },
        ];

        // First request — no breakpoints (no stability yet)
        let bp1 = optimizer.analyze_request(&sections);
        assert!(bp1.is_empty());

        // Second request — same prefix, building stability
        let bp2 = optimizer.analyze_request(&sections);
        assert!(bp2.is_empty()); // Need 2 stable turns

        // Third request — stability threshold reached
        let bp3 = optimizer.analyze_request(&sections);
        assert!(!bp3.is_empty());
        assert_eq!(bp3[0].content_type as u8, CacheContentType::SystemPrompt as u8);
    }

    #[test]
    fn tool_order_change_breaks_cache() {
        let optimizer = CacheOptimizer::new();
        let prev = vec!["bash".into(), "read_file".into(), "write_file".into()];
        let same = vec!["bash".into(), "read_file".into(), "write_file".into()];
        let reordered = vec!["read_file".into(), "bash".into(), "write_file".into()];
        let added = vec!["bash".into(), "read_file".into(), "write_file".into(), "grep".into()];

        assert!(!optimizer.would_break_cache(&same, &prev));
        assert!(optimizer.would_break_cache(&reordered, &prev));
        assert!(optimizer.would_break_cache(&added, &prev));
    }
}

