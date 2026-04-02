//! Reactive Compaction on API Error Recovery
//!
//! A state machine for error recovery with withholding semantics,
//! escalation tiers, and idempotent retry. Handles:
//! - prompt-too-long (413) errors via multi-stage recovery
//! - output truncation via max_output_tokens escalation
//! - withholding errors from the stream until recovery is attempted

use crate::events::AgentEvent;
use pipit_context::budget::CompressionStats;
use pipit_context::ContextManager;
use pipit_provider::{LlmProvider, ProviderError};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Recovery state machine states.
/// Transitions form a DAG: Normal → Withheld → CollapseDrain/ReactiveCompact → Normal | Exhausted
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryState {
    /// Normal operation — no error pending.
    Normal,
    /// An error has been received but withheld from the caller.
    /// Recovery will be attempted before surfacing it.
    Withheld {
        error: String,
        error_kind: RecoveryErrorKind,
    },
    /// Draining staged context collapses as first recovery attempt.
    CollapseDrain,
    /// Full emergency summarization as second recovery attempt.
    ReactiveCompact,
    /// All recovery attempts exhausted — error will be surfaced.
    Exhausted {
        original_error: String,
    },
}

/// Classifies errors for recovery routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryErrorKind {
    /// HTTP 413 or context overflow — needs context reduction.
    PromptTooLong,
    /// Output was truncated at max_tokens — needs escalation.
    OutputTruncated,
    /// Media/content size error — needs content reduction.
    ContentTooLarge,
}

/// Output token escalation tiers.
const OUTPUT_ESCALATION_TIERS: &[u32] = &[8192, 16384, 32768, 65536];

/// Max recovery attempts for output truncation with meta-messages.
const MAX_META_RECOVERY: u32 = 3;

/// The reactive recovery controller.
pub struct RecoveryController {
    state: RecoveryState,
    /// Whether reactive compact has been attempted this session.
    has_attempted_reactive_compact: bool,
    /// Current output token tier index.
    output_tier_index: usize,
    /// Number of meta-message retries for output truncation.
    meta_recovery_count: u32,
}

impl RecoveryController {
    pub fn new() -> Self {
        Self {
            state: RecoveryState::Normal,
            has_attempted_reactive_compact: false,
            output_tier_index: 0,
            meta_recovery_count: 0,
        }
    }

    /// Current state.
    pub fn state(&self) -> &RecoveryState {
        &self.state
    }

    /// Returns true if we're in a recovery state (not Normal).
    pub fn is_recovering(&self) -> bool {
        !matches!(self.state, RecoveryState::Normal)
    }

    /// Classify a provider error for recovery routing.
    pub fn classify_error(error: &ProviderError) -> Option<RecoveryErrorKind> {
        match error {
            ProviderError::ContextOverflow { .. } => Some(RecoveryErrorKind::PromptTooLong),
            ProviderError::RequestTooLarge { .. } => Some(RecoveryErrorKind::PromptTooLong),
            ProviderError::OutputTruncated => Some(RecoveryErrorKind::OutputTruncated),
            ProviderError::Other(msg) => {
                if msg.contains("413") || msg.contains("payload too large") || msg.contains("Payload Too Large") {
                    Some(RecoveryErrorKind::PromptTooLong)
                } else if msg.contains("max_tokens") || msg.contains("output_tokens") {
                    Some(RecoveryErrorKind::OutputTruncated)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Withhold an error and begin recovery. Returns true if recovery will be attempted.
    pub fn withhold_error(&mut self, error: &ProviderError) -> bool {
        match Self::classify_error(error) {
            Some(kind) => {
                self.state = RecoveryState::Withheld {
                    error: error.to_string(),
                    error_kind: kind,
                };
                true
            }
            None => false,
        }
    }

    /// Attempt recovery. Returns the recovery action to take.
    pub fn next_recovery_action(&mut self) -> RecoveryAction {
        match &self.state {
            RecoveryState::Normal => RecoveryAction::None,

            RecoveryState::Withheld { error_kind, .. } => {
                match error_kind {
                    RecoveryErrorKind::PromptTooLong => {
                        // First: try draining collapses
                        self.state = RecoveryState::CollapseDrain;
                        RecoveryAction::DrainCollapses
                    }
                    RecoveryErrorKind::OutputTruncated => {
                        // Escalate output tokens
                        if self.output_tier_index < OUTPUT_ESCALATION_TIERS.len() - 1 {
                            self.output_tier_index += 1;
                            let new_limit = OUTPUT_ESCALATION_TIERS[self.output_tier_index];
                            self.state = RecoveryState::Normal;
                            RecoveryAction::EscalateOutputTokens(new_limit)
                        } else if self.meta_recovery_count < MAX_META_RECOVERY {
                            self.meta_recovery_count += 1;
                            self.state = RecoveryState::Normal;
                            RecoveryAction::InjectMetaMessage
                        } else {
                            let error = format!(
                                "Output token escalation exhausted after {} tiers and {} meta retries",
                                OUTPUT_ESCALATION_TIERS.len(),
                                self.meta_recovery_count,
                            );
                            self.state = RecoveryState::Exhausted {
                                original_error: error.clone(),
                            };
                            RecoveryAction::GiveUp(error)
                        }
                    }
                    RecoveryErrorKind::ContentTooLarge => {
                        // For content size errors: attempt reactive compact
                        if !self.has_attempted_reactive_compact {
                            self.state = RecoveryState::ReactiveCompact;
                            RecoveryAction::ReactiveCompact
                        } else {
                            let error = "Content too large, reactive compact already attempted".to_string();
                            self.state = RecoveryState::Exhausted {
                                original_error: error.clone(),
                            };
                            RecoveryAction::GiveUp(error)
                        }
                    }
                }
            }

            RecoveryState::CollapseDrain => {
                // Collapse drain didn't free enough; try reactive compact
                if !self.has_attempted_reactive_compact {
                    self.state = RecoveryState::ReactiveCompact;
                    RecoveryAction::ReactiveCompact
                } else {
                    let error =
                        "Context overflow recovery exhausted (collapse drain + reactive compact)"
                            .to_string();
                    self.state = RecoveryState::Exhausted {
                        original_error: error.clone(),
                    };
                    RecoveryAction::GiveUp(error)
                }
            }

            RecoveryState::ReactiveCompact => {
                // Reactive compact already tried — give up
                let error = "Context overflow recovery exhausted after reactive compact".to_string();
                self.state = RecoveryState::Exhausted {
                    original_error: error.clone(),
                };
                RecoveryAction::GiveUp(error)
            }

            RecoveryState::Exhausted { original_error } => {
                RecoveryAction::GiveUp(original_error.clone())
            }
        }
    }

    /// Mark recovery as successful (transition back to Normal).
    pub fn recovery_succeeded(&mut self) {
        self.state = RecoveryState::Normal;
    }

    /// Mark that reactive compact was attempted.
    pub fn mark_reactive_compact_attempted(&mut self) {
        self.has_attempted_reactive_compact = true;
    }

    /// Execute reactive compact on the context manager.
    /// This is a full emergency summarization.
    pub async fn execute_reactive_compact(
        &mut self,
        context: &mut ContextManager,
        provider: &dyn LlmProvider,
        cancel: CancellationToken,
        event_tx: &broadcast::Sender<AgentEvent>,
    ) -> Result<CompressionStats, String> {
        self.has_attempted_reactive_compact = true;

        let _ = event_tx.send(AgentEvent::CompressionStart);

        // Force a more aggressive compress: temporarily reduce preserve_recent
        let stats = context
            .compress(provider, cancel)
            .await
            .map_err(|e| format!("Reactive compact failed: {}", e))?;

        let _ = event_tx.send(AgentEvent::CompressionEnd {
            messages_removed: stats.messages_removed,
            tokens_freed: stats.tokens_freed,
        });

        if stats.tokens_freed > 0 {
            self.state = RecoveryState::Normal;
        }

        Ok(stats)
    }

    /// Get the current output token limit based on escalation state.
    pub fn current_output_limit(&self) -> u32 {
        OUTPUT_ESCALATION_TIERS[self.output_tier_index]
    }

    /// Generate a meta-message for output truncation recovery.
    pub fn meta_recovery_message() -> String {
        "Output token limit hit. Resume directly from where you stopped. \
         Do not repeat previous content. Continue the implementation."
            .to_string()
    }

    /// Reset recovery state (e.g., on new user message).
    pub fn reset(&mut self) {
        self.state = RecoveryState::Normal;
        // Don't reset has_attempted_reactive_compact — that persists per session
        self.meta_recovery_count = 0;
    }
}

/// Actions the caller should take based on recovery state.
#[derive(Debug, Clone)]
pub enum RecoveryAction {
    /// No recovery needed.
    None,
    /// Drain staged context collapses, then retry.
    DrainCollapses,
    /// Execute a full reactive compact (emergency summarization), then retry.
    ReactiveCompact,
    /// Escalate max_output_tokens to the given value, then retry.
    EscalateOutputTokens(u32),
    /// Inject a meta-message asking the model to continue, then retry.
    InjectMetaMessage,
    /// All recovery exhausted — surface the error.
    GiveUp(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_controller_escalates_through_tiers() {
        let mut ctrl = RecoveryController::new();

        // Initial state
        assert!(matches!(ctrl.state(), RecoveryState::Normal));
        assert_eq!(ctrl.current_output_limit(), 8192);

        // Withhold an output truncation error
        let error = ProviderError::OutputTruncated;
        assert!(ctrl.withhold_error(&error));
        assert!(ctrl.is_recovering());

        // First recovery: escalate output tokens
        let action = ctrl.next_recovery_action();
        assert!(matches!(action, RecoveryAction::EscalateOutputTokens(16384)));
        assert_eq!(ctrl.current_output_limit(), 16384);
    }

    #[test]
    fn recovery_controller_handles_prompt_too_long() {
        let mut ctrl = RecoveryController::new();

        let error = ProviderError::ContextOverflow {
            used: 210000,
            limit: 200000,
        };
        assert!(ctrl.withhold_error(&error));

        // First: drain collapses
        let action = ctrl.next_recovery_action();
        assert!(matches!(action, RecoveryAction::DrainCollapses));

        // If that fails: reactive compact
        let action = ctrl.next_recovery_action();
        assert!(matches!(action, RecoveryAction::ReactiveCompact));

        // If that fails: give up
        let action = ctrl.next_recovery_action();
        assert!(matches!(action, RecoveryAction::GiveUp(_)));
    }

    #[test]
    fn recovery_prevents_infinite_reactive_compact() {
        let mut ctrl = RecoveryController::new();
        ctrl.mark_reactive_compact_attempted();

        let error = ProviderError::ContextOverflow {
            used: 210000,
            limit: 200000,
        };
        ctrl.withhold_error(&error);

        // Drain collapses first
        let action = ctrl.next_recovery_action();
        assert!(matches!(action, RecoveryAction::DrainCollapses));

        // Since reactive was already attempted, should give up
        let action = ctrl.next_recovery_action();
        assert!(matches!(action, RecoveryAction::GiveUp(_)));
    }
}
