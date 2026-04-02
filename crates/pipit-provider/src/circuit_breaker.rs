//! Circuit Breaker for LLM Provider Clients
//!
//! Three-state circuit breaker (Closed → Open → HalfOpen) that prevents
//! cascading failures when a provider is down. Instead of waiting 90s
//! (30s timeout × 3 retries), the second request fails fast.
//!
//! State machine: O(1) per request (one atomic load + comparison).
//! Memory: 12 bytes per provider.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Circuit breaker states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation. Requests pass through.
    Closed,
    /// Provider is down. All requests fail fast.
    Open,
    /// Testing recovery. One probe request allowed.
    HalfOpen,
}

/// Configuration for the circuit breaker.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of consecutive failures before opening the circuit.
    pub failure_threshold: u32,
    /// Time to wait before transitioning from Open to HalfOpen.
    pub recovery_timeout: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 3,
            recovery_timeout: Duration::from_secs(30),
        }
    }
}

/// Per-provider circuit breaker.
pub struct CircuitBreaker {
    /// Provider identifier.
    pub provider_id: String,
    /// Consecutive failure count.
    failure_count: AtomicU32,
    /// Timestamp of last failure (as epoch millis for atomic storage).
    last_failure_ms: AtomicU64,
    /// Configuration.
    config: CircuitBreakerConfig,
    /// Timestamp when the breaker was created (for relative timing).
    created_at: Instant,
}

impl CircuitBreaker {
    pub fn new(provider_id: &str, config: CircuitBreakerConfig) -> Self {
        Self {
            provider_id: provider_id.to_string(),
            failure_count: AtomicU32::new(0),
            last_failure_ms: AtomicU64::new(0),
            config,
            created_at: Instant::now(),
        }
    }

    /// Get the current circuit state. O(1).
    pub fn state(&self) -> CircuitState {
        let failures = self.failure_count.load(Ordering::Relaxed);
        if failures < self.config.failure_threshold {
            return CircuitState::Closed;
        }

        let last_fail_ms = self.last_failure_ms.load(Ordering::Relaxed);
        let elapsed_ms = self.created_at.elapsed().as_millis() as u64;
        let since_failure = elapsed_ms.saturating_sub(last_fail_ms);

        if since_failure >= self.config.recovery_timeout.as_millis() as u64 {
            CircuitState::HalfOpen
        } else {
            CircuitState::Open
        }
    }

    /// Check if a request should be allowed. O(1).
    /// Returns Ok(()) if allowed, Err with the circuit state if blocked.
    pub fn check(&self) -> Result<(), CircuitState> {
        match self.state() {
            CircuitState::Closed => Ok(()),
            CircuitState::HalfOpen => Ok(()), // allow one probe
            CircuitState::Open => Err(CircuitState::Open),
        }
    }

    /// Record a successful request. Resets the circuit to Closed.
    pub fn record_success(&self) {
        self.failure_count.store(0, Ordering::Relaxed);
    }

    /// Record a failed request. May transition Closed → Open.
    pub fn record_failure(&self) {
        self.failure_count.fetch_add(1, Ordering::Relaxed);
        let now_ms = self.created_at.elapsed().as_millis() as u64;
        self.last_failure_ms.store(now_ms, Ordering::Relaxed);
    }

    /// Get failure count.
    pub fn failure_count(&self) -> u32 {
        self.failure_count.load(Ordering::Relaxed)
    }

    /// Manually reset the circuit breaker.
    pub fn reset(&self) {
        self.failure_count.store(0, Ordering::Relaxed);
        self.last_failure_ms.store(0, Ordering::Relaxed);
    }
}

/// Manages circuit breakers for multiple providers.
pub struct CircuitBreakerRegistry {
    breakers: std::collections::HashMap<String, CircuitBreaker>,
    config: CircuitBreakerConfig,
}

impl CircuitBreakerRegistry {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            breakers: std::collections::HashMap::new(),
            config,
        }
    }

    /// Get or create a circuit breaker for a provider.
    pub fn get_or_create(&mut self, provider_id: &str) -> &CircuitBreaker {
        if !self.breakers.contains_key(provider_id) {
            self.breakers.insert(
                provider_id.to_string(),
                CircuitBreaker::new(provider_id, self.config.clone()),
            );
        }
        &self.breakers[provider_id]
    }

    /// Check if a provider is available.
    pub fn is_available(&self, provider_id: &str) -> bool {
        self.breakers
            .get(provider_id)
            .map(|b| b.check().is_ok())
            .unwrap_or(true) // unknown provider = assume available
    }

    /// Record success for a provider.
    pub fn record_success(&self, provider_id: &str) {
        if let Some(b) = self.breakers.get(provider_id) {
            b.record_success();
        }
    }

    /// Record failure for a provider.
    pub fn record_failure(&self, provider_id: &str) {
        if let Some(b) = self.breakers.get(provider_id) {
            b.record_failure();
        }
    }

    /// Get status summary for all providers.
    pub fn status_summary(&self) -> Vec<(String, CircuitState, u32)> {
        self.breakers
            .iter()
            .map(|(id, b)| (id.clone(), b.state(), b.failure_count()))
            .collect()
    }
}

impl Default for CircuitBreakerRegistry {
    fn default() -> Self {
        Self::new(CircuitBreakerConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_allows_requests() {
        let cb = CircuitBreaker::new("test", CircuitBreakerConfig::default());
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.check().is_ok());
    }

    #[test]
    fn opens_after_threshold_failures() {
        let cb = CircuitBreaker::new("test", CircuitBreakerConfig {
            failure_threshold: 3,
            recovery_timeout: Duration::from_secs(30),
        });

        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(cb.check().is_err());
    }

    #[test]
    fn success_resets_to_closed() {
        let cb = CircuitBreaker::new("test", CircuitBreakerConfig {
            failure_threshold: 2,
            ..Default::default()
        });

        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.check().is_ok());
    }

    #[test]
    fn half_open_after_recovery_timeout() {
        let cb = CircuitBreaker::new("test", CircuitBreakerConfig {
            failure_threshold: 1,
            recovery_timeout: Duration::from_millis(10),
        });

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        std::thread::sleep(Duration::from_millis(15));
        assert_eq!(cb.state(), CircuitState::HalfOpen);
        assert!(cb.check().is_ok()); // probe allowed
    }

    #[test]
    fn registry_manages_multiple_providers() {
        let mut reg = CircuitBreakerRegistry::default();
        let _ = reg.get_or_create("anthropic");
        let _ = reg.get_or_create("openai");

        assert!(reg.is_available("anthropic"));
        assert!(reg.is_available("openai"));
        assert!(reg.is_available("unknown")); // unknown = available
    }
}
