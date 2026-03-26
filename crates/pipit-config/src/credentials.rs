//! Credential store for pipit — persists provider credentials to
//! `~/.pipit/credentials.json`. Supports API keys, OAuth tokens (device +
//! authorization code), and Google Application Default Credentials.

use crate::ProviderKind;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Where the credential file lives.
fn credentials_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".pipit").join("credentials.json"))
}

/// A single stored credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoredCredential {
    /// Plain API key (most providers).
    ApiKey {
        api_key: String,
    },
    /// OAuth token obtained via device-code or authorization-code flow.
    OAuthToken {
        access_token: String,
        refresh_token: Option<String>,
        /// Unix timestamp in seconds. `None` = never expires.
        expires_at: Option<u64>,
        /// Which flow created this.
        #[serde(default)]
        flow: OAuthFlow,
    },
    /// Google Application Default Credentials — we shell out to `gcloud`
    /// at resolve time, so nothing is stored except the marker.
    GoogleAdc,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OAuthFlow {
    #[default]
    DeviceCode,
    AuthorizationCode,
}

/// The on-disk credential file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CredentialStore {
    #[serde(default)]
    pub credentials: HashMap<String, StoredCredential>,
}

impl CredentialStore {
    /// Load from `~/.pipit/credentials.json`, or return empty store.
    pub fn load() -> Self {
        let Some(path) = credentials_path() else {
            return Self::default();
        };
        if !path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(data) => match serde_json::from_str(&data) {
                Ok(store) => store,
                Err(e) => {
                    tracing::warn!(
                        "Failed to parse credentials at {}: {}. Starting with empty store.",
                        path.display(), e
                    );
                    Self::default()
                }
            },
            Err(e) => {
                tracing::warn!(
                    "Failed to read credentials at {}: {}. Starting with empty store.",
                    path.display(), e
                );
                Self::default()
            }
        }
    }

    /// Persist to disk. Creates `~/.pipit/` if needed.
    pub fn save(&self) -> Result<(), std::io::Error> {
        let Some(path) = credentials_path() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Cannot determine home directory",
            ));
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(&path, json)
    }

    /// Store a credential for a provider.
    pub fn set(&mut self, provider: &str, cred: StoredCredential) {
        self.credentials.insert(provider.to_string(), cred);
    }

    /// Remove a credential.
    pub fn remove(&mut self, provider: &str) -> bool {
        self.credentials.remove(provider).is_some()
    }

    /// Get a credential.
    pub fn get(&self, provider: &str) -> Option<&StoredCredential> {
        self.credentials.get(provider)
    }

    /// Resolve a usable API key / bearer token for the given provider.
    /// For GoogleAdc this shells out to `gcloud`.
    /// Returns `None` if no credential is stored or the token is expired and
    /// cannot be refreshed.
    pub fn resolve_token(&self, provider: ProviderKind) -> Option<String> {
        let key = provider.to_string();
        let cred = self.credentials.get(&key)?;
        match cred {
            StoredCredential::ApiKey { api_key } => Some(api_key.clone()),
            StoredCredential::OAuthToken {
                access_token,
                expires_at,
                ..
            } => {
                // Check expiry
                if let Some(exp) = expires_at {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    if now >= *exp {
                        tracing::warn!(
                            "OAuth token for {} expired — run `pipit auth login {}`",
                            key,
                            key
                        );
                        return None;
                    }
                }
                Some(access_token.clone())
            }
            StoredCredential::GoogleAdc => resolve_google_adc(),
        }
    }

    /// List all stored providers and their credential types.
    pub fn list(&self) -> Vec<(String, &'static str)> {
        self.credentials
            .iter()
            .map(|(k, v)| {
                let kind = match v {
                    StoredCredential::ApiKey { .. } => "api_key",
                    StoredCredential::OAuthToken { flow, .. } => match flow {
                        OAuthFlow::DeviceCode => "oauth_device",
                        OAuthFlow::AuthorizationCode => "oauth_code",
                    },
                    StoredCredential::GoogleAdc => "google_adc",
                };
                (k.clone(), kind)
            })
            .collect()
    }

    /// Path to the credential file (for display purposes).
    pub fn path() -> Option<PathBuf> {
        credentials_path()
    }
}

/// Shell out to `gcloud auth application-default print-access-token`.
fn resolve_google_adc() -> Option<String> {
    let output = std::process::Command::new("gcloud")
        .args(["auth", "application-default", "print-access-token"])
        .output()
        .ok()?;
    if !output.status.success() {
        tracing::warn!(
            "gcloud ADC failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return None;
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

// ---------- OAuth Device Code Flow ----------

/// Parameters for an OAuth device-code flow.
#[derive(Debug, Clone)]
pub struct OAuthDeviceConfig {
    pub device_auth_url: String,
    pub token_url: String,
    pub client_id: String,
    pub scopes: String,
}

/// The device-code grant response from the authorization server.
#[derive(Debug, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(alias = "verification_url")]
    pub _verification_url: Option<String>,
    pub expires_in: u64,
    pub interval: Option<u64>,
}

/// Token response from the authorization server.
#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
    pub token_type: Option<String>,
}

/// Token error from the authorization server.
#[derive(Debug, Deserialize)]
pub struct TokenErrorResponse {
    pub error: String,
    pub error_description: Option<String>,
}

/// Run the OAuth device-code flow. This is blocking on the async runtime.
///
/// 1. POST to device_auth_url → get device_code + user_code + verification_uri
/// 2. Print instructions for the user
/// 3. Poll token_url until the user authorizes or timeout
/// 4. Return the access token + refresh token
pub async fn oauth_device_flow(
    config: &OAuthDeviceConfig,
) -> Result<TokenResponse, String> {
    let client = reqwest::Client::new();

    // Step 1: Request device code
    let resp = client
        .post(&config.device_auth_url)
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("scope", config.scopes.as_str()),
        ])
        .send()
        .await
        .map_err(|e| format!("Device auth request failed: {}", e))?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Device auth failed: {}", body));
    }

    let device: DeviceCodeResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse device code response: {}", e))?;

    // Step 2: Show instructions
    eprintln!();
    eprintln!("  ╭─────────────────────────────────────────────╮");
    eprintln!("  │  Open this URL in your browser:             │");
    eprintln!("  │                                             │");
    eprintln!("  │  {}  ", device.verification_uri);
    eprintln!("  │                                             │");
    eprintln!("  │  Enter code:  {}                  ", device.user_code);
    eprintln!("  ╰─────────────────────────────────────────────╯");
    eprintln!();
    eprintln!("  Waiting for authorization...");

    // Step 3: Poll for token
    let interval = std::time::Duration::from_secs(device.interval.unwrap_or(5));
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(device.expires_in);

    loop {
        tokio::time::sleep(interval).await;

        if std::time::Instant::now() > deadline {
            return Err("Device authorization timed out".to_string());
        }

        let poll_resp = client
            .post(&config.token_url)
            .form(&[
                ("client_id", config.client_id.as_str()),
                ("device_code", device.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await
            .map_err(|e| format!("Token poll failed: {}", e))?;

        let body = poll_resp
            .text()
            .await
            .map_err(|e| format!("Token body read failed: {}", e))?;

        // Try parsing as success
        if let Ok(token) = serde_json::from_str::<TokenResponse>(&body) {
            eprintln!("  ✓ Authorization successful!");
            return Ok(token);
        }

        // Try parsing as error
        if let Ok(err) = serde_json::from_str::<TokenErrorResponse>(&body) {
            match err.error.as_str() {
                "authorization_pending" => continue,
                "slow_down" => {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
                "expired_token" => return Err("Device code expired".into()),
                "access_denied" => return Err("Authorization denied by user".into()),
                other => {
                    return Err(format!(
                        "Token error: {} — {}",
                        other,
                        err.error_description.unwrap_or_default()
                    ))
                }
            }
        }

        // Unknown response — keep polling
    }
}

/// Pre-configured OAuth device flow parameters for known providers.
pub fn oauth_device_config_for(provider: ProviderKind) -> Option<OAuthDeviceConfig> {
    match provider {
        ProviderKind::OpenAi => Some(OAuthDeviceConfig {
            device_auth_url: "https://auth0.openai.com/oauth/device/code".into(),
            token_url: "https://auth0.openai.com/oauth/token".into(),
            client_id: "pdlLIX2Y72MIl2rhLhTE9VV9bN905kBh".into(),
            scopes: "openid profile email offline_access".into(),
        }),
        // GitHub Copilot uses GitHub's device flow
        // but that requires a registered OAuth app client_id.
        // Users can set up their own via `pipit auth login openai --device`
        _ => None,
    }
}
