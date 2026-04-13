//! GitHub Copilot provider with device-flow OAuth.
//!
//! Authentication uses the GitHub device flow:
//! 1. POST `/login/device/code` → get user_code and verification_uri
//! 2. User enters the code at the verification URL
//! 3. Poll `/login/oauth/access_token` with device_code
//! 4. Exchange GitHub token for Copilot token via `/copilot_internal/v2/token`
//! 5. Parse Copilot token to extract proxy endpoint URL
//!
//! The Copilot API itself is OpenAI-compatible, so we delegate to OpenAiProvider
//! after obtaining the token.

use crate::{
    oauth, CompletionRequest, ContentEvent, LlmProvider, ModelCapabilities, ProviderError,
    TokenCount,
};
use async_trait::async_trait;
use futures::stream::Stream;
use reqwest::Client;
use serde::Deserialize;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

const GITHUB_DEVICE_AUTH_URL: &str = "https://github.com/login/device/code";
const GITHUB_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
const GITHUB_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98"; // VS Code Copilot Chat
const DEFAULT_COPILOT_URL: &str = "https://api.individual.githubcopilot.com";

/// Copilot token response from GitHub API.
#[derive(Debug, Clone, Deserialize)]
struct CopilotToken {
    token: String,
    expires_at: u64,
    /// Semicolon-separated key=value pairs, including `proxy-ep=...`
    #[serde(default)]
    endpoints: Option<CopilotEndpoints>,
}

#[derive(Debug, Clone, Deserialize)]
struct CopilotEndpoints {
    api: String,
}

pub struct CopilotOAuthProvider {
    client: Client,
    model: String,
    /// Cached Copilot API token (short-lived, refreshed from GitHub token).
    copilot_token: Arc<RwLock<Option<String>>>,
    /// The base URL for the Copilot API (extracted from token).
    api_url: Arc<RwLock<String>>,
    /// GitHub OAuth token (long-lived, used to fetch Copilot tokens).
    github_token: Arc<RwLock<Option<oauth::OAuthToken>>>,
    capabilities: ModelCapabilities,
}

impl CopilotOAuthProvider {
    pub fn new(
        model: String,
        api_key: String,
        base_url: Option<String>,
    ) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        // If api_key provided, use it directly as the Copilot token
        let copilot_token = if !api_key.is_empty() {
            Some(api_key)
        } else {
            None
        };

        let api_url =
            base_url.unwrap_or_else(|| DEFAULT_COPILOT_URL.to_string());

        let github_token = oauth::load_token("github_copilot");

        let capabilities = ModelCapabilities {
            context_window: 128_000,
            max_output_tokens: 16_384,
            supports_tool_use: true,
            supports_streaming: true,
            supports_thinking: false,
            supports_images: true,
            supports_prefill: false,
            preferred_edit_format: Some(crate::PreferredFormat::SearchReplace),
        };

        Ok(Self {
            client,
            model,
            copilot_token: Arc::new(RwLock::new(copilot_token)),
            api_url: Arc::new(RwLock::new(api_url)),
            github_token: Arc::new(RwLock::new(github_token)),
            capabilities,
        })
    }

    /// Start the device flow. Returns the user code and verification URL.
    pub async fn start_device_flow(&self) -> Result<oauth::DeviceCodeResponse, ProviderError> {
        oauth::device_code_request(
            &self.client,
            GITHUB_DEVICE_AUTH_URL,
            GITHUB_CLIENT_ID,
            "read:user",
        )
        .await
        .map_err(|e| ProviderError::AuthFailed { message: e })
    }

    /// Complete the device flow by polling for the token.
    pub async fn complete_device_flow(
        &self,
        device_code: &str,
        interval: u64,
    ) -> Result<(), ProviderError> {
        let token = oauth::device_code_poll(
            &self.client,
            GITHUB_TOKEN_URL,
            GITHUB_CLIENT_ID,
            device_code,
            Duration::from_secs(interval),
            Duration::from_secs(900), // 15 min timeout
        )
        .await
        .map_err(|e| ProviderError::AuthFailed { message: e })?;

        oauth::save_token("github_copilot", &token)
            .map_err(|e| ProviderError::Other(format!("save token: {e}")))?;

        *self.github_token.write().await = Some(token);
        Ok(())
    }

    /// Exchange the GitHub token for a Copilot API token.
    async fn fetch_copilot_token(&self) -> Result<String, ProviderError> {
        let github_token = self.github_token.read().await;
        let gh_token = github_token.as_ref().ok_or(ProviderError::AuthFailed {
            message: "No GitHub token. Run `pipit auth github_copilot` to authenticate.".into(),
        })?;

        let resp = self
            .client
            .get(COPILOT_TOKEN_URL)
            .bearer_auth(&gh_token.access_token)
            .header("User-Agent", "GitHubCopilotChat/0.35.0")
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::AuthFailed {
                message: format!("Copilot token fetch failed ({status}): {body}"),
            });
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::InvalidResponse {
                message: format!("invalid copilot token response: {e}"),
            })?;

        let token = body["token"]
            .as_str()
            .ok_or(ProviderError::InvalidResponse {
                message: "missing token in copilot response".into(),
            })?
            .to_string();

        // Parse proxy endpoint from token if present
        // Token format: tid=xxx;exp=xxx;proxy-ep=proxy.individual.githubcopilot.com;...
        if let Some(proxy_ep) = Self::parse_token_field(&token, "proxy-ep") {
            let url = if proxy_ep.starts_with("http") {
                proxy_ep
            } else {
                format!("https://{proxy_ep}")
            };
            *self.api_url.write().await = url;
        }

        *self.copilot_token.write().await = Some(token.clone());
        Ok(token)
    }

    /// Parse a field from the semicolon-delimited Copilot token.
    fn parse_token_field(token: &str, field: &str) -> Option<String> {
        token
            .split(';')
            .find_map(|part| {
                let (key, val) = part.split_once('=')?;
                if key.trim() == field {
                    Some(val.trim().to_string())
                } else {
                    None
                }
            })
    }

    /// Ensure we have a valid Copilot API token.
    async fn ensure_copilot_token(&self) -> Result<String, ProviderError> {
        // If we have a direct token, use it
        let token_guard = self.copilot_token.read().await;
        if let Some(ref token) = *token_guard {
            if !token.is_empty() {
                return Ok(token.clone());
            }
        }
        drop(token_guard);

        // Fetch a new Copilot token from GitHub
        self.fetch_copilot_token().await
    }
}

#[async_trait]
impl LlmProvider for CopilotOAuthProvider {
    fn id(&self) -> &str {
        "github_copilot_oauth"
    }

    async fn complete(
        &self,
        request: CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<ContentEvent, ProviderError>> + Send>>,
        ProviderError,
    > {
        let token = self.ensure_copilot_token().await?;
        let api_url = self.api_url.read().await.clone();

        // Delegate to the OpenAI provider which handles SSE parsing.
        // The Copilot API is OpenAI-compatible, so we create a temporary
        // OpenAiProvider with the correct token and Copilot-specific headers.
        let inner = crate::openai::OpenAiProvider::with_id(
            "github_copilot".to_string(),
            self.model.clone(),
            token,
            Some(api_url),
        )?;
        inner.complete(request, cancel).await
    }

    async fn count_tokens(&self, messages: &[crate::Message]) -> Result<TokenCount, ProviderError> {
        let tokens: u64 = messages.iter().map(|m| m.estimated_tokens()).sum();
        Ok(TokenCount { tokens })
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }
}
