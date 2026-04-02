pub mod types;
pub mod anthropic;
pub mod azure_openai;
pub mod circuit_breaker;
pub mod fallback;
pub mod google;
pub mod openai;
pub mod retry;
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
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ContentEvent, ProviderError>> + Send>>, ProviderError>;

    /// Count tokens for the given messages (estimate if not natively supported).
    async fn count_tokens(&self, messages: &[Message]) -> Result<TokenCount, ProviderError>;

    /// Model capabilities for edit format selection and context budgeting.
    fn capabilities(&self) -> &ModelCapabilities;
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
        PK::Anthropic => Ok(Box::new(anthropic::AnthropicProvider::new(
            model.to_string(),
            api_key.to_string(),
            base_url.map(|s| s.to_string()),
        )?)),

        PK::AnthropicCompatible => {
            let url = base_url
                .ok_or_else(|| ProviderError::Other("--base-url required for anthropic_compatible".into()))?;
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

        PK::OpenAiCompatible => {
            let url = base_url
                .ok_or_else(|| ProviderError::Other("--base-url required for openai_compatible".into()))?;
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

        PK::XAi => Ok(Box::new(openai::OpenAiProvider::with_id(
            "xai".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(base_url.unwrap_or("https://api.x.ai").to_string()),
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
            Some(base_url.unwrap_or("https://api.groq.com/openai").to_string()),
        )?)),

        PK::Mistral => Ok(Box::new(openai::OpenAiProvider::with_id(
            "mistral".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(base_url.unwrap_or("https://api.mistral.ai").to_string()),
        )?)),

        PK::Ollama => Ok(Box::new(openai::OpenAiProvider::with_id(
            "ollama".to_string(),
            model.to_string(),
            api_key.to_string(),
            Some(base_url.unwrap_or("http://localhost:11434").to_string()),
        )?)),

        PK::Google => Ok(Box::new(google::GoogleProvider::new(
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
