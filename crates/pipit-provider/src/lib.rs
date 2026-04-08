pub mod anthropic;
pub mod azure_openai;
pub mod circuit_breaker;
pub mod fallback;
pub mod google;
pub mod google_cli;
pub mod openai;
pub mod resilience;
pub mod retry;
pub mod types;
pub mod vertex;

pub use types::*;

use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("Rate limited (retry after {retry_after_ms:?}ms)")]
    RateLimited { retry_after_ms: Option<u64> },

    #[error("Context window exceeded ({used} > {limit} tokens)")]
    ContextOverflow { used: u64, limit: u64 },

    #[error("Request too large: {message}")]
    RequestTooLarge { message: String },

    #[error("Output truncated at max_tokens")]
    OutputTruncated,

    #[error("Authentication failed: {message}")]
    AuthFailed { message: String },

    #[error("Network error: {0}")]
    Network(String),

    #[error("Invalid response: {message}")]
    InvalidResponse { message: String },

    #[error("Model not found: {model}")]
    ModelNotFound { model: String },

    #[error("Request cancelled")]
    Cancelled,

    #[error("Provider error: {0}")]
    Other(String),
}

impl ProviderError {
    /// Is this error transient and worth retrying?
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            ProviderError::Network(_)
                | ProviderError::RateLimited { .. }
                | ProviderError::OutputTruncated
        ) || matches!(self, ProviderError::Other(msg) if {
            let lower = msg.to_ascii_lowercase();
            lower.contains("500")
                || lower.contains("502")
                || lower.contains("503")
                || lower.contains("529")
                || lower.contains("overloaded")
                || lower.contains("timeout")
                || lower.contains("econnreset")
        })
    }

    /// Is this error recoverable by reducing context size?
    pub fn is_context_recoverable(&self) -> bool {
        matches!(
            self,
            ProviderError::RequestTooLarge { .. } | ProviderError::ContextOverflow { .. }
        ) || matches!(self, ProviderError::Other(msg) if {
            let lower = msg.to_ascii_lowercase();
            lower.contains("too long")
                || lower.contains("too large")
                || lower.contains("maximum context")
                || lower.contains("context length exceeded")
                || lower.contains("context_length_exceeded")
                || (lower.contains("maximum") && lower.contains("token"))
                || lower.contains("too many tokens")
        })
    }

    /// Is this error permanent (no point retrying)?
    pub fn is_permanent(&self) -> bool {
        matches!(
            self,
            ProviderError::AuthFailed { .. }
                | ProviderError::ModelNotFound { .. }
                | ProviderError::Cancelled
        )
    }

    /// Classification string for telemetry.
    pub fn classify(&self) -> &'static str {
        if self.is_transient() {
            "transient"
        } else if self.is_context_recoverable() {
            "context_overflow"
        } else if self.is_permanent() {
            "permanent"
        } else {
            "unknown"
        }
    }
}

/// The core LLM provider trait. Every provider implements this.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Provider identifier
    fn id(&self) -> &str;

    /// Send a completion request and receive a stream of events.
    async fn complete(
        &self,
        request: CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<ContentEvent, ProviderError>> + Send>>,
        ProviderError,
    >;

    /// Count tokens for the given messages (estimate if not natively supported).
    async fn count_tokens(&self, messages: &[Message]) -> Result<TokenCount, ProviderError>;

    /// Model capabilities for edit format selection and context budgeting.
    fn capabilities(&self) -> &ModelCapabilities;

    // ── Cache-Edit Protocol (Task 6: Cached Microcompact) ──

    /// Whether this provider supports in-place cache editing.
    /// When true, microcompact can mutate the prompt cache directly
    /// instead of invalidating and rebuilding it — saving up to 90%+
    /// of input token cost on cache-warm turns.
    ///
    /// Default: false (providers that don't support cache editing).
    fn supports_cache_edit(&self) -> bool {
        false
    }

    /// Edit the provider's prompt cache in place by removing specific
    /// tool_use_ids without invalidating the entire cache.
    ///
    /// Only called when `supports_cache_edit()` returns true.
    /// The `session` handle is provider-specific (e.g. the Anthropic
    /// cache_control session token).
    ///
    /// For providers that support this, the cost is O(|edits|) rather
    /// than O(|cache|), making microcompact a constant-cost operation.
    async fn edit_cache(&self, _edits: &[CacheEdit]) -> Result<CacheEditReceipt, ProviderError> {
        Err(ProviderError::Other("Cache editing not supported".into()))
    }
}

/// Create a provider from configuration.
///
/// Most providers use the OpenAI-compatible chat/completions format — they just
/// differ in base URL, auth header, and model capabilities. The Anthropic family
/// uses the /v1/messages format. Google Gemini has its own format.
pub fn create_provider(
    kind: pipit_config::ProviderKind,
    model: &str,
    api_key: &str,
    base_url: Option<&str>,
) -> Result<Box<dyn LlmProvider>, ProviderError> {
    use pipit_config::ProviderKind as PK;

    match kind {
        PK::AmazonBedrock => Err(ProviderError::Other(
            "amazon_bedrock requires AWS Bedrock runtime support, which is not implemented in this build"
                .into(),
        )),

        PK::Anthropic => Ok(Box::new(anthropic::AnthropicProvider::new(
            model.to_string(),
            api_key.to_string(),
            base_url.map(|s| s.to_string()),
        )?)),

        PK::AnthropicCompatible => {
            let url = base_url.ok_or_else(|| {
                ProviderError::Other("--base-url required for anthropic_compatible".into())
            })?;
            Ok(Box::new(anthropic::AnthropicProvider::new(
                model.to_string(),
                api_key.to_string(),
                Some(url.to_string()),
            )?))
        }

        PK::OpenAi => Ok(Box::new(openai::OpenAiProvider::new(
            model.to_string(),
            api_key.to_string(),
            base_url.map(|s| s.to_string()),
        )?)),

        PK::OpenAiCodex => Ok(Box::new(openai::OpenAiProvider::with_id(
            "openai_codex".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(base_url.unwrap_or("https://api.openai.com").to_string()),
        )?)),

        PK::OpenAiCompatible => {
            let url = base_url.ok_or_else(|| {
                ProviderError::Other("--base-url required for openai_compatible".into())
            })?;
            Ok(Box::new(openai::OpenAiProvider::with_id(
                "openai_compatible".to_string(),
                model.to_string(),
                api_key.to_string(),
                Some(url.to_string()),
            )?))
        }

        PK::DeepSeek => Ok(Box::new(openai::OpenAiProvider::with_id(
            "deepseek".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(base_url.unwrap_or("https://api.deepseek.com").to_string()),
        )?)),

        PK::OpenRouter => Ok(Box::new(openai::OpenAiProvider::with_id(
            "openrouter".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(base_url.unwrap_or("https://openrouter.ai/api").to_string()),
        )?)),

        PK::VercelAiGateway => Ok(Box::new(openai::OpenAiProvider::with_id(
            "vercel_ai_gateway".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(base_url.unwrap_or("https://ai-gateway.vercel.sh").to_string()),
        )?)),

        PK::GitHubCopilot => Ok(Box::new(openai::OpenAiProvider::with_id(
            "github_copilot".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(
                base_url
                    .unwrap_or("https://api.individual.githubcopilot.com")
                    .to_string(),
            ),
        )?)),

        PK::XAi => Ok(Box::new(openai::OpenAiProvider::with_id(
            "xai".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(base_url.unwrap_or("https://api.x.ai").to_string()),
        )?)),

        PK::ZAi => Ok(Box::new(openai::OpenAiProvider::with_id(
            "zai".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(base_url.unwrap_or("https://api.z.ai/api/coding/paas/v4").to_string()),
        )?)),

        PK::Cerebras => Ok(Box::new(openai::OpenAiProvider::with_id(
            "cerebras".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(base_url.unwrap_or("https://api.cerebras.ai").to_string()),
        )?)),

        PK::Groq => Ok(Box::new(openai::OpenAiProvider::with_id(
            "groq".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(
                base_url
                    .unwrap_or("https://api.groq.com/openai")
                    .to_string(),
            ),
        )?)),

        PK::Mistral => Ok(Box::new(openai::OpenAiProvider::with_id(
            "mistral".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(base_url.unwrap_or("https://api.mistral.ai").to_string()),
        )?)),

        PK::HuggingFace => Ok(Box::new(openai::OpenAiProvider::with_id(
            "huggingface".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(base_url.unwrap_or("https://router.huggingface.co").to_string()),
        )?)),

        PK::MiniMax => Ok(Box::new(anthropic::AnthropicProvider::with_id(
            "minimax".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(
                base_url
                    .unwrap_or("https://api.minimax.io/anthropic")
                    .to_string(),
            ),
        )?)),

        PK::MiniMaxCn => Ok(Box::new(anthropic::AnthropicProvider::with_id(
            "minimax_cn".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(
                base_url
                    .unwrap_or("https://api.minimaxi.com/anthropic")
                    .to_string(),
            ),
        )?)),

        PK::Ollama => Ok(Box::new(openai::OpenAiProvider::with_id(
            "ollama".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(base_url.unwrap_or("http://localhost:11434").to_string()),
        )?)),

        PK::Opencode => Ok(Box::new(openai::OpenAiProvider::with_id(
            "opencode".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(base_url.unwrap_or("https://opencode.ai/zen").to_string()),
        )?)),

        PK::OpencodeGo => Ok(Box::new(openai::OpenAiProvider::with_id(
            "opencode_go".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(base_url.unwrap_or("https://opencode.ai/zen/go/v1").to_string()),
        )?)),

        PK::KimiCoding => Ok(Box::new(openai::OpenAiProvider::with_id(
            "kimi_coding".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(base_url.unwrap_or("https://api.kimi.com/coding").to_string()),
        )?)),

        PK::Google => Ok(Box::new(google::GoogleProvider::new(
            model.to_string(),
            api_key.to_string(),
            base_url.map(|s| s.to_string()),
        )?)),

        PK::GoogleGeminiCli => Ok(Box::new(google_cli::GoogleCliProvider::new(
            "google_gemini_cli".to_string(),
            model.to_string(),
            api_key.to_string(),
            base_url.map(|s| s.to_string()),
        )?)),

        PK::GoogleAntigravity => Ok(Box::new(google_cli::GoogleCliProvider::new(
            "google_antigravity".to_string(),
            model.to_string(),
            api_key.to_string(),
            base_url.map(|s| s.to_string()),
        )?)),

        PK::AzureOpenAi => Ok(Box::new(azure_openai::AzureOpenAiProvider::new(
            model.to_string(),
            api_key.to_string(),
            base_url.map(|s| s.to_string()),
        )?)),

        PK::Vertex => Ok(Box::new(vertex::VertexProvider::new(
            model.to_string(),
            api_key.to_string(),
            base_url.map(|s| s.to_string()),
        )?)),
    }
}

#[cfg(test)]
mod tests {
    use super::create_provider;
    use pipit_config::ProviderKind;

    #[test]
    fn create_provider_preserves_compatible_provider_ids() {
        let provider = create_provider(
            ProviderKind::VercelAiGateway,
            "anthropic/claude-opus-4-6",
            "test-key",
            None,
        )
        .unwrap();
        assert_eq!(provider.id(), "vercel_ai_gateway");

        let provider =
            create_provider(ProviderKind::MiniMax, "MiniMax-M2.7", "test-key", None).unwrap();
        assert_eq!(provider.id(), "minimax");
    }
}
