//! # Cross-Provider Cache Orchestration (D2)
//!
//! Cache-aware prompt editing across providers. When switching providers
//! (e.g., from Anthropic to OpenAI), cached prompt prefixes have to be
//! re-sent. This module tracks cache states across providers and optimizes
//! prompt ordering for maximum prefix-cache hits.
//!
//! ## Design
//!
//! Each provider maintains a prompt-prefix cache. Cache keys are hashes
//! of (system_prompt, messages[..n]). When we detect a cache-eligible
//! prefix, we reorder the prompt to put the cacheable parts first.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Hash of a prompt prefix (message hash chain).
pub type PrefixHash = u64;

/// Cache state for a single provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderCacheState {
    /// Known cached prefix hashes → token count.
    pub cached_prefixes: HashMap<PrefixHash, CacheEntry>,
    /// Total cache hits this session.
    pub hits: u64,
    /// Total cache misses this session.
    pub misses: u64,
}

/// A single cache entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub token_count: u32,
    pub last_used_turn: u32,
    pub hit_count: u32,
}

/// Cache orchestrator managing cross-provider prompt caching.
pub struct CacheOrchestrator {
    providers: HashMap<String, ProviderCacheState>,
    current_provider: String,
    /// How many prefix tokens qualify for caching (min length).
    min_cache_prefix_tokens: u32,
    /// Maximum cached prefixes per provider.
    max_entries_per_provider: usize,
}

/// Decision about how to construct the prompt for cache efficiency.
#[derive(Debug, Clone)]
pub struct CacheAdvice {
    /// Whether to use cached prefix.
    pub use_cache: bool,
    /// Number of prefix tokens that are cache-eligible.
    pub cached_prefix_tokens: u32,
    /// Estimated cost savings in tokens (reading from cache is cheaper).
    pub estimated_savings: u32,
    /// Suggested provider if switching would improve cache hits.
    pub suggested_provider: Option<String>,
}

impl CacheOrchestrator {
    pub fn new(current_provider: &str) -> Self {
        let mut providers = HashMap::new();
        providers.insert(
            current_provider.to_string(),
            ProviderCacheState::default(),
        );
        Self {
            providers,
            current_provider: current_provider.to_string(),
            min_cache_prefix_tokens: 1024,
            max_entries_per_provider: 100,
        }
    }

    /// Compute a prefix hash from a system prompt and message history.
    pub fn compute_prefix_hash(system: &str, messages: &[&str]) -> PrefixHash {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        system.hash(&mut hasher);
        for msg in messages {
            msg.hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Record that a prefix was sent to a provider and was cached.
    pub fn record_cache_write(
        &mut self,
        provider: &str,
        prefix_hash: PrefixHash,
        token_count: u32,
        turn: u32,
    ) {
        let state = self
            .providers
            .entry(provider.to_string())
            .or_default();

        // Evict oldest if at capacity
        if state.cached_prefixes.len() >= self.max_entries_per_provider {
            let oldest = state
                .cached_prefixes
                .iter()
                .min_by_key(|(_, e)| e.last_used_turn)
                .map(|(k, _)| *k);
            if let Some(key) = oldest {
                state.cached_prefixes.remove(&key);
            }
        }

        state.cached_prefixes.insert(
            prefix_hash,
            CacheEntry {
                token_count,
                last_used_turn: turn,
                hit_count: 0,
            },
        );
    }

    /// Check if a prefix is cached for a provider.
    pub fn check_cache(
        &mut self,
        provider: &str,
        prefix_hash: PrefixHash,
    ) -> Option<u32> {
        let state = self.providers.get_mut(provider)?;
        if let Some(entry) = state.cached_prefixes.get_mut(&prefix_hash) {
            entry.hit_count += 1;
            state.hits += 1;
            Some(entry.token_count)
        } else {
            state.misses += 1;
            None
        }
    }

    /// Get cache advice: should we use cache, and which provider has the best cache?
    pub fn advise(
        &self,
        prefix_hash: PrefixHash,
        estimated_tokens: u32,
    ) -> CacheAdvice {
        // Check current provider first
        if let Some(state) = self.providers.get(&self.current_provider) {
            if let Some(entry) = state.cached_prefixes.get(&prefix_hash) {
                if entry.token_count >= self.min_cache_prefix_tokens {
                    return CacheAdvice {
                        use_cache: true,
                        cached_prefix_tokens: entry.token_count,
                        estimated_savings: entry.token_count / 4, // ~75% discount on cached
                        suggested_provider: None,
                    };
                }
            }
        }

        // Check other providers
        for (provider, state) in &self.providers {
            if provider == &self.current_provider {
                continue;
            }
            if let Some(entry) = state.cached_prefixes.get(&prefix_hash) {
                if entry.token_count >= self.min_cache_prefix_tokens
                    && entry.token_count > estimated_tokens / 2
                {
                    return CacheAdvice {
                        use_cache: true,
                        cached_prefix_tokens: entry.token_count,
                        estimated_savings: entry.token_count / 4,
                        suggested_provider: Some(provider.clone()),
                    };
                }
            }
        }

        CacheAdvice {
            use_cache: false,
            cached_prefix_tokens: 0,
            estimated_savings: 0,
            suggested_provider: None,
        }
    }

    /// Switch the active provider.
    pub fn switch_provider(&mut self, provider: &str) {
        self.providers
            .entry(provider.to_string())
            .or_default();
        self.current_provider = provider.to_string();
    }

    /// Get hit rate for a provider.
    pub fn hit_rate(&self, provider: &str) -> Option<f64> {
        let state = self.providers.get(provider)?;
        let total = state.hits + state.misses;
        if total == 0 {
            Some(0.0)
        } else {
            Some(state.hits as f64 / total as f64)
        }
    }

    /// Get the current provider.
    pub fn current_provider(&self) -> &str {
        &self.current_provider
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_write_and_read() {
        let mut orch = CacheOrchestrator::new("anthropic");
        let hash = CacheOrchestrator::compute_prefix_hash("system", &["hello", "world"]);
        orch.record_cache_write("anthropic", hash, 2048, 1);
        let tokens = orch.check_cache("anthropic", hash);
        assert_eq!(tokens, Some(2048));
    }

    #[test]
    fn cache_miss() {
        let mut orch = CacheOrchestrator::new("anthropic");
        let result = orch.check_cache("anthropic", 12345);
        assert_eq!(result, None);
    }

    #[test]
    fn advise_uses_current_provider_cache() {
        let mut orch = CacheOrchestrator::new("anthropic");
        let hash = CacheOrchestrator::compute_prefix_hash("sys", &["msg1"]);
        orch.record_cache_write("anthropic", hash, 2048, 1);
        let advice = orch.advise(hash, 5000);
        assert!(advice.use_cache);
        assert!(advice.suggested_provider.is_none());
    }

    #[test]
    fn advise_suggests_provider_switch() {
        let mut orch = CacheOrchestrator::new("openai");
        let hash = CacheOrchestrator::compute_prefix_hash("sys", &["msg1"]);
        orch.record_cache_write("anthropic", hash, 4096, 1);
        let advice = orch.advise(hash, 5000);
        assert!(advice.use_cache);
        assert_eq!(advice.suggested_provider, Some("anthropic".into()));
    }

    #[test]
    fn hit_rate_tracking() {
        let mut orch = CacheOrchestrator::new("anthropic");
        let hash = CacheOrchestrator::compute_prefix_hash("sys", &["a"]);
        orch.record_cache_write("anthropic", hash, 2048, 1);
        orch.check_cache("anthropic", hash); // hit
        orch.check_cache("anthropic", 99999); // miss
        let rate = orch.hit_rate("anthropic").unwrap();
        assert!((rate - 0.5).abs() < 0.01);
    }

    #[test]
    fn eviction_on_capacity() {
        let mut orch = CacheOrchestrator::new("anthropic");
        // Default max is 100 entries
        for i in 0..105u64 {
            orch.record_cache_write("anthropic", i, 2048, i as u32);
        }
        let state = orch.providers.get("anthropic").unwrap();
        assert!(state.cached_prefixes.len() <= 100);
    }
}
