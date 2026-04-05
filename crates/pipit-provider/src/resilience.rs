//! Unified Resilience Controller
//!
//! Composes retry, circuit-breaker, and fallback into a single observable
//! state machine. Eliminates the gap where independent mechanisms fail
//! to coordinate transitions.
//!
//! State machine:
//! ```text
//! Normal → [retriable error] → Retrying(attempt)
//! Retrying(n) → [success] → Normal
//! Retrying(n) → [consecutive_overload ≥ 3] → FallbackTriggered
//! Retrying(n) → [max_retries] → CircuitCheck
//! CircuitCheck → [threshold exceeded] → CircuitOpen
//! CircuitOpen → [recovery_timeout] → CircuitHalfOpen
//! CircuitHalfOpen → [probe success] → Normal
//! FallbackTriggered → [fallback available] → Normal(fallback_provider)
//! FallbackTriggered → [exhausted] → Terminal
//! ```
//!
//! Memory: ~128 bytes per provider. O(1) per transition.

use crate::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};
use crate::fallback::{FallbackController, FallbackConfig, FallbackResult};
use crate::retry::{AdaptiveRetryPolicy, RetryContext, RetryEvent, compute_backoff};
use crate::{LlmProvider, ProviderError};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Observable events from the resilience controller.
#[derive(Debug, Clone)]
pub enum ResilienceEvent {
    /// A retry is being scheduled.
    RetryScheduled {
        attempt: u32,
        wait_ms: u64,
        error: String,
    },
    /// Circuit breaker opened (fast-fail mode).
    CircuitOpened {
        provider_id: String,
        failure_count: u32,
    },
    /// Circuit breaker is testing recovery.
    CircuitHalfOpen {
        provider_id: String,
    },
    /// Circuit breaker recovered.
    CircuitClosed {
        provider_id: String,
    },
    /// Model fallback triggered.
    FallbackTriggered {
        from_model: String,
        to_model: String,
    },
    /// All fallbacks exhausted.
    FallbackExhausted {
        error: String,
    },
    /// Persistent mode heartbeat.
    Heartbeat {
        total_wait_secs: u64,
    },
    /// Context overflow recovery (output tokens reduced).
    ContextReduced {
        new_max_tokens: u32,
    },
}

/// The current state of the resilience FSM.
#[derive(Debug, Clone, PartialEq)]
pub enum ResilienceState {
    /// Normal operation.
    Normal,
    /// Retrying after a transient error.
    Retrying { attempt: u32 },
    /// Circuit breaker is open — fast-fail.
    CircuitOpen,
    /// Probing after circuit breaker recovery timeout.
    CircuitHalfOpen,
    /// Fallback has been triggered — using alternate provider.
    FallbackActive { model: String },
    /// All recovery options exhausted.
    Terminal,
}

/// Configuration for the resilience controller.
#[derive(Clone)]
pub struct ResilienceConfig {
    pub retry: AdaptiveRetryPolicy,
    pub circuit_breaker: CircuitBreakerConfig,
    pub fallback: Option<FallbackConfig>,
}

impl std::fmt::Debug for ResilienceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResilienceConfig")
            .field("retry", &self.retry)
            .field("circuit_breaker", &self.circuit_breaker)
            .field("fallback", &self.fallback.is_some())
            .finish()
    }
}

impl Default for ResilienceConfig {
    fn default() -> Self {
        Self {
            retry: AdaptiveRetryPolicy::default(),
            circuit_breaker: CircuitBreakerConfig::default(),
            fallback: None,
        }
    }
}

/// Unified resilience controller composing retry, circuit-breaker, and fallback.
pub struct ResilienceController {
    config: ResilienceConfig,
    circuit_breaker: CircuitBreaker,
    fallback: Option<FallbackController>,
    state: ResilienceState,
    retry_ctx: RetryContext,
    event_tx: Option<mpsc::Sender<ResilienceEvent>>,
}

impl ResilienceController {
    pub fn new(
        provider_id: &str,
        config: ResilienceConfig,
    ) -> Self {
        let circuit_breaker = CircuitBreaker::new(provider_id, config.circuit_breaker.clone());
        let fallback = config.fallback.as_ref().map(|fc| FallbackController::new(fc.clone()));

        Self {
            config,
            circuit_breaker,
            fallback,
            state: ResilienceState::Normal,
            retry_ctx: RetryContext::default(),
            event_tx: None,
        }
    }

    /// Attach an event observer channel.
    pub fn with_events(mut self, tx: mpsc::Sender<ResilienceEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// Get the current FSM state.
    pub fn state(&self) -> &ResilienceState {
        &self.state
    }

    /// Get the current provider (may be a fallback).
    pub fn current_provider(&self) -> Option<&Arc<dyn LlmProvider>> {
        self.fallback.as_ref().map(|f| f.current_provider())
    }

    /// Get the retry context (for parameter adjustments like max_tokens override).
    pub fn retry_context(&self) -> &RetryContext {
        &self.retry_ctx
    }

    /// Execute an operation through the resilience pipeline.
    ///
    /// Handles retry, circuit-break, and fallback transitions automatically.
    pub async fn execute<F, Fut, T>(
        &mut self,
        operation: F,
    ) -> Result<T, ProviderError>
    where
        F: Fn(&RetryContext) -> Fut,
        Fut: std::future::Future<Output = Result<T, ProviderError>>,
    {
        let max_retries = self.config.retry.base.max_retries;

        for attempt in 0..=max_retries {
            // Check circuit breaker
            if let Err(CircuitState::Open) = self.circuit_breaker.check() {
                self.state = ResilienceState::CircuitOpen;
                self.emit(ResilienceEvent::CircuitOpened {
                    provider_id: self.circuit_breaker.provider_id.clone(),
                    failure_count: self.circuit_breaker.failure_count(),
                });

                // Try fallback
                if let Some(result) = self.try_fallback(ProviderError::Network(
                    "Circuit breaker open".to_string(),
                )) {
                    return Err(result);
                }

                return Err(ProviderError::Network("Circuit breaker open — all providers exhausted".to_string()));
            }

            if self.circuit_breaker.state() == CircuitState::HalfOpen {
                self.state = ResilienceState::CircuitHalfOpen;
                self.emit(ResilienceEvent::CircuitHalfOpen {
                    provider_id: self.circuit_breaker.provider_id.clone(),
                });
            }

            // Execute the operation
            match operation(&self.retry_ctx).await {
                Ok(result) => {
                    self.circuit_breaker.record_success();
                    self.retry_ctx.consecutive_overload = 0;
                    self.state = ResilienceState::Normal;

                    if self.circuit_breaker.state() == CircuitState::Closed {
                        self.emit(ResilienceEvent::CircuitClosed {
                            provider_id: self.circuit_breaker.provider_id.clone(),
                        });
                    }

                    return Ok(result);
                }
                Err(e) => {
                    self.circuit_breaker.record_failure();

                    // Context overflow: adjust tokens and retry
                    if e.is_context_recoverable() {
                        if let Some(reduced) = crate::retry::recover_from_context_overflow(
                            &e,
                            &self.config.retry,
                        ) {
                            self.retry_ctx.max_tokens_override = Some(reduced);
                            self.emit(ResilienceEvent::ContextReduced {
                                new_max_tokens: reduced,
                            });
                            continue;
                        }
                    }

                    // Permanent errors: don't retry
                    if e.is_permanent() {
                        return Err(e);
                    }

                    // Track overload for fallback trigger
                    if is_overload(&e) {
                        self.retry_ctx.consecutive_overload += 1;
                        if self.retry_ctx.consecutive_overload >= self.config.retry.max_overload_retries {
                            if let Some(err) = self.try_fallback(e) {
                                return Err(err);
                            }
                            // Fallback succeeded, continue with new provider
                            continue;
                        }
                    } else {
                        self.retry_ctx.consecutive_overload = 0;
                    }

                    // Transient errors: retry with backoff
                    if e.is_transient() && attempt < max_retries {
                        self.state = ResilienceState::Retrying { attempt: attempt + 1 };
                        let wait = compute_backoff(&self.config.retry.base, attempt);

                        self.emit(ResilienceEvent::RetryScheduled {
                            attempt: attempt + 1,
                            wait_ms: wait.as_millis() as u64,
                            error: e.to_string(),
                        });

                        tokio::time::sleep(wait).await;
                        continue;
                    }

                    return Err(e);
                }
            }
        }

        Err(ProviderError::Other("Resilience: max retries exceeded".to_string()))
    }

    /// Attempt fallback to next provider.
    fn try_fallback(&mut self, error: ProviderError) -> Option<ProviderError> {
        if let Some(ref mut fallback) = self.fallback {
            match fallback.attempt_fallback(error) {
                FallbackResult::Switched { from_model, to_model, .. } => {
                    self.state = ResilienceState::FallbackActive { model: to_model.clone() };
                    self.retry_ctx.consecutive_overload = 0;
                    self.circuit_breaker.reset();

                    self.emit(ResilienceEvent::FallbackTriggered {
                        from_model,
                        to_model,
                    });

                    None // Continue execution with new provider
                }
                FallbackResult::Exhausted { original_error } => {
                    self.state = ResilienceState::Terminal;
                    self.emit(ResilienceEvent::FallbackExhausted {
                        error: original_error.to_string(),
                    });
                    Some(original_error)
                }
                FallbackResult::NotRetriable { error } => {
                    Some(error)
                }
            }
        } else {
            Some(error)
        }
    }

    /// Reset for a new request (preserves fallback model selection).
    pub fn reset_for_new_request(&mut self) {
        self.retry_ctx = RetryContext::default();
        self.state = ResilienceState::Normal;
        if let Some(ref mut fallback) = self.fallback {
            fallback.reset_for_new_request();
        }
    }

    /// Full reset to primary model.
    pub fn reset_to_primary(&mut self) {
        self.retry_ctx = RetryContext::default();
        self.state = ResilienceState::Normal;
        self.circuit_breaker.reset();
        if let Some(ref mut fallback) = self.fallback {
            fallback.reset_to_primary();
        }
    }

    fn emit(&self, event: ResilienceEvent) {
        if let Some(ref tx) = self.event_tx {
            let _ = tx.try_send(event);
        }
    }
}

fn is_overload(error: &ProviderError) -> bool {
    matches!(error, ProviderError::RateLimited { .. })
        || matches!(error, ProviderError::Other(msg) if {
            let l = msg.to_ascii_lowercase();
            l.contains("529") || l.contains("overloaded") || l.contains("capacity")
        })
}

/// Make `recover_from_context_overflow` accessible from this module.
pub use crate::retry::recover_from_context_overflow;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state() {
        let ctrl = ResilienceController::new("test", ResilienceConfig::default());
        assert_eq!(*ctrl.state(), ResilienceState::Normal);
    }
}
