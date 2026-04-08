//! Integration tests for the provider resilience layer.
//!
//! Tests circuit breaker state transitions, error classification,
//! and fallback eligibility logic without making real API calls.

use pipit_provider::ProviderError;
use pipit_provider::circuit_breaker::{
    CircuitBreaker, CircuitBreakerConfig, CircuitBreakerRegistry, CircuitState,
};
use std::time::Duration;

// ── Circuit Breaker State Machine ──

#[test]
fn circuit_breaker_starts_closed() {
    let cb = CircuitBreaker::new("test", CircuitBreakerConfig::default());
    assert_eq!(cb.state(), CircuitState::Closed);
    assert!(cb.check().is_ok());
    assert_eq!(cb.failure_count(), 0);
}

#[test]
fn circuit_opens_after_threshold_failures() {
    let config = CircuitBreakerConfig {
        failure_threshold: 3,
        recovery_timeout: Duration::from_secs(30),
    };
    let cb = CircuitBreaker::new("anthropic", config);

    // 2 failures: still Closed
    cb.record_failure();
    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Closed);

    // 3rd failure: transitions to Open
    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Open);
    assert!(cb.check().is_err());
}

#[test]
fn success_resets_circuit_to_closed() {
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        recovery_timeout: Duration::from_secs(30),
    };
    let cb = CircuitBreaker::new("openai", config);

    cb.record_failure();
    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Open);

    cb.record_success();
    assert_eq!(cb.state(), CircuitState::Closed);
    assert!(cb.check().is_ok());
}

#[test]
fn circuit_transitions_to_half_open_after_timeout() {
    let config = CircuitBreakerConfig {
        failure_threshold: 1,
        recovery_timeout: Duration::from_millis(1), // very short for testing
    };
    let cb = CircuitBreaker::new("test", config);

    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Open);

    // Wait for recovery timeout
    std::thread::sleep(Duration::from_millis(10));

    // Should be HalfOpen now
    assert_eq!(cb.state(), CircuitState::HalfOpen);
    assert!(cb.check().is_ok()); // probe request allowed
}

// ── Circuit Breaker Registry ──

#[test]
fn registry_creates_breakers_on_demand() {
    let mut registry = CircuitBreakerRegistry::new(CircuitBreakerConfig::default());

    // Unknown provider is assumed available
    assert!(registry.is_available("new_provider"));

    // After creation, starts closed
    let _cb = registry.get_or_create("anthropic");
    assert!(registry.is_available("anthropic"));
}

#[test]
fn registry_tracks_failures_per_provider() {
    let config = CircuitBreakerConfig {
        failure_threshold: 2,
        recovery_timeout: Duration::from_secs(30),
    };
    let mut registry = CircuitBreakerRegistry::new(config);

    registry.get_or_create("anthropic");
    registry.get_or_create("openai");

    // Fail anthropic
    registry.record_failure("anthropic");
    registry.record_failure("anthropic");
    assert!(!registry.is_available("anthropic"));
    assert!(registry.is_available("openai")); // unaffected
}

// ── Error Classification ──

#[test]
fn transient_errors_are_retryable() {
    assert!(ProviderError::Network("connection reset".to_string()).is_transient());
    assert!(
        ProviderError::RateLimited {
            retry_after_ms: Some(1000)
        }
        .is_transient()
    );
    assert!(ProviderError::OutputTruncated.is_transient());
    assert!(ProviderError::Other("server returned 503".to_string()).is_transient());
    assert!(ProviderError::Other("overloaded, try again".to_string()).is_transient());
}

#[test]
fn permanent_errors_are_not_retryable() {
    assert!(
        ProviderError::AuthFailed {
            message: "bad key".to_string()
        }
        .is_permanent()
    );
    assert!(
        ProviderError::ModelNotFound {
            model: "gpt-5".to_string()
        }
        .is_permanent()
    );
    assert!(ProviderError::Cancelled.is_permanent());
}

#[test]
fn context_recoverable_errors_detected() {
    assert!(
        ProviderError::RequestTooLarge {
            message: "too big".to_string()
        }
        .is_context_recoverable()
    );
    assert!(
        ProviderError::ContextOverflow {
            used: 300_000,
            limit: 200_000
        }
        .is_context_recoverable()
    );
    assert!(
        ProviderError::Other("context_length_exceeded: 250000 > 200000".to_string())
            .is_context_recoverable()
    );
    assert!(
        ProviderError::Other("too many tokens in the prompt".to_string()).is_context_recoverable()
    );
}

#[test]
fn non_context_errors_are_not_context_recoverable() {
    assert!(!ProviderError::Network("timeout".to_string()).is_context_recoverable());
    assert!(!ProviderError::OutputTruncated.is_context_recoverable());
    assert!(
        !ProviderError::AuthFailed {
            message: "no".to_string()
        }
        .is_context_recoverable()
    );
}

// ── Error Classification Mutual Exclusivity ──

#[test]
fn permanent_errors_are_never_transient() {
    let permanent_errors = vec![
        ProviderError::AuthFailed {
            message: "bad".to_string(),
        },
        ProviderError::ModelNotFound {
            model: "x".to_string(),
        },
        ProviderError::Cancelled,
    ];
    for err in &permanent_errors {
        assert!(
            !err.is_transient(),
            "Permanent error should not be transient: {:?}",
            err
        );
    }
}
