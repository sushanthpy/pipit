//! Speculative Compaction — hides compaction latency under LLM TTFT.
//!
//! On every turn, speculatively start a compaction pass in parallel with the
//! LLM call. The compacted state is committed only if:
//!   (a) The LLM call fails with ContextOverflow, or
//!   (b) needs_compression() was already true at turn start.
//!
//! Expected latency: E[savings] = P(overflow) × T_compact
//! On overflow turns: T_turn = max(T_llm, T_compact) instead of T_llm + T_compact
//! On non-overflow turns: zero user-visible impact (speculative work discarded)

use pipit_provider::Message;
use tokio_util::sync::CancellationToken;

/// Result of speculative compaction (may or may not be committed).
#[derive(Debug)]
pub struct SpeculativeCompactResult {
    /// The compacted message history (only committed on overflow).
    pub compacted_messages: Vec<Message>,
    /// Tokens freed by compaction.
    pub tokens_freed: u64,
    /// Messages removed.
    pub messages_removed: usize,
    /// Whether this result should be committed.
    pub should_commit: bool,
    /// Whether the speculation completed before the LLM call.
    pub completed_before_llm: bool,
}

/// Decision on whether to speculate based on telemetry signals.
///
/// Speculate when:
///   1. TTFT EMA is above threshold (model is slow → plenty of time to compact)
///   2. Token usage is above 80% of budget (overflow likely)
///   3. context.needs_compression() is already true
pub fn should_speculate(
    ttft_ema_ms: Option<f64>,
    token_usage_fraction: f64,
    needs_compression: bool,
) -> bool {
    // Always speculate if compression is already needed
    if needs_compression {
        return true;
    }

    // Speculate if token usage is high (overflow likely)
    if token_usage_fraction > 0.80 {
        return true;
    }

    // Speculate if TTFT is high (plenty of parallel time)
    if let Some(ttft) = ttft_ema_ms {
        if ttft > 1500.0 && token_usage_fraction > 0.60 {
            return true;
        }
    }

    false
}

/// Run compaction speculatively in a background task.
///
/// Returns a JoinHandle that resolves to the compacted messages.
/// The caller can await this if the LLM call fails with overflow,
/// or drop it if the LLM call succeeds.
pub fn spawn_speculative_compaction(
    messages: Vec<Message>,
    budget_tokens: u64,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<Option<SpeculativeCompactResult>> {
    tokio::spawn(async move {
        if cancel.is_cancelled() {
            return None;
        }

        let mut compacted = messages;
        let original_count = compacted.len();

        // Run lightweight compaction passes:
        // 1. Dedup pass (O(n) hash)
        let dedup_result = crate::dedup::dedup_tool_results(&mut compacted);

        // 2. Utility-based eviction if still over budget
        let current_tokens: u64 = compacted.iter()
            .map(|m| m.estimated_tokens())
            .sum();

        let mut total_freed = dedup_result.tokens_freed;

        if current_tokens > budget_tokens {
            let utilities = crate::utility::estimate_utilities(&compacted, 6);
            let keep = crate::utility::greedy_knapsack(&utilities, budget_tokens);
            let (evicted, freed) = crate::utility::apply_eviction(&mut compacted, &keep);
            total_freed += freed;
        }

        let messages_removed = original_count - compacted.len();

        Some(SpeculativeCompactResult {
            compacted_messages: compacted,
            tokens_freed: total_freed,
            messages_removed,
            should_commit: false, // caller decides
            completed_before_llm: true,
        })
    })
}

/// Commit a speculative compaction result: replace the live messages
/// with the pre-compacted version.
pub fn commit_speculation(
    live_messages: &mut Vec<Message>,
    result: SpeculativeCompactResult,
) -> (usize, u64) {
    let removed = result.messages_removed;
    let freed = result.tokens_freed;
    *live_messages = result.compacted_messages;
    (removed, freed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_speculate_when_compression_needed() {
        assert!(should_speculate(None, 0.5, true));
    }

    #[test]
    fn should_speculate_when_high_usage() {
        assert!(should_speculate(None, 0.85, false));
    }

    #[test]
    fn should_not_speculate_when_low_usage() {
        assert!(!should_speculate(None, 0.3, false));
    }

    #[test]
    fn should_speculate_with_high_ttft_and_moderate_usage() {
        assert!(should_speculate(Some(2000.0), 0.65, false));
    }

    #[test]
    fn should_not_speculate_with_low_ttft_and_moderate_usage() {
        assert!(!should_speculate(Some(500.0), 0.65, false));
    }
}
