//! Runtime Provider Registry — Dynamic provider registration.
//!
//! Replaces the static `create_provider(ProviderKind, ...)` dispatch with a
//! `ProviderRegistry` that holds factory closures keyed by string name.
//!
//! Built-in providers auto-register at startup. Plugins can register additional
//! providers at runtime via `registry.register("gitlab-duo", factory)`.
//!
//! The circuit breaker composes as a decorator — third-party providers inherit
//! resilience for free.

use crate::{LlmProvider, ProviderError};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Factory function that creates a provider from (model, api_key, base_url).
pub type ProviderFactory =
    Arc<dyn Fn(&str, &str, Option<&str>) -> Result<Box<dyn LlmProvider>, ProviderError> + Send + Sync>;

/// Runtime provider registry supporting dynamic registration.
pub struct ProviderRegistry {
    factories: RwLock<HashMap<String, ProviderFactory>>,
}

impl ProviderRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            factories: RwLock::new(HashMap::new()),
        }
    }

    /// Create a registry pre-populated with all built-in providers.
    pub fn with_builtins() -> Self {
        let registry = Self::new();
        register_builtins(&registry);
        registry
    }

    /// Register a provider factory under a name.
    /// Overwrites any existing factory with the same name.
    pub fn register(&self, name: impl Into<String>, factory: ProviderFactory) {
        self.factories
            .write()
            .expect("registry lock poisoned")
            .insert(name.into(), factory);
    }

    /// Unregister a provider by name.
    pub fn unregister(&self, name: &str) -> bool {
        self.factories
            .write()
            .expect("registry lock poisoned")
            .remove(name)
            .is_some()
    }

    /// Create a provider by name.
    pub fn create(
        &self,
        name: &str,
        model: &str,
        api_key: &str,
        base_url: Option<&str>,
    ) -> Result<Box<dyn LlmProvider>, ProviderError> {
        let factories = self.factories.read().expect("registry lock poisoned");
        let factory = factories.get(name).ok_or_else(|| {
            let available: Vec<&str> = factories.keys().map(|s| s.as_str()).collect();
            ProviderError::Other(format!(
                "Unknown provider '{}'. Available: {}",
                name,
                available.join(", ")
            ))
        })?;
        factory(model, api_key, base_url)
    }

    /// List all registered provider names.
    pub fn names(&self) -> Vec<String> {
        let factories = self.factories.read().expect("registry lock poisoned");
        let mut names: Vec<String> = factories.keys().cloned().collect();
        names.sort();
        names
    }

    /// Check if a provider is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.factories
            .read()
            .expect("registry lock poisoned")
            .contains_key(name)
    }

    /// Number of registered providers.
    pub fn len(&self) -> usize {
        self.factories
            .read()
            .expect("registry lock poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

/// Register all built-in providers into a registry.
fn register_builtins(registry: &ProviderRegistry) {
    use crate::*;

    // ── Anthropic family ──
    registry.register(
        "anthropic",
        Arc::new(|model, api_key, base_url| {
            Ok(Box::new(anthropic::AnthropicProvider::new(
                model.to_string(),
                api_key.to_string(),
                base_url.map(|s| s.to_string()),
            )?))
        }),
    );
    registry.register(
        "anthropic_compatible",
        Arc::new(|model, api_key, base_url| {
            let url = base_url.ok_or_else(|| {
                ProviderError::Other("--base-url required for anthropic_compatible".into())
            })?;
            Ok(Box::new(anthropic::AnthropicProvider::new(
                model.to_string(),
                api_key.to_string(),
                Some(url.to_string()),
            )?))
        }),
    );
    registry.register(
        "minimax",
        Arc::new(|model, api_key, base_url| {
            Ok(Box::new(anthropic::AnthropicProvider::with_id(
                "minimax".into(),
                model.into(),
                api_key.into(),
                Some(base_url.unwrap_or("https://api.minimax.io/anthropic").into()),
            )?))
        }),
    );
    registry.register(
        "minimax_cn",
        Arc::new(|model, api_key, base_url| {
            Ok(Box::new(anthropic::AnthropicProvider::with_id(
                "minimax_cn".into(),
                model.into(),
                api_key.into(),
                Some(base_url.unwrap_or("https://api.minimaxi.com/anthropic").into()),
            )?))
        }),
    );

    // ── OpenAI family ──
    registry.register(
        "openai",
        Arc::new(|model, api_key, base_url| {
            Ok(Box::new(openai::OpenAiProvider::new(
                model.into(),
                api_key.into(),
                base_url.map(|s| s.to_string()),
            )?))
        }),
    );
    registry.register(
        "openai_compatible",
        Arc::new(|model, api_key, base_url| {
            let url = base_url.ok_or_else(|| {
                ProviderError::Other("--base-url required for openai_compatible".into())
            })?;
            Ok(Box::new(openai::OpenAiProvider::with_id(
                "openai_compatible".into(),
                model.into(),
                api_key.into(),
                Some(url.into()),
            )?))
        }),
    );

    // OpenAI-compatible services (each with default base URL)
    let openai_services: &[(&str, &str)] = &[
        ("openai_codex", "https://api.openai.com"),
        ("deepseek", "https://api.deepseek.com"),
        ("openrouter", "https://openrouter.ai/api"),
        ("vercel_ai_gateway", "https://ai-gateway.vercel.sh"),
        ("github_copilot", "https://api.individual.githubcopilot.com"),
        ("xai", "https://api.x.ai"),
        ("zai", "https://api.z.ai/api/coding/paas/v4"),
        ("cerebras", "https://api.cerebras.ai"),
        ("groq", "https://api.groq.com/openai"),
        ("huggingface", "https://router.huggingface.co"),
        ("ollama", "http://localhost:11434"),
        ("opencode", "https://opencode.ai/zen"),
        ("opencode_go", "https://opencode.ai/zen/go/v1"),
        ("kimi_coding", "https://api.kimi.com/coding"),
    ];

    for &(name, default_url) in openai_services {
        let name_owned = name.to_string();
        let url_owned = default_url.to_string();
        registry.register(
            name,
            Arc::new(move |model, api_key, base_url| {
                Ok(Box::new(openai::OpenAiProvider::with_id(
                    name_owned.clone(),
                    model.into(),
                    api_key.into(),
                    Some(base_url.unwrap_or(&url_owned).to_string()),
                )?))
            }),
        );
    }

    // ── Google family ──
    registry.register(
        "google",
        Arc::new(|model, api_key, base_url| {
            Ok(Box::new(google::GoogleProvider::new(
                model.into(),
                api_key.into(),
                base_url.map(|s| s.to_string()),
            )?))
        }),
    );
    registry.register(
        "google_gemini_cli",
        Arc::new(|model, api_key, base_url| {
            Ok(Box::new(google_cli::GoogleCliProvider::new(
                "google_gemini_cli".into(),
                model.into(),
                api_key.into(),
                base_url.map(|s| s.to_string()),
            )?))
        }),
    );
    registry.register(
        "google_antigravity",
        Arc::new(|model, api_key, base_url| {
            Ok(Box::new(google_cli::GoogleCliProvider::new(
                "google_antigravity".into(),
                model.into(),
                api_key.into(),
                base_url.map(|s| s.to_string()),
            )?))
        }),
    );

    // ── Specialized providers ──
    registry.register(
        "azure_openai",
        Arc::new(|model, api_key, base_url| {
            Ok(Box::new(azure_openai::AzureOpenAiProvider::new(
                model.into(),
                api_key.into(),
                base_url.map(|s| s.to_string()),
            )?))
        }),
    );
    registry.register(
        "vertex",
        Arc::new(|model, api_key, base_url| {
            Ok(Box::new(vertex::VertexProvider::new(
                model.into(),
                api_key.into(),
                base_url.map(|s| s.to_string()),
            )?))
        }),
    );
    registry.register(
        "mistral",
        Arc::new(|model, api_key, base_url| {
            Ok(Box::new(mistral::MistralProvider::new(
                model.into(),
                api_key.into(),
                base_url.map(|s| s.to_string()),
            )?))
        }),
    );
    registry.register(
        "amazon_bedrock",
        Arc::new(|model, api_key, base_url| {
            Ok(Box::new(bedrock::BedrockProvider::new(
                model.into(),
                api_key.into(),
                base_url.map(|s| s.to_string()),
            )?))
        }),
    );

    // ── OAuth providers ──
    registry.register(
        "openai_responses",
        Arc::new(|model, api_key, base_url| {
            Ok(Box::new(openai_responses::OpenAiResponsesProvider::new(
                model.into(),
                api_key.into(),
                base_url.map(|s| s.to_string()),
            )?))
        }),
    );
    registry.register(
        "codex_oauth",
        Arc::new(|model, api_key, base_url| {
            Ok(Box::new(codex_oauth::CodexOAuthProvider::new(
                model.into(),
                api_key.into(),
                base_url.map(|s| s.to_string()),
            )?))
        }),
    );
    registry.register(
        "copilot_oauth",
        Arc::new(|model, api_key, base_url| {
            Ok(Box::new(copilot_oauth::CopilotOAuthProvider::new(
                model.into(),
                api_key.into(),
                base_url.map(|s| s.to_string()),
            )?))
        }),
    );

    // ── Faux (testing) ──
    registry.register(
        "faux",
        Arc::new(|_model, _api_key, _base_url| {
            Ok(Box::new(faux::FauxProvider::text("faux response")))
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_contains_all_providers() {
        let reg = ProviderRegistry::with_builtins();
        // Core providers
        assert!(reg.contains("openai"));
        assert!(reg.contains("anthropic"));
        assert!(reg.contains("google"));
        assert!(reg.contains("mistral"));
        assert!(reg.contains("amazon_bedrock"));
        assert!(reg.contains("azure_openai"));
        assert!(reg.contains("vertex"));
        assert!(reg.contains("faux"));
        // OpenAI-compatible services
        assert!(reg.contains("deepseek"));
        assert!(reg.contains("groq"));
        assert!(reg.contains("ollama"));
        assert!(reg.contains("openrouter"));
        assert!(reg.contains("github_copilot"));
        // OAuth providers
        assert!(reg.contains("openai_responses"));
        assert!(reg.contains("codex_oauth"));
        assert!(reg.contains("copilot_oauth"));
        // Should have 25+ providers
        assert!(reg.len() >= 25, "expected ≥25 providers, got {}", reg.len());
    }

    #[test]
    fn custom_provider_registration() {
        let reg = ProviderRegistry::new();
        assert!(!reg.contains("custom_llm"));

        reg.register(
            "custom_llm",
            Arc::new(|_model, _api_key, _base_url| {
                Ok(Box::new(crate::faux::FauxProvider::text("custom response")))
            }),
        );

        assert!(reg.contains("custom_llm"));
        let provider = reg.create("custom_llm", "model", "key", None).unwrap();
        assert_eq!(provider.id(), "faux");
    }

    #[test]
    fn unregister_removes_provider() {
        let reg = ProviderRegistry::with_builtins();
        assert!(reg.contains("faux"));
        assert!(reg.unregister("faux"));
        assert!(!reg.contains("faux"));
        assert!(!reg.unregister("faux")); // second call returns false
    }

    #[test]
    fn create_unknown_provider_errors() {
        let reg = ProviderRegistry::new();
        let result = reg.create("nonexistent", "model", "key", None);
        assert!(result.is_err());
        let err = format!("{}", result.err().unwrap());
        assert!(err.contains("Unknown provider"));
    }

    #[test]
    fn names_returns_sorted_list() {
        let reg = ProviderRegistry::with_builtins();
        let names = reg.names();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn faux_provider_creates_successfully() {
        let reg = ProviderRegistry::with_builtins();
        let provider = reg.create("faux", "any", "any", None).unwrap();
        assert_eq!(provider.id(), "faux");
    }
}
