//! Model Routing — Alias resolution, priority cascade, fast-mode auto-routing.
//!
//! Priority cascade (highest → lowest):
//!   1. Session override (/model command)
//!   2. CLI --model flag
//!   3. PIPIT_MODEL or ANTHROPIC_MODEL env var
//!   4. Project config (.pipit/config.toml)
//!   5. Global config (~/.config/pipit/config.toml)
//!   6. Default model for the active provider
//!
//! Fast-mode auto-routing:
//!   Score S = α·token_count + β·tool_history + γ·entropy
//!   If S < θ_fast → route to small model (Haiku-class)
//!
//! Model alias resolution: "opus" → "claude-opus-4-20250514"
//!   HashMap lookup: O(1)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

// ═══════════════════════════════════════════════════════════════════════════
//  Model Aliases
// ═══════════════════════════════════════════════════════════════════════════

/// Built-in model aliases. Resolved before any provider API call.
pub fn default_aliases() -> HashMap<String, String> {
    let mut m = HashMap::new();

    // Anthropic aliases
    m.insert("opus".into(), "claude-opus-4-20250514".into());
    m.insert("sonnet".into(), "claude-sonnet-4-20250514".into());
    m.insert("haiku".into(), "claude-haiku-4-5-20251001".into());
    m.insert("claude-opus".into(), "claude-opus-4-20250514".into());
    m.insert("claude-sonnet".into(), "claude-sonnet-4-20250514".into());
    m.insert("claude-haiku".into(), "claude-haiku-4-5-20251001".into());

    // OpenAI aliases
    m.insert("gpt4o".into(), "gpt-4o".into());
    m.insert("gpt4".into(), "gpt-4-turbo".into());
    m.insert("gpt4o-mini".into(), "gpt-4o-mini".into());
    m.insert("o1".into(), "o1".into());
    m.insert("o3".into(), "o3".into());

    // Google aliases
    m.insert("gemini-pro".into(), "gemini-2.5-pro".into());
    m.insert("gemini-flash".into(), "gemini-2.5-flash".into());

    // DeepSeek aliases
    m.insert("deepseek".into(), "deepseek-chat".into());
    m.insert("deepseek-r1".into(), "deepseek-reasoner".into());

    m
}

// ═══════════════════════════════════════════════════════════════════════════
//  Priority Cascade
// ═══════════════════════════════════════════════════════════════════════════

/// Model selection priority levels (highest priority first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ModelPriority {
    /// Session override (/model command or runtime API call)
    SessionOverride = 0,
    /// CLI --model flag
    CliFlag = 1,
    /// Environment variable (PIPIT_MODEL or ANTHROPIC_MODEL)
    EnvVar = 2,
    /// Project-level config (.pipit/config.toml)
    ProjectConfig = 3,
    /// Global config (~/.config/pipit/config.toml)
    GlobalConfig = 4,
    /// Provider default
    ProviderDefault = 5,
}

/// Tracks the source of the current model selection.
#[derive(Debug, Clone)]
pub struct ModelSelection {
    pub model_id: String,
    pub resolved_from: String,
    pub priority: ModelPriority,
    pub is_fast_mode: bool,
}

/// The model router. Resolves aliases and applies the priority cascade.
#[derive(Debug)]
pub struct ModelRouter {
    aliases: HashMap<String, String>,
    /// Current model selection (highest priority wins).
    current: Arc<RwLock<Option<ModelSelection>>>,
    /// Fast mode threshold. Set to 0.0 to disable.
    fast_mode_threshold: f64,
    /// Small model to route to in fast mode.
    fast_model: String,
    /// Sources in priority order (populated during init).
    sources: Vec<(ModelPriority, Option<String>)>,
}

impl ModelRouter {
    pub fn new() -> Self {
        Self {
            aliases: default_aliases(),
            current: Arc::new(RwLock::new(None)),
            fast_mode_threshold: 0.0, // Disabled by default
            fast_model: "claude-haiku-4-5-20251001".into(),
            sources: Vec::new(),
        }
    }

    /// Initialize the router with all model sources.
    pub fn init(
        &mut self,
        cli_model: Option<&str>,
        project_config_model: Option<&str>,
        global_config_model: Option<&str>,
        provider_default: &str,
    ) {
        // Populate sources in priority order
        self.sources.clear();

        // Env vars
        let env_model = std::env::var("PIPIT_MODEL")
            .or_else(|_| std::env::var("ANTHROPIC_MODEL"))
            .ok();

        self.sources
            .push((ModelPriority::CliFlag, cli_model.map(String::from)));
        self.sources.push((ModelPriority::EnvVar, env_model));
        self.sources.push((
            ModelPriority::ProjectConfig,
            project_config_model.map(String::from),
        ));
        self.sources.push((
            ModelPriority::GlobalConfig,
            global_config_model.map(String::from),
        ));
        self.sources.push((
            ModelPriority::ProviderDefault,
            Some(provider_default.to_string()),
        ));

        // Resolve: first non-None source wins
        for (priority, source) in &self.sources {
            if let Some(model) = source {
                let resolved = self.resolve_alias(model);
                let mut current = self.current.write().unwrap();
                *current = Some(ModelSelection {
                    model_id: resolved.clone(),
                    resolved_from: model.clone(),
                    priority: *priority,
                    is_fast_mode: false,
                });
                tracing::info!(model = %resolved, priority = ?priority, "Model selected");
                return;
            }
        }
    }

    /// Resolve an alias to a model ID. Returns the input unchanged if no alias matches.
    pub fn resolve_alias(&self, model: &str) -> String {
        self.aliases
            .get(&model.to_lowercase())
            .cloned()
            .unwrap_or_else(|| model.to_string())
    }

    /// Override the model for the current session (/model command).
    pub fn set_session_override(&self, model: &str) {
        let resolved = self
            .aliases
            .get(&model.to_lowercase())
            .cloned()
            .unwrap_or_else(|| model.to_string());

        let mut current = self.current.write().unwrap();
        *current = Some(ModelSelection {
            model_id: resolved.clone(),
            resolved_from: model.to_string(),
            priority: ModelPriority::SessionOverride,
            is_fast_mode: false,
        });
        tracing::info!(model = %resolved, "Session model override set");
    }

    /// Clear session override (reverts to next priority level).
    pub fn clear_session_override(&self) {
        let mut current = self.current.write().unwrap();
        if let Some(ref sel) = *current {
            if sel.priority == ModelPriority::SessionOverride {
                *current = None;
                // Re-resolve from remaining sources
                drop(current);
                // Caller should re-init or manually set
            }
        }
    }

    /// Get the current model ID.
    pub fn current_model(&self) -> String {
        self.current
            .read()
            .unwrap()
            .as_ref()
            .map(|s| s.model_id.clone())
            .unwrap_or_else(|| "claude-sonnet-4-20250514".to_string())
    }

    /// Get the full model selection details.
    pub fn current_selection(&self) -> Option<ModelSelection> {
        self.current.read().unwrap().clone()
    }

    /// Enable fast mode with a threshold and target model.
    pub fn enable_fast_mode(&mut self, threshold: f64, fast_model: &str) {
        self.fast_mode_threshold = threshold;
        self.fast_model = self.resolve_alias(fast_model);
    }

    /// Disable fast mode.
    pub fn disable_fast_mode(&mut self) {
        self.fast_mode_threshold = 0.0;
    }

    /// Determine whether to use the fast model for a given query.
    ///
    /// Complexity score S = α·token_count + β·tool_call_count + γ·entropy_estimate
    /// where:
    ///   α = 0.001 (1 per 1000 tokens)
    ///   β = 0.5 (each prior tool call adds complexity)
    ///   γ = 2.0 (information density multiplier)
    ///
    /// If S < threshold → use fast model.
    pub fn should_use_fast_model(
        &self,
        input_tokens: u64,
        prior_tool_calls: u32,
        message_text: &str,
    ) -> bool {
        if self.fast_mode_threshold <= 0.0 {
            return false;
        }

        let alpha = 0.001;
        let beta = 0.5;
        let gamma = 2.0;

        let token_score = alpha * input_tokens as f64;
        let tool_score = beta * prior_tool_calls as f64;
        let entropy = estimate_entropy(message_text);
        let entropy_score = gamma * entropy;

        let total = token_score + tool_score + entropy_score;

        total < self.fast_mode_threshold
    }

    /// Get the fast model ID (for routing).
    pub fn fast_model_id(&self) -> &str {
        &self.fast_model
    }

    /// Register a custom alias.
    pub fn add_alias(&mut self, alias: &str, model_id: &str) {
        self.aliases
            .insert(alias.to_lowercase(), model_id.to_string());
    }
}

/// Estimate Shannon entropy of a text string (bits per character).
///
/// H = -Σ p(c) · log₂(p(c))
///
/// Low entropy → repetitive/simple text → simpler query.
/// High entropy → diverse/complex text → harder query.
fn estimate_entropy(text: &str) -> f64 {
    if text.is_empty() {
        return 0.0;
    }

    let mut freq = [0u32; 256];
    let len = text.len() as f64;

    for byte in text.bytes() {
        freq[byte as usize] += 1;
    }

    let mut entropy = 0.0;
    for &count in &freq {
        if count > 0 {
            let p = count as f64 / len;
            entropy -= p * p.log2();
        }
    }

    entropy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_resolution() {
        let router = ModelRouter::new();
        assert_eq!(router.resolve_alias("opus"), "claude-opus-4-20250514");
        assert_eq!(router.resolve_alias("sonnet"), "claude-sonnet-4-20250514");
        assert_eq!(router.resolve_alias("haiku"), "claude-haiku-4-5-20251001");
        assert_eq!(router.resolve_alias("gpt4o"), "gpt-4o");
        assert_eq!(router.resolve_alias("custom-model-v1"), "custom-model-v1"); // passthrough
    }

    #[test]
    fn alias_case_insensitive() {
        let router = ModelRouter::new();
        assert_eq!(router.resolve_alias("OPUS"), "claude-opus-4-20250514");
        assert_eq!(router.resolve_alias("Sonnet"), "claude-sonnet-4-20250514");
    }

    #[test]
    fn session_override_highest_priority() {
        let mut router = ModelRouter::new();
        router.init(
            Some("haiku"),              // CLI flag
            Some("sonnet"),             // project config
            None,                       // global config
            "claude-sonnet-4-20250514", // provider default
        );
        // CLI flag wins
        assert_eq!(router.current_model(), "claude-haiku-4-5-20251001");

        // Session override supersedes CLI
        router.set_session_override("opus");
        assert_eq!(router.current_model(), "claude-opus-4-20250514");
    }

    #[test]
    fn fast_mode_routing() {
        let mut router = ModelRouter::new();
        router.enable_fast_mode(5.0, "haiku");

        // Simple query: low tokens, no prior tools, low entropy
        assert!(router.should_use_fast_model(100, 0, "hello"));

        // Complex query: high tokens, many tools, high entropy
        assert!(!router.should_use_fast_model(50000, 10, "explain the quantum mechanical implications of topological insulators in the context of condensed matter physics"));
    }

    #[test]
    fn entropy_calculation() {
        // Repetitive text → low entropy
        let low = estimate_entropy("aaaaaaaaaa");
        // Diverse text → higher entropy
        let high = estimate_entropy("the quick brown fox jumps over the lazy dog");
        assert!(low < high);
    }
}
