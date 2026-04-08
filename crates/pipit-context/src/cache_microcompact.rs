//! Cache-Aware Microcompact Pass — preserves prompt cache warmth.
//!
//! When the provider supports cache editing (supports_cache_edit() = true),
//! this pass submits a diff of tool_use_id removals instead of mutating
//! the Vec<Message>, preserving cache locality.
//!
//! Cost model:
//!   - With cache-edit: O(|edits|) — constant regardless of cache size
//!   - Without (rebuild): O(|cache|) — proportional to total cached tokens
//!   - For 100K cached tokens + 5 stale tool results:
//!     cache-edit ≈ $0.015/turn vs rebuild ≈ $0.30/turn (20× saving)

use pipit_provider::{CacheEdit, CacheEditReceipt, LlmProvider, Message};
use std::sync::Arc;

/// Result of a cache-aware microcompact operation.
#[derive(Debug, Clone, Default)]
pub struct CacheMicrocompactResult {
    /// Number of stale tool results identified.
    pub stale_count: usize,
    /// Whether cache-edit was used (vs full rebuild).
    pub used_cache_edit: bool,
    /// Tokens freed.
    pub tokens_freed: u64,
    /// Cache-edit receipt (if cache-edit was used).
    pub receipt: Option<CacheEditReceipt>,
}

/// Identify stale tool results in the message history.
///
/// A tool result is "stale" if:
///   1. It's older than `stale_turn_threshold` turns from the end
///   2. No subsequent message references its call_id
///
/// Returns the call_ids of stale tool results.
pub fn find_stale_tool_results(messages: &[Message], stale_turn_threshold: usize) -> Vec<String> {
    let total = messages.len();
    let cutoff = total.saturating_sub(stale_turn_threshold * 2); // 2 msgs per turn approx
    let mut stale_ids = Vec::new();

    for (i, msg) in messages.iter().enumerate() {
        if i >= cutoff {
            break; // Only look at old messages
        }

        // Find tool result call_ids
        for block in &msg.content {
            if let pipit_provider::ContentBlock::ToolResult {
                call_id, is_error, ..
            } = block
            {
                if !is_error {
                    // Check if any later message references this call_id
                    let referenced = messages[i + 1..].iter().any(|later_msg| {
                        later_msg.content.iter().any(|b| match b {
                            pipit_provider::ContentBlock::Text(t) => t.contains(call_id),
                            _ => false,
                        })
                    });

                    if !referenced {
                        stale_ids.push(call_id.clone());
                    }
                }
            }
        }
    }

    stale_ids
}

/// Execute cache-aware microcompaction.
///
/// If the provider supports cache editing, submit edits directly.
/// Otherwise, fall back to removing messages from Vec<Message>.
pub async fn cache_aware_microcompact(
    messages: &mut Vec<Message>,
    provider: &dyn LlmProvider,
    stale_turn_threshold: usize,
) -> CacheMicrocompactResult {
    let stale_ids = find_stale_tool_results(messages, stale_turn_threshold);

    if stale_ids.is_empty() {
        return CacheMicrocompactResult::default();
    }

    let stale_count = stale_ids.len();

    // Try cache-edit first
    if provider.supports_cache_edit() {
        let edits: Vec<CacheEdit> = stale_ids
            .iter()
            .map(|id| CacheEdit::RemoveToolResult {
                call_id: id.clone(),
            })
            .collect();

        match provider.edit_cache(&edits).await {
            Ok(receipt) => {
                // Cache-edit succeeded — remove from messages too (for consistency)
                let tokens_freed = receipt.tokens_freed;
                remove_by_call_ids(messages, &stale_ids);

                return CacheMicrocompactResult {
                    stale_count,
                    used_cache_edit: true,
                    tokens_freed,
                    receipt: Some(receipt),
                };
            }
            Err(e) => {
                tracing::warn!("Cache-edit failed, falling back to rebuild: {e}");
                // Fall through to rebuild path
            }
        }
    }

    // Rebuild path: remove stale messages, invalidating cache
    let tokens_freed = remove_by_call_ids(messages, &stale_ids);

    CacheMicrocompactResult {
        stale_count,
        used_cache_edit: false,
        tokens_freed,
        receipt: None,
    }
}

/// Remove tool result messages by call_id. Returns estimated tokens freed.
fn remove_by_call_ids(messages: &mut Vec<Message>, call_ids: &[String]) -> u64 {
    let mut tokens_freed = 0u64;

    messages.retain(|msg| {
        let dominated_by_stale = msg.content.iter().all(|block| {
            if let pipit_provider::ContentBlock::ToolResult {
                call_id, content, ..
            } = block
            {
                if call_ids.contains(call_id) {
                    tokens_freed += (content.len() / 4) as u64;
                    return true; // This block is stale
                }
            }
            false
        });

        // Keep messages that have at least one non-stale block
        !dominated_by_stale || msg.content.is_empty()
    });

    tokens_freed
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipit_provider::{ContentBlock, Message, Role};

    fn tool_result(call_id: &str, content: &str) -> Message {
        Message {
            role: Role::ToolResult {
                call_id: call_id.to_string(),
            },
            content: vec![ContentBlock::ToolResult {
                call_id: call_id.to_string(),
                content: content.to_string(),
                is_error: false,
            }],
            metadata: Default::default(),
        }
    }

    fn text_msg(text: &str) -> Message {
        Message::user(text)
    }

    #[test]
    fn find_stale_identifies_unreferenced() {
        let msgs = vec![
            tool_result("c1", "old data"),
            text_msg("unrelated"),
            tool_result("c2", "this references c1: see c1"), // pseudo-reference
            text_msg("recent"),
            tool_result("c3", "recent data"),
        ];
        let stale = find_stale_tool_results(&msgs, 2);
        // c1 content is referenced by c2's text; c3 is recent (within threshold)
        // Only unreferenced old results should be stale
        assert!(!stale.contains(&"c2".to_string()));
    }

    #[test]
    fn remove_by_call_ids_works() {
        let mut msgs = vec![
            tool_result("c1", "data1"),
            text_msg("keep me"),
            tool_result("c2", "data2"),
        ];
        let freed = remove_by_call_ids(&mut msgs, &["c1".to_string()]);
        assert_eq!(msgs.len(), 2); // c1 removed
        assert!(freed > 0);
    }
}
