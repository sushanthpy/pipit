//! Turn Continuation Protocol
//!
//! Extends the reactive recovery controller into a full continuation protocol.
//! When a model response or tool output overflows the budget, instead of
//! discarding work, the protocol:
//! 1. Persists compacted state
//! 2. Summarizes oversized artifacts into the blob store
//! 3. Re-enters the turn with a canonical continuation marker
//!
//! This converts hard failures into bounded-state continuations.
//!
//! Complexity: Recovery automaton transitions are O(1). Context reduction
//! from O(n) raw history to O(s + r) where s = snapshot summary, r = recent.
//! Blob indirection: O(1) reference insertion vs O(L) repeated prompt injection.

use crate::blob_store::{BlobDescriptor, BlobStore, BlobStoreError};
use crate::reactive::{RecoveryAction, RecoveryController, RecoveryErrorKind};
use pipit_context::budget::{CompressionStats, ContextManager};
use pipit_provider::ProviderError;
use serde::{Deserialize, Serialize};

/// A continuation marker injected into context when a turn is resumed
/// after overflow recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuationMarker {
    /// Turn number where the overflow occurred.
    pub turn: u32,
    /// What kind of overflow triggered continuation.
    pub trigger: ContinuationTrigger,
    /// References to blob-stored artifacts from the interrupted turn.
    pub blob_refs: Vec<BlobRef>,
    /// Summary of what was compacted/discarded.
    pub compaction_summary: String,
    /// Tokens freed by the continuation recovery.
    pub tokens_freed: u64,
}

/// What triggered the continuation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContinuationTrigger {
    /// Model response exceeded max_tokens.
    OutputTruncated,
    /// Total context exceeded model limit.
    ContextOverflow,
    /// Tool result was too large for inline context.
    ToolResultOverflow { tool_name: String, call_id: String },
}

/// Reference to a blob-stored artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobRef {
    /// Content hash in the blob store.
    pub hash: String,
    /// Human-readable summary.
    pub summary: String,
    /// Original size in bytes.
    pub original_size: usize,
}

/// Actions the continuation protocol produces.
#[derive(Debug)]
pub enum ContinuationAction {
    /// No continuation needed — normal flow.
    None,
    /// Retry the same turn after blob-storing oversized content.
    RetryWithBlobs {
        marker: ContinuationMarker,
        nudge_message: String,
    },
    /// Compact context and retry with reduced history.
    CompactAndRetry { marker: ContinuationMarker },
    /// Recovery exhausted — surface error.
    GiveUp { reason: String },
}

/// The continuation controller orchestrates recovery from context overflow.
///
/// Wraps the existing `RecoveryController` and adds blob-store integration
/// for oversized content indirection.
pub struct ContinuationController {
    recovery: RecoveryController,
    /// Number of continuation attempts in current turn.
    continuation_count: u32,
    /// Maximum continuations per turn.
    max_continuations: u32,
}

impl ContinuationController {
    pub fn new() -> Self {
        Self {
            recovery: RecoveryController::new(),
            continuation_count: 0,
            max_continuations: 3,
        }
    }

    /// Handle an error that may trigger continuation recovery.
    ///
    /// Returns a `ContinuationAction` that the agent loop should execute.
    pub fn handle_error(
        &mut self,
        error: &ProviderError,
        turn: u32,
        blob_store: Option<&mut BlobStore>,
    ) -> ContinuationAction {
        // Check if the recovery controller recognizes this error
        let error_kind = RecoveryController::classify_error(error);
        let Some(kind) = error_kind else {
            return ContinuationAction::None;
        };

        // Enforce continuation limit
        if self.continuation_count >= self.max_continuations {
            return ContinuationAction::GiveUp {
                reason: format!(
                    "Max {} continuations exceeded for turn {}",
                    self.max_continuations, turn
                ),
            };
        }

        self.continuation_count += 1;

        match kind {
            RecoveryErrorKind::OutputTruncated => {
                let marker = ContinuationMarker {
                    turn,
                    trigger: ContinuationTrigger::OutputTruncated,
                    blob_refs: Vec::new(),
                    compaction_summary: "Output was truncated at max_tokens".to_string(),
                    tokens_freed: 0,
                };

                ContinuationAction::RetryWithBlobs {
                    marker,
                    nudge_message: "[System: Your previous response was truncated. Please continue from where you left off.]".to_string(),
                }
            }
            RecoveryErrorKind::PromptTooLong | RecoveryErrorKind::ContentTooLarge => {
                let marker = ContinuationMarker {
                    turn,
                    trigger: ContinuationTrigger::ContextOverflow,
                    blob_refs: Vec::new(),
                    compaction_summary: format!(
                        "Context overflow recovery (attempt {})",
                        self.continuation_count
                    ),
                    tokens_freed: 0,
                };

                ContinuationAction::CompactAndRetry { marker }
            }
        }
    }

    /// Store an oversized tool result in the blob store, returning a compact reference.
    pub fn store_oversized_result(
        blob_store: &mut BlobStore,
        content: &str,
        tool_name: &str,
        call_id: &str,
        summary: &str,
    ) -> Result<Option<BlobRef>, BlobStoreError> {
        let descriptor = blob_store.store_if_large(content, tool_name, call_id, summary)?;

        Ok(descriptor.map(|d| BlobRef {
            hash: d.hash.clone(),
            summary: d.summary.clone(),
            original_size: d.size,
        }))
    }

    /// Format a continuation marker as a context message for the model.
    pub fn format_continuation_context(marker: &ContinuationMarker) -> String {
        let mut parts = Vec::new();

        parts.push(format!(
            "[Continuation: Turn {} recovered from {:?}]",
            marker.turn, marker.trigger
        ));

        if !marker.blob_refs.is_empty() {
            parts.push(format!(
                "  {} large results stored externally:",
                marker.blob_refs.len()
            ));
            for blob in &marker.blob_refs {
                parts.push(format!(
                    "    - {} ({}B): {}",
                    blob.hash, blob.original_size, blob.summary
                ));
            }
        }

        if marker.tokens_freed > 0 {
            parts.push(format!(
                "  {} tokens freed by compaction",
                marker.tokens_freed
            ));
        }

        parts.join("\n")
    }

    /// Reset continuation state for a new turn.
    pub fn reset_for_new_turn(&mut self) {
        self.continuation_count = 0;
    }

    /// Current continuation count in this turn.
    pub fn continuation_count(&self) -> u32 {
        self.continuation_count
    }
}

impl Default for ContinuationController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_continuation_limit() {
        let mut ctrl = ContinuationController::new();
        ctrl.max_continuations = 2;

        // First two should produce actions
        let err = ProviderError::OutputTruncated;
        let action = ctrl.handle_error(&err, 1, None);
        assert!(matches!(action, ContinuationAction::RetryWithBlobs { .. }));

        let action = ctrl.handle_error(&err, 1, None);
        assert!(matches!(action, ContinuationAction::RetryWithBlobs { .. }));

        // Third should give up
        let action = ctrl.handle_error(&err, 1, None);
        assert!(matches!(action, ContinuationAction::GiveUp { .. }));
    }

    #[test]
    fn test_reset_for_new_turn() {
        let mut ctrl = ContinuationController::new();
        ctrl.continuation_count = 3;
        ctrl.reset_for_new_turn();
        assert_eq!(ctrl.continuation_count(), 0);
    }

    #[test]
    fn test_format_continuation_context() {
        let marker = ContinuationMarker {
            turn: 5,
            trigger: ContinuationTrigger::ContextOverflow,
            blob_refs: vec![BlobRef {
                hash: "abc123".into(),
                summary: "test output".into(),
                original_size: 50000,
            }],
            compaction_summary: "Compacted old messages".into(),
            tokens_freed: 10000,
        };

        let text = ContinuationController::format_continuation_context(&marker);
        assert!(text.contains("Continuation: Turn 5"));
        assert!(text.contains("abc123"));
        assert!(text.contains("10000 tokens freed"));
    }
}
