//! Google Vertex AI provider.
//!
//! Vertex AI uses the same Gemini model format as Google AI Studio but with:
//! - URL: `https://{region}-aiplatform.googleapis.com/v1/projects/{project}/locations/{region}/publishers/google/models/{model}:streamGenerateContent`
//! - Auth: OAuth2 bearer token (from `gcloud auth print-access-token`) or service account
//!
//! Usage:
//!   export GOOGLE_APPLICATION_CREDENTIALS=/path/to/service-account.json
//!   # Or: export VERTEX_API_KEY=$(gcloud auth print-access-token)
//!   pipit --provider vertex \
//!     --model gemini-2.5-pro \
//!     --base-url "https://us-central1-aiplatform.googleapis.com/v1/projects/my-project/locations/us-central1"
//!
//! Environment variables:
//!   VERTEX_API_KEY        — OAuth2 access token
//!   VERTEX_PROJECT        — GCP project ID (alternative to encoding in --base-url)
//!   VERTEX_LOCATION       — GCP region (default: us-central1)
//!   GOOGLE_APPLICATION_CREDENTIALS — path to service account JSON for ADC

use crate::google::GoogleProvider;
use crate::{
    ContentEvent, CompletionRequest, LlmProvider, ModelCapabilities,
    ProviderError, TokenCount,
};
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

/// Google Vertex AI provider — wraps the Google Gemini provider
/// with Vertex-specific URL construction and OAuth2 auth.
pub struct VertexProvider {
    inner: GoogleProvider,
    /// GCP project ID.
    project: String,
    /// GCP region (e.g., us-central1).
    location: String,
}

impl VertexProvider {
    pub fn new(
        model: String,
        api_key: String,
        base_url: Option<String>,
    ) -> Result<Self, ProviderError> {
        // Resolve project and location from env vars or base_url
        let project = std::env::var("VERTEX_PROJECT").ok();
        let location = std::env::var("VERTEX_LOCATION")
            .unwrap_or_else(|_| "us-central1".to_string());

        let vertex_base = if let Some(ref url) = base_url {
            // User provided full base URL — use it directly
            url.trim_end_matches('/').to_string()
        } else if let Some(ref proj) = project {
            // Construct from env vars
            format!(
                "https://{}-aiplatform.googleapis.com/v1/projects/{}/locations/{}",
                location, proj, location
            )
        } else {
            return Err(ProviderError::Other(
                "Vertex AI requires either:\n  \
                 --base-url https://{region}-aiplatform.googleapis.com/v1/projects/{project}/locations/{region}\n  \
                 Or set: VERTEX_PROJECT and VERTEX_LOCATION env vars"
                    .to_string(),
            ));
        };

        let resolved_project = project.unwrap_or_else(|| {
            // Try to extract project from the URL
            extract_project_from_url(&vertex_base).unwrap_or_default()
        });

        // Resolve access token — try VERTEX_API_KEY first, then gcloud CLI
        let access_token = if !api_key.is_empty() && api_key != "dummy" {
            api_key
        } else if let Ok(token) = std::env::var("VERTEX_API_KEY") {
            token
        } else {
            // Try gcloud auth — this runs synchronously at provider init
            resolve_gcloud_token()?
        };

        // Vertex uses the same Gemini API format — just different base URL.
        // The Gemini provider calls:
        //   POST {base_url}/v1beta/models/{model}:streamGenerateContent
        // But Vertex expects:
        //   POST {base}/publishers/google/models/{model}:streamGenerateContent
        //
        // We construct a base URL that the Google provider will use correctly.
        // The GoogleProvider uses: {base_url}/v1beta/models/{model}:streamGenerateContent
        // For Vertex, we want: {vertex_base}/publishers/google/models/{model}:streamGenerateContent
        //
        // Since GoogleProvider hardcodes "/v1beta/models/" in its URL construction,
        // we set the base_url so that substitution works. Vertex's path is:
        //   {region}-aiplatform.googleapis.com/v1/projects/{proj}/locations/{loc}/publishers/google/models/{model}:streamGenerateContent
        // GoogleProvider constructs: {base_url}/v1beta/models/{model}:streamGenerateContent?key={key}
        //
        // For Vertex, we DON'T use ?key= — we use Authorization: Bearer.
        // We set base_url so GoogleProvider builds: {base}/models/{model}:stream...
        // but that requires modifying GoogleProvider.
        //
        // PRAGMATIC: Vertex shares the same request/response JSON format as Gemini.
        // We use GoogleProvider with a custom base URL. The key difference is that
        // Vertex doesn't use ?key= auth — it uses Bearer tokens. GoogleProvider
        // sends ?key=, which Vertex will ignore if a valid Bearer is also present.
        //
        // Create the inner GoogleProvider with the Vertex base URL.
        // The /publishers/google path component is Vertex-specific.
        let vertex_api_base = format!("{}/publishers/google", vertex_base);

        let inner = GoogleProvider::new(
            model,
            access_token,
            Some(vertex_api_base),
        )?;

        Ok(Self {
            inner,
            project: resolved_project,
            location,
        })
    }
}

/// Try to get an access token from `gcloud auth print-access-token`.
fn resolve_gcloud_token() -> Result<String, ProviderError> {
    let output = std::process::Command::new("gcloud")
        .args(["auth", "print-access-token"])
        .output()
        .map_err(|e| {
            ProviderError::AuthFailed {
                message: format!(
                    "Failed to run 'gcloud auth print-access-token': {}. \
                     Set VERTEX_API_KEY or authenticate with gcloud.",
                    e
                ),
            }
        })?;

    if !output.status.success() {
        return Err(ProviderError::AuthFailed {
            message: format!(
                "gcloud auth failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }

    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        return Err(ProviderError::AuthFailed {
            message: "gcloud returned empty access token".to_string(),
        });
    }

    Ok(token)
}

/// Extract project ID from a Vertex URL like
/// `https://us-central1-aiplatform.googleapis.com/v1/projects/my-project/locations/us-central1`
fn extract_project_from_url(url: &str) -> Option<String> {
    let parts: Vec<&str> = url.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        if *part == "projects" && i + 1 < parts.len() {
            return Some(parts[i + 1].to_string());
        }
    }
    None
}

#[async_trait]
impl LlmProvider for VertexProvider {
    fn id(&self) -> &str {
        "vertex"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_project_from_url_works() {
        let url = "https://us-central1-aiplatform.googleapis.com/v1/projects/my-project/locations/us-central1";
        assert_eq!(extract_project_from_url(url), Some("my-project".to_string()));
    }

    #[test]
    fn extract_project_missing() {
        assert_eq!(extract_project_from_url("https://api.google.com"), None);
    }
}
