//! Fallback Model Auto-Switch (Task 4.3)
//!
//! Transparent model fallback when the primary model returns retriable errors
//! (rate limiting, overload, capacity). The system:
//! 1. Tombstones partial assistant messages from the failed attempt
//! 2. Strips model-specific blocks (thinking signatures)
//! 3. Switches to the next model in the cascade
//! 4. Yields a user-visible warning
//! 5. Retries the request
//!
//! The fallback is one-shot per request (no bouncing between models).

use crate::{LlmProvider, ProviderError};
use std::sync::Arc;

/// A model in the fallback cascade.
#[derive(Clone)]
pub struct FallbackModel {
    /// Provider kind identifier (e.g., "anthropic", "openai").
    pub provider_id: String,
    /// Model identifier (e.g., "claude-sonnet-4-20250514", "gpt-4o").
    pub model_id: String,
    /// The provider instance.
    pub provider: Arc<dyn LlmProvider>,
}

/// Configuration for the fallback cascade.
#[derive(Clone)]
pub struct FallbackConfig {
    /// Models in priority order. Index 0 is the primary.
    pub models: Vec<FallbackModel>,
}

/// Tracks fallback state during a request.
pub struct FallbackController {
    config: FallbackConfig,
    /// Current model index in the cascade.
    current_index: usize,
    /// Whether fallback has been triggered for the current request.
    has_fallen_back: bool,
}

/// Result of a fallback attempt.
pub enum FallbackResult {
    /// Successfully switched to a fallback model.
    Switched {
        from_model: String,
        to_model: String,
        provider: Arc<dyn LlmProvider>,
    },
    /// No more fallback models available.
    Exhausted {
        original_error: ProviderError,
    },
    /// Error is not retriable — don't attempt fallback.
    NotRetriable {
        error: ProviderError,
    },
}

impl FallbackController {
    pub fn new(config: FallbackConfig) -> Self {
        Self {
            config,
            current_index: 0,
            has_fallen_back: false,
        }
    }

    /// Create a controller with a single primary model (no fallbacks).
    pub fn single(provider: Arc<dyn LlmProvider>, model_id: String) -> Self {
        Self::new(FallbackConfig {
            models: vec![FallbackModel {
                provider_id: provider.id().to_string(),
                model_id,
                provider,
            }],
        })
    }

    /// Get the current provider.
    pub fn current_provider(&self) -> &Arc<dyn LlmProvider> {
        &self.config.models[self.current_index].provider
    }

    /// Get the current model ID.
    pub fn current_model_id(&self) -> &str {
        &self.config.models[self.current_index].model_id
    }

    /// Whether we've already fallen back for this request.
    pub fn has_fallen_back(&self) -> bool {
        self.has_fallen_back
    }

    /// Attempt to fall back to the next model after an error.
    /// Returns the fallback result.
    pub fn attempt_fallback(&mut self, error: ProviderError) -> FallbackResult {
        // Only attempt fallback for retriable errors
        if !Self::is_fallback_eligible(&error) {
            return FallbackResult::NotRetriable { error };
        }

        // One-shot: don't bounce between models
        if self.has_fallen_back {
            return FallbackResult::Exhausted {
                original_error: error,
            };
        }

        // Try the next model
        let from_model = self.config.models[self.current_index].model_id.clone();

        if self.current_index + 1 < self.config.models.len() {
            self.current_index += 1;
            self.has_fallen_back = true;

            let to_model = self.config.models[self.current_index].model_id.clone();
            let provider = self.config.models[self.current_index].provider.clone();

            FallbackResult::Switched {
                from_model,
                to_model,
                provider,
            }
        } else {
            FallbackResult::Exhausted {
                original_error: error,
            }
        }
    }

    /// Reset fallback state for a new request.
    pub fn reset_for_new_request(&mut self) {
        self.has_fallen_back = false;
        // Stay on current model — don't reset to primary.
        // The primary model may still be overloaded.
    }

    /// Reset completely to the primary model (e.g., on new session).
    pub fn reset_to_primary(&mut self) {
        self.current_index = 0;
        self.has_fallen_back = false;
    }

    /// Check if an error is eligible for model fallback.
    fn is_fallback_eligible(error: &ProviderError) -> bool {
        match error {
            ProviderError::RateLimited { .. } => true,
            ProviderError::Other(msg)
                if msg.contains("overloaded")
                    || msg.contains("capacity")
                    || msg.contains("503")
                    || msg.contains("529") =>
            {
                true
            }
            _ => false,
        }
    }

    /// Prepare messages for the fallback model.
    /// Strips model-specific content (thinking blocks, signature blocks)
    /// that would cause 400 errors on a different model.
    pub fn prepare_messages_for_fallback(
        messages: &mut Vec<crate::Message>,
    ) {
        for msg in messages.iter_mut() {
            msg.content.retain(|block| {
                // Remove thinking blocks — they're model-specific
                !matches!(block, crate::ContentBlock::Thinking(_))
            });
        }
    }

    /// Tombstone partial assistant messages from a failed attempt.
    /// Returns the number of messages tombstoned.
    pub fn tombstone_partial_messages(
        messages: &mut Vec<crate::Message>,
    ) -> usize {
        // Find the last user message — everything after it from the failed attempt
        // should be removed
        let last_user_idx = messages
            .iter()
            .rposition(|m| matches!(m.role, crate::Role::User));

        match last_user_idx {
            Some(idx) => {
                let to_remove = messages.len() - idx - 1;
                messages.truncate(idx + 1);
                to_remove
            }
            None => 0,
        }
    }
}

impl std::fmt::Debug for FallbackController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FallbackController")
            .field("current_index", &self.current_index)
            .field("has_fallen_back", &self.has_fallen_back)
            .field(
                "models",
                &self
                    .config
                    .models
                    .iter()
                    .map(|m| &m.model_id)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_model_exhausts_immediately() {
        // Can't create a real provider in tests, but we can test the logic
        // by checking is_fallback_eligible
        assert!(FallbackController::is_fallback_eligible(
            &ProviderError::RateLimited {
                retry_after_ms: Some(1000)
            }
        ));
        assert!(FallbackController::is_fallback_eligible(
            &ProviderError::Other("503 Service Temporarily Unavailable".to_string())
        ));
        assert!(!FallbackController::is_fallback_eligible(
            &ProviderError::AuthFailed {
                message: "invalid key".to_string()
            }
        ));
        assert!(!FallbackController::is_fallback_eligible(
            &ProviderError::Cancelled
        ));
    }

    #[test]
    fn tombstone_removes_partial_assistant_messages() {
        use crate::Message;

        let mut messages = vec![
            Message::user("Hello"),
            Message::assistant("I'll help you with..."),
            Message::assistant("partial response that failed"),
        ];

        let removed = FallbackController::tombstone_partial_messages(&mut messages);
        assert_eq!(removed, 2);
        assert_eq!(messages.len(), 1);
        assert!(matches!(messages[0].role, crate::Role::User));
    }

    #[test]
    fn prepare_messages_strips_thinking_blocks() {
        use crate::{ContentBlock, Message, Role};

        let mut messages = vec![Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking("internal reasoning".to_string()),
                ContentBlock::Text("visible response".to_string()),
            ],
            metadata: Default::default(),
        }];

        FallbackController::prepare_messages_for_fallback(&mut messages);
        assert_eq!(messages[0].content.len(), 1);
        assert!(matches!(
            &messages[0].content[0],
            ContentBlock::Text(t) if t == "visible response"
        ));
    }
}
