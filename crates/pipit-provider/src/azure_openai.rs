//! Azure OpenAI provider.
//!
//! Azure OpenAI uses the same request/response format as OpenAI, but with:
//! - URL: `{endpoint}/openai/deployments/{deployment}/chat/completions?api-version={ver}`
//! - Auth: `api-key: {key}` header (also accepts `Authorization: Bearer` in 2024+ API versions)
//!
//! Usage:
//!   export AZURE_OPENAI_API_KEY=your-key
//!   pipit --provider azure_openai \
//!     --model gpt-4o \                          # used as deployment name
//!     --base-url https://myres.openai.azure.com
//!
//! Or provide the full endpoint URL directly:
//!   pipit --provider azure_openai \
//!     --model gpt-4o \
//!     --base-url "https://myres.openai.azure.com/openai/deployments/gpt-4o"
//!
//! Set AZURE_API_VERSION to override API version (default: 2024-12-01-preview).

use crate::openai::OpenAiProvider;
use crate::{
    CompletionRequest, ContentEvent, LlmProvider, ModelCapabilities, ProviderError, TokenCount,
};
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

/// Azure OpenAI provider — thin wrapper over the OpenAI provider
/// with Azure-specific URL construction.
pub struct AzureOpenAiProvider {
    inner: OpenAiProvider,
}

impl AzureOpenAiProvider {
    pub fn new(
        model: String,
        api_key: String,
        base_url: Option<String>,
    ) -> Result<Self, ProviderError> {
        // Resolve endpoint: explicit base_url > AZURE_OPENAI_ENDPOINT env var
        let endpoint = base_url
            .or_else(|| std::env::var("AZURE_OPENAI_ENDPOINT").ok())
            .ok_or_else(|| {
                ProviderError::Other(
                    "--base-url or AZURE_OPENAI_ENDPOINT required for azure_openai \
                     (e.g., https://my-resource.openai.azure.com)"
                        .into(),
                )
            })?;

        let api_version = std::env::var("AZURE_OPENAI_API_VERSION")
            .or_else(|_| std::env::var("AZURE_API_VERSION"))
            .unwrap_or_else(|_| "2024-12-01-preview".to_string());

        // Resolve deployment name: model param > AZURE_OPENAI_DEPLOYMENT env var
        let deployment = if model.is_empty() {
            std::env::var("AZURE_OPENAI_DEPLOYMENT").unwrap_or(model.clone())
        } else {
            model.clone()
        };

        // If the user already provided a full deployment URL, use it directly.
        // Otherwise, construct it from endpoint + deployment name.
        let azure_base = if endpoint.contains("/openai/deployments/") {
            // Full URL provided — strip trailing slashes
            endpoint.trim_end_matches('/').to_string()
        } else {
            // Construct: {endpoint}/openai/deployments/{deployment}
            format!(
                "{}/openai/deployments/{}",
                endpoint.trim_end_matches('/'),
                deployment
            )
        };

        // Azure expects: POST {base}/chat/completions?api-version=X
        // We set base_url to the deployment URL and chat_path to include
        // the correct path + query string (no /v1/ prefix for Azure).
        let chat_path = format!("/chat/completions?api-version={}", api_version);

        let mut inner =
            OpenAiProvider::with_id("azure_openai".to_string(), model, api_key, Some(azure_base))?;
        inner.set_chat_path(chat_path);

        Ok(Self { inner })
    }
}

#[async_trait]
impl LlmProvider for AzureOpenAiProvider {
    fn id(&self) -> &str {
        "azure_openai"
    }

    async fn complete(
        &self,
        request: CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<ContentEvent, ProviderError>> + Send>>,
        ProviderError,
    > {
        self.inner.complete(request, cancel).await
    }

    async fn count_tokens(&self, messages: &[crate::Message]) -> Result<TokenCount, ProviderError> {
        self.inner.count_tokens(messages).await
    }

    fn capabilities(&self) -> &ModelCapabilities {
        self.inner.capabilities()
    }
}
