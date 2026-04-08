//! Environment Simulator with Fault Injection — Task 7.2
//!
//! Simulates external services with configurable failure modes:
//! - Latency: LogNormal(μ, σ²), default median 10ms, P99 ≈ 220ms
//! - Faults: Bernoulli(p_fault) per request
//! - Rate limiting: Token bucket with capacity C, refill rate r
//!
//! P(≥1 fault in N requests) = 1 - (1-p)^N ≈ 1.0 for p=0.02, N=1000.

use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Configuration for fault injection on a mock service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaultConfig {
    /// Probability of failure per request (Bernoulli).
    pub fault_probability: f64,
    /// Latency distribution: LogNormal(μ, σ). Median ≈ e^μ ms.
    pub latency_mu: f64,
    pub latency_sigma: f64,
    /// Rate limiter: token bucket capacity.
    pub rate_limit_capacity: u32,
    /// Rate limiter: tokens refilled per second.
    pub rate_limit_refill: f64,
    /// Possible error codes when fault triggers.
    pub error_codes: Vec<u16>,
}

impl Default for FaultConfig {
    fn default() -> Self {
        Self {
            fault_probability: 0.02,
            latency_mu: 2.3,    // median ≈ 10ms
            latency_sigma: 1.0, // P99 ≈ 220ms
            rate_limit_capacity: 100,
            rate_limit_refill: 100.0,
            error_codes: vec![500, 502, 503, 429],
        }
    }
}

/// A mock external service with fault injection.
pub struct ServiceMock {
    pub name: String,
    pub config: FaultConfig,
    tokens: f64,
    last_refill: Instant,
    pub stats: ServiceStats,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServiceStats {
    pub total_requests: u64,
    pub successful: u64,
    pub faults_injected: u64,
    pub rate_limited: u64,
    pub total_latency_ms: u64,
}

/// Response from a mock service call.
#[derive(Debug, Clone)]
pub struct MockResponse {
    pub status_code: u16,
    pub latency: Duration,
    pub body: String,
    pub fault_injected: bool,
}

impl ServiceMock {
    pub fn new(name: &str, config: FaultConfig) -> Self {
        Self {
            name: name.to_string(),
            tokens: config.rate_limit_capacity as f64,
            last_refill: Instant::now(),
            config,
            stats: ServiceStats::default(),
        }
    }

    /// Simulate a service call with latency and fault injection.
    pub fn call(&mut self) -> MockResponse {
        let mut rng = rand::thread_rng();
        self.stats.total_requests += 1;

        // Token bucket rate limiting
        self.refill_tokens();
        if self.tokens < 1.0 {
            self.stats.rate_limited += 1;
            return MockResponse {
                status_code: 429,
                latency: Duration::from_millis(1),
                body: r#"{"error":"rate_limited"}"#.to_string(),
                fault_injected: false,
            };
        }
        self.tokens -= 1.0;

        // Log-normal latency: L ~ LogNormal(μ, σ²)
        let latency_ms = self.sample_log_normal(&mut rng);
        let latency = Duration::from_millis(latency_ms as u64);
        self.stats.total_latency_ms += latency_ms as u64;

        // Bernoulli fault injection
        if rng.gen_range(0.0..1.0) < self.config.fault_probability {
            self.stats.faults_injected += 1;
            let idx = rng.gen_range(0..self.config.error_codes.len().max(1));
            let code = self.config.error_codes.get(idx).copied().unwrap_or(500);
            return MockResponse {
                status_code: code,
                latency,
                body: format!(r#"{{"error":"simulated_fault","code":{}}}"#, code),
                fault_injected: true,
            };
        }

        self.stats.successful += 1;
        MockResponse {
            status_code: 200,
            latency,
            body: r#"{"status":"ok"}"#.to_string(),
            fault_injected: false,
        }
    }

    fn refill_tokens(&mut self) {
        let elapsed = self.last_refill.elapsed().as_secs_f64();
        let refill = elapsed * self.config.rate_limit_refill;
        self.tokens = (self.tokens + refill).min(self.config.rate_limit_capacity as f64);
        self.last_refill = Instant::now();
    }

    /// Sample from LogNormal(μ, σ) using Box-Muller.
    fn sample_log_normal(&self, rng: &mut impl Rng) -> f64 {
        let u1: f64 = rng.gen_range(0.0..1.0f64).max(1e-10);
        let u2: f64 = rng.gen_range(0.0..1.0);
        let z = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
        let normal_sample = self.config.latency_mu + self.config.latency_sigma * z;
        normal_sample.exp().max(0.1) // ms, minimum 0.1ms
    }
}

/// An environment containing multiple mock services.
pub struct EnvironmentSimulator {
    pub services: HashMap<String, ServiceMock>,
}

impl EnvironmentSimulator {
    pub fn new() -> Self {
        Self {
            services: HashMap::new(),
        }
    }

    /// Add a service with default fault config.
    pub fn add_service(&mut self, name: &str, config: FaultConfig) {
        self.services
            .insert(name.to_string(), ServiceMock::new(name, config));
    }

    /// Create a pre-configured e-commerce environment.
    pub fn ecommerce() -> Self {
        let mut env = Self::new();
        env.add_service(
            "payment_gateway",
            FaultConfig {
                fault_probability: 0.02,
                latency_mu: 3.0, // median ~20ms
                ..Default::default()
            },
        );
        env.add_service(
            "inventory_api",
            FaultConfig {
                fault_probability: 0.01,
                latency_mu: 2.0, // median ~7ms
                ..Default::default()
            },
        );
        env.add_service(
            "email_service",
            FaultConfig {
                fault_probability: 0.05, // Less reliable
                latency_mu: 4.0,         // median ~55ms
                ..Default::default()
            },
        );
        env.add_service(
            "database",
            FaultConfig {
                fault_probability: 0.001, // Very reliable
                latency_mu: 1.0,          // median ~3ms
                rate_limit_capacity: 1000,
                rate_limit_refill: 500.0,
                ..Default::default()
            },
        );
        env
    }

    /// Call a service and return the response.
    pub fn call_service(&mut self, name: &str) -> Option<MockResponse> {
        self.services.get_mut(name).map(|s| s.call())
    }

    /// Get aggregate stats across all services.
    pub fn aggregate_stats(&self) -> HashMap<String, ServiceStats> {
        self.services
            .iter()
            .map(|(name, svc)| (name.clone(), svc.stats.clone()))
            .collect()
    }
}

impl Default for EnvironmentSimulator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_mock_basic() {
        let mut svc = ServiceMock::new("test", FaultConfig::default());
        let mut ok_count = 0;
        for _ in 0..100 {
            let resp = svc.call();
            if resp.status_code == 200 {
                ok_count += 1;
            }
        }
        assert!(
            ok_count > 80,
            "Most requests should succeed: {}/100",
            ok_count
        );
        assert!(svc.stats.total_requests == 100);
    }

    #[test]
    fn test_fault_injection_probability() {
        let mut svc = ServiceMock::new(
            "test",
            FaultConfig {
                fault_probability: 0.5, // 50% fault rate
                rate_limit_capacity: 10000,
                rate_limit_refill: 10000.0,
                ..Default::default()
            },
        );
        for _ in 0..1000 {
            svc.call();
        }
        // With p=0.5, expect ~500 faults ± noise
        assert!(
            svc.stats.faults_injected > 350,
            "faults: {}",
            svc.stats.faults_injected
        );
        assert!(
            svc.stats.faults_injected < 650,
            "faults: {}",
            svc.stats.faults_injected
        );
    }

    #[test]
    fn test_ecommerce_environment() {
        let mut env = EnvironmentSimulator::ecommerce();
        for _ in 0..100 {
            env.call_service("payment_gateway");
            env.call_service("database");
        }
        let stats = env.aggregate_stats();
        assert!(stats.get("payment_gateway").unwrap().total_requests == 100);
        assert!(stats.get("database").unwrap().total_requests == 100);
    }

    #[test]
    fn test_log_normal_latency_distribution() {
        let mut svc = ServiceMock::new(
            "test",
            FaultConfig {
                fault_probability: 0.0, // No faults
                rate_limit_capacity: 10000,
                rate_limit_refill: 10000.0,
                ..Default::default()
            },
        );
        let mut latencies = Vec::new();
        for _ in 0..1000 {
            let resp = svc.call();
            latencies.push(resp.latency.as_millis() as f64);
        }
        latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = latencies[500];
        let p99 = latencies[990];
        // With μ=2.3, σ=1.0: median ≈ e^2.3 ≈ 10ms, P99 ≈ 220ms
        assert!(
            median > 2.0 && median < 50.0,
            "Median latency: {}ms",
            median
        );
        assert!(p99 > median, "P99 should > median: p99={}ms", p99);
    }
}
