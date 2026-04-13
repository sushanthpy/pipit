//! OpenAI Codex provider with OAuth/PKCE authentication.
//!
//! Uses the ChatGPT backend API with PKCE authorization flow:
//! 1. Generate PKCE challenge
//! 2. Open browser to `https://auth.openai.com/oauth/authorize` with code challenge
//! 3. Receive callback on `http://localhost:1455/auth/callback`
//! 4. Exchange code for tokens
//! 5. Use access token with the Responses API format
//!
//! Falls back to API key if OPENAI_API_KEY is set.

use crate::{
    oauth, CompletionRequest, ContentEvent, LlmProvider, ModelCapabilities, PreferredFormat,
    ProviderError, StopReason, TokenCount, UsageMetadata,
};
use async_trait::async_trait;
use futures::stream::Stream;
use reqwest::Client;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

const AUTH_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CLIENT_ID: &str = "app_codex";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const CODEX_SCOPE: &str = "openai.chat";

pub struct CodexOAuthProvider {
    inner: crate::openai_responses::OpenAiResponsesProvider,
    client: Client,
    /// Cached OAuth token, refreshed automatically.
    token: Arc<RwLock<Option<oauth::OAuthToken>>>,
}

impl CodexOAuthProvider {
    /// Create a new Codex OAuth provider.
    ///
    /// If `api_key` is non-empty, it's used directly (skip OAuth).
    /// Otherwise, attempts to load a cached OAuth token.
    pub fn new(
        model: String,
        api_key: String,
        base_url: Option<String>,
    ) -> Result<Self, ProviderError> {
        let effective_key = if api_key.is_empty() {
            // Try loading cached OAuth token
            if let Some(token) = oauth::load_token("openai_codex") {
                token.access_token.clone()
            } else {
                String::new()
            }
        } else {
            api_key
        };

        let inner = crate::openai_responses::OpenAiResponsesProvider::new(
            model,
            effective_key,
            base_url,
        )?;

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let cached_token = oauth::load_token("openai_codex");

        Ok(Self {
            inner,
            client,
            token: Arc::new(RwLock::new(cached_token)),
        })
    }

    /// Generate the authorization URL for the PKCE flow.
    pub fn authorization_url(pkce: &oauth::PkceChallenge) -> String {
        format!(
            "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&code_challenge={}&code_challenge_method=S256",
            AUTH_URL,
            CLIENT_ID,
            urlencoding::encode(REDIRECT_URI),
            urlencoding::encode(CODEX_SCOPE),
            pkce.challenge
        )
    }

    /// Complete the OAuth flow by exchanging an authorization code.
    pub async fn complete_auth(
        &self,
        code: &str,
        pkce_verifier: &str,
    ) -> Result<(), ProviderError> {
        let token = oauth::exchange_code(
            &self.client,
            TOKEN_URL,
            CLIENT_ID,
            code,
            REDIRECT_URI,
            pkce_verifier,
        )
        .await
        .map_err(|e| ProviderError::AuthFailed { message: e })?;

        oauth::save_token("openai_codex", &token)
            .map_err(|e| ProviderError::Other(format!("failed to save token: {e}")))?;

        *self.token.write().await = Some(token);
        Ok(())
    }

    /// Ensure we have a valid access token, refreshing if needed.
    async fn ensure_token(&self) -> Result<String, ProviderError> {
        let token_guard = self.token.read().await;
        if let Some(ref token) = *token_guard {
            if !token.is_expired() {
                return Ok(token.access_token.clone());
            }
        }
        drop(token_guard);

        // Try refresh — clone the refresh token first to avoid borrow issues
        let refresh_tok = {
            let guard = self.token.read().await;
            guard.as_ref().and_then(|t| t.refresh_token.clone())
        };
        if let Some(rt) = refresh_tok {
            match oauth::refresh_token(&self.client, TOKEN_URL, CLIENT_ID, &rt).await {
                Ok(new_token) => {
                    let access = new_token.access_token.clone();
                    let _ = oauth::save_token("openai_codex", &new_token);
                    *self.token.write().await = Some(new_token);
                    return Ok(access);
                }
                Err(e) => {
                    tracing::warn!("Codex token refresh failed: {e}");
                }
            }
        }

        Err(ProviderError::AuthFailed {
            message: "No valid Codex OAuth token. Run `pipit auth openai_codex` to authenticate."
                .into(),
        })
    }
}

#[async_trait]
impl LlmProvider for CodexOAuthProvider {
    fn id(&self) -> &str {
        "openai_codex_oauth"
    }

    async fn complete(
        &self,
        request: CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<ContentEvent, ProviderError>> + Send>>,
        ProviderError,
    > {
        // For now, delegate to the inner Responses provider.
        // A full implementation would use ensure_token() to get OAuth token
        // and make the request with that token instead.
        self.inner.complete(request, cancel).await
    }

    async fn count_tokens(&self, messages: &[crate::Message]) -> Result<TokenCount, ProviderError> {
        self.inner.count_tokens(messages).await
    }

    fn capabilities(&self) -> &ModelCapabilities {
        self.inner.capabilities()
    }
}
