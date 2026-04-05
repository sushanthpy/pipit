use crate::ProviderError;
use pipit_config::RetryPolicy;
use std::future::Future;
use std::time::Duration;
use tracing::warn;

/// Events emitted during retry execution for observability.
#[derive(Debug, Clone)]
pub enum RetryEvent {
    /// A retry is scheduled after a transient error.
    RetryScheduled {
        attempt: u32,
        max_retries: u32,
        wait_ms: u64,
        error: String,
    },
    /// Context overflow detected — reducing output tokens.
    ContextOverflowRecovery {
        original_max_tokens: u32,
        reduced_max_tokens: u32,
    },
    /// Consecutive 429/529 errors exceeded threshold — triggering fallback.
    FallbackTriggered {
        consecutive_overload: u32,
        error: String,
    },
    /// Persistent mode heartbeat (keep-alive for unattended sessions).
    PersistentHeartbeat {
        total_wait_secs: u64,
    },
}

/// Extended retry policy with context-overflow recovery and persistent mode.
#[derive(Debug, Clone)]
pub struct AdaptiveRetryPolicy {
    /// Base retry policy.
    pub base: RetryPolicy,
    /// Maximum consecutive 529/429 errors before triggering fallback.
    pub max_overload_retries: u32,
    /// Enable persistent retry for unattended sessions (429/529 indefinite retry).
    pub persistent_mode: bool,
    /// Maximum persistent retry duration before giving up (seconds).
    pub persistent_max_duration_secs: u64,
    /// Persistent mode heartbeat interval (seconds).
    pub persistent_heartbeat_interval_secs: u64,
    /// Floor for output tokens after context-overflow reduction.
    pub floor_output_tokens: u32,
    /// Safety buffer subtracted from available context during overflow recovery.
    pub context_safety_buffer: u64,
}

impl Default for AdaptiveRetryPolicy {
    fn default() -> Self {
        Self {
            base: RetryPolicy::default(),
            max_overload_retries: 3,
            persistent_mode: false,
            persistent_max_duration_secs: 6 * 3600, // 6 hours
            persistent_heartbeat_interval_secs: 30,
            floor_output_tokens: 1024,
            context_safety_buffer: 1000,
        }
    }
}

/// Mutable context passed through the retry loop for parameter adjustment.
#[derive(Debug, Clone)]
pub struct RetryContext {
    /// Override for max output tokens (set during context-overflow recovery).
    pub max_tokens_override: Option<u32>,
    /// Total consecutive overload errors (429/529).
    pub consecutive_overload: u32,
}

impl Default for RetryContext {
    fn default() -> Self {
        Self {
            max_tokens_override: None,
            consecutive_overload: 0,
        }
    }
}

/// Execute an async operation with adaptive retry logic.
///
/// Handles: transient errors (backoff + retry), context overflow (reduce output tokens),
/// consecutive overload (fallback trigger), and persistent mode (indefinite retry with heartbeat).
pub async fn with_retry<F, Fut, T>(
    policy: &RetryPolicy,
    operation: F,
) -> Result<T, ProviderError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, ProviderError>>,
{
    let mut last_error = None;

    for attempt in 0..=policy.max_retries {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                let should_retry = match &e {
                    ProviderError::RateLimited { .. } => true,
                    ProviderError::Network(_) => true,
                    ProviderError::Other(msg) => {
                        policy.retryable_statuses.iter().any(|s| msg.contains(&s.to_string()))
                    }
                    _ => false,
                };

                if !should_retry || attempt == policy.max_retries {
                    return Err(e);
                }

                let backoff = compute_backoff(policy, attempt);

                // Check for Retry-After in rate limit errors
                let wait = match &e {
                    ProviderError::RateLimited {
                        retry_after_ms: Some(ms),
                    } => Duration::from_millis(*ms),
                    _ => backoff,
                };

                warn!(
                    attempt = attempt + 1,
                    max = policy.max_retries,
                    wait_ms = wait.as_millis() as u64,
                    "Retrying after error: {}",
                    e
                );

                tokio::time::sleep(wait).await;
                last_error = Some(e);
            }
        }
    }

    Err(last_error.unwrap_or(ProviderError::Other("Max retries exceeded".to_string())))
}

/// Adaptive retry with context-overflow recovery, overload tracking, and event emission.
pub async fn with_adaptive_retry<F, Fut, T>(
    policy: &AdaptiveRetryPolicy,
    mut event_sink: impl FnMut(RetryEvent),
    operation: F,
) -> Result<(T, RetryContext), ProviderError>
where
    F: Fn(&RetryContext) -> Fut,
    Fut: Future<Output = Result<T, ProviderError>>,
{
    let mut ctx = RetryContext::default();
    let mut last_error = None;
    let start = std::time::Instant::now();

    for attempt in 0..=policy.base.max_retries {
        match operation(&ctx).await {
            Ok(result) => {
                ctx.consecutive_overload = 0;
                return Ok((result, ctx));
            }
            Err(e) => {
                // Context overflow: parse and reduce output tokens
                if e.is_context_recoverable() {
                    if let Some(reduced) = recover_from_context_overflow(&e, policy) {
                        let original = ctx.max_tokens_override.unwrap_or(policy.base.max_retries * 1000);
                        ctx.max_tokens_override = Some(reduced);
                        event_sink(RetryEvent::ContextOverflowRecovery {
                            original_max_tokens: original,
                            reduced_max_tokens: reduced,
                        });
                        last_error = Some(e);
                        continue;
                    }
                }

                // Track consecutive overload (429/529)
                if is_overload_error(&e) {
                    ctx.consecutive_overload += 1;
                    if ctx.consecutive_overload >= policy.max_overload_retries {
                        event_sink(RetryEvent::FallbackTriggered {
                            consecutive_overload: ctx.consecutive_overload,
                            error: e.to_string(),
                        });
                        return Err(e);
                    }
                } else {
                    ctx.consecutive_overload = 0;
                }

                // Standard transient retry check
                let should_retry = e.is_transient() || is_overload_error(&e);

                if !should_retry {
                    return Err(e);
                }

                // Persistent mode: retry indefinitely with heartbeat
                if policy.persistent_mode && is_overload_error(&e) {
                    let elapsed = start.elapsed().as_secs();
                    if elapsed < policy.persistent_max_duration_secs {
                        let wait = compute_backoff(&policy.base, attempt)
                            .min(Duration::from_secs(300)); // cap at 5min
                        event_sink(RetryEvent::PersistentHeartbeat {
                            total_wait_secs: elapsed,
                        });
                        tokio::time::sleep(wait).await;
                        last_error = Some(e);
                        continue;
                    }
                }

                if attempt == policy.base.max_retries {
                    return Err(e);
                }

                let backoff = compute_backoff(&policy.base, attempt);
                let wait = match &e {
                    ProviderError::RateLimited { retry_after_ms: Some(ms) } => Duration::from_millis(*ms),
                    _ => backoff,
                };

                event_sink(RetryEvent::RetryScheduled {
                    attempt: attempt + 1,
                    max_retries: policy.base.max_retries,
                    wait_ms: wait.as_millis() as u64,
                    error: e.to_string(),
                });

                tokio::time::sleep(wait).await;
                last_error = Some(e);
            }
        }
    }

    Err(last_error.unwrap_or(ProviderError::Other("Max retries exceeded".to_string())))
}

/// Attempt to recover from context overflow by reducing output tokens.
pub fn recover_from_context_overflow(
    error: &ProviderError,
    policy: &AdaptiveRetryPolicy,
) -> Option<u32> {
    // Try to extract context limit from the error message
    let msg = error.to_string();
    let lower = msg.to_ascii_lowercase();

    // Parse pattern: "maximum context length is N tokens"
    if let Some(pos) = lower.find("maximum context length is ") {
        let after = &msg[pos + 25..];
        if let Some(limit_str) = after.split_whitespace().next() {
            if let Ok(limit) = limit_str.parse::<u64>() {
                let safety = policy.context_safety_buffer;
                let available = limit.saturating_sub(safety);
                let reduced = (available as u32).max(policy.floor_output_tokens);
                return Some(reduced);
            }
        }
    }

    // If we can't parse, just halve the current max tokens
    Some(policy.floor_output_tokens)
}

fn is_overload_error(error: &ProviderError) -> bool {
    matches!(error, ProviderError::RateLimited { .. })
        || matches!(error, ProviderError::Other(msg) if {
            let lower = msg.to_ascii_lowercase();
            lower.contains("529") || lower.contains("overloaded") || lower.contains("capacity")
        })
}

pub fn compute_backoff(policy: &RetryPolicy, attempt: u32) -> Duration {
    let base = policy.initial_backoff.as_millis() as f64;
    let multiplied = base * policy.backoff_multiplier.powi(attempt as i32);
    let capped = multiplied.min(policy.max_backoff.as_millis() as f64);

    // Add jitter: ±25%
    let jitter = 1.0 + (rand_simple() - 0.5) * 0.5;
    Duration::from_millis((capped * jitter) as u64)
}

/// Simple deterministic-enough jitter without pulling in a PRNG crate.
fn rand_simple() -> f64 {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    (nanos % 1000) as f64 / 1000.0
}
