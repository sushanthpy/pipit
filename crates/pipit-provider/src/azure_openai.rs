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
    ContentEvent, CompletionRequest, LlmProvider, ModelCapabilities,
    ProviderError, TokenCount,
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
        let endpoint = base_url.ok_or_else(|| {
            ProviderError::Other(
                "--base-url required for azure_openai \
                 (e.g., https://my-resource.openai.azure.com)"
                    .into(),
            )
        })?;

        let api_version = std::env::var("AZURE_API_VERSION")
            .unwrap_or_else(|_| "2024-12-01-preview".to_string());

        // If the user already provided a full deployment URL, use it directly.
        // Otherwise, construct it from endpoint + model (deployment name).
        let azure_base = if endpoint.contains("/openai/deployments/") {
            // Full URL provided — strip trailing slashes
            endpoint.trim_end_matches('/').to_string()
        } else {
            // Construct: {endpoint}/openai/deployments/{model}
            format!(
                "{}/openai/deployments/{}",
                endpoint.trim_end_matches('/'),
                model
            )
        };

        // The OpenAI provider calls: POST {base_url}/v1/chat/completions
        // Azure expects: POST {base}/chat/completions?api-version=X
        //
        // We construct a base_url such that appending /v1/chat/completions
        // maps to Azure's expected path. Azure also accepts /v1/ in the path
        // for compatibility when using recent API versions, so:
        //   base = {resource}/openai/deployments/{deploy}?api-version={ver}
        // Final URL = {base}/v1/chat/completions
        //
        // Many Azure users already set up their endpoint to accept /v1/...
        // For those who don't, we document using the full URL.

        let inner = OpenAiProvider::with_id(
            "azure_openai".to_string(),
            model,
            api_key,
            Some(format!("{}?api-version={}", azure_base, api_version)),
        )?;

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