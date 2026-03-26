use crate::ProviderError;
use pipit_config::RetryPolicy;
use std::future::Future;
use std::time::Duration;
use tracing::warn;

/// Execute an async operation with retry logic.
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

fn compute_backoff(policy: &RetryPolicy, attempt: u32) -> Duration {
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
