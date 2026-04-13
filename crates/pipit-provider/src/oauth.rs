//! Generic PKCE OAuth module.
//!
//! Provides the core PKCE (Proof Key for Code Exchange) flow with S256 challenge,
//! token storage/refresh, and device-flow support. Used by:
//! - Anthropic OAuth (authorization code + PKCE)
//! - OpenAI Codex OAuth (authorization code + PKCE)
//! - GitHub Copilot (device flow)
//!
//! Tokens are stored to `~/.config/pipit/tokens/{provider}.json` and refreshed
//! automatically when expired.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// PKCE verifier/challenge pair.
#[derive(Debug, Clone)]
pub struct PkceChallenge {
    pub verifier: String,
    pub challenge: String,
}

impl PkceChallenge {
    /// Generate a new PKCE challenge pair using S256.
    pub fn generate() -> Self {
        use sha2::Digest;
        let mut buf = [0u8; 32];
        getrandom::fill(&mut buf).expect("failed to generate random bytes");
        let verifier = base64url_encode(&buf);
        let digest = sha2::Sha256::digest(verifier.as_bytes());
        let challenge = base64url_encode(digest.as_slice());
        Self {
            verifier,
            challenge,
        }
    }
}

/// Stored OAuth token set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: String,
    /// Absolute expiry time (seconds since UNIX epoch).
    pub expires_at: Option<u64>,
    /// Scopes granted.
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl OAuthToken {
    /// Whether the access token has expired (with 60s safety margin).
    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            Some(exp) => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                now + 60 >= exp
            }
            None => false, // No expiry → assume valid
        }
    }

    /// Create from a token response with `expires_in` seconds.
    pub fn from_response(
        access_token: String,
        refresh_token: Option<String>,
        token_type: String,
        expires_in: Option<u64>,
        scopes: Vec<String>,
    ) -> Self {
        let expires_at = expires_in.map(|secs| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                + secs
        });
        Self {
            access_token,
            refresh_token,
            token_type,
            expires_at,
            scopes,
        }
    }
}

/// Token storage path for a provider.
fn token_path(provider: &str) -> PathBuf {
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pipit")
        .join("tokens");
    config_dir.join(format!("{}.json", provider))
}

/// Load a stored token for a provider, if it exists.
pub fn load_token(provider: &str) -> Option<OAuthToken> {
    let path = token_path(provider);
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Save a token for a provider.
pub fn save_token(provider: &str, token: &OAuthToken) -> Result<(), String> {
    let path = token_path(provider);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create token dir: {e}"))?;
    }
    let data =
        serde_json::to_string_pretty(token).map_err(|e| format!("failed to serialize: {e}"))?;
    std::fs::write(&path, data).map_err(|e| format!("failed to write token: {e}"))?;
    // Restrict permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Refresh an OAuth token using the refresh_token grant.
pub async fn refresh_token(
    client: &reqwest::Client,
    token_url: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<OAuthToken, String> {
    let resp = client
        .post(token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .map_err(|e| format!("refresh request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("refresh failed ({status}): {body}"));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("invalid refresh response: {e}"))?;

    Ok(OAuthToken::from_response(
        body["access_token"]
            .as_str()
            .ok_or("missing access_token")?
            .to_string(),
        body["refresh_token"]
            .as_str()
            .map(|s| s.to_string())
            .or_else(|| Some(refresh_token.to_string())),
        body["token_type"]
            .as_str()
            .unwrap_or("Bearer")
            .to_string(),
        body["expires_in"].as_u64(),
        vec![],
    ))
}

/// Exchange an authorization code for tokens (PKCE flow).
pub async fn exchange_code(
    client: &reqwest::Client,
    token_url: &str,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<OAuthToken, String> {
    let resp = client
        .post(token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await
        .map_err(|e| format!("token exchange failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("token exchange failed ({status}): {body}"));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("invalid token response: {e}"))?;

    Ok(OAuthToken::from_response(
        body["access_token"]
            .as_str()
            .ok_or("missing access_token")?
            .to_string(),
        body["refresh_token"].as_str().map(|s| s.to_string()),
        body["token_type"]
            .as_str()
            .unwrap_or("Bearer")
            .to_string(),
        body["expires_in"].as_u64(),
        vec![],
    ))
}

/// Device flow: initiate device authorization.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    #[serde(default = "default_interval")]
    pub interval: u64,
    pub expires_in: u64,
}

fn default_interval() -> u64 {
    5
}

/// Start the device code flow.
pub async fn device_code_request(
    client: &reqwest::Client,
    device_auth_url: &str,
    client_id: &str,
    scope: &str,
) -> Result<DeviceCodeResponse, String> {
    let resp = client
        .post(device_auth_url)
        .form(&[("client_id", client_id), ("scope", scope)])
        .send()
        .await
        .map_err(|e| format!("device code request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("device code request failed ({status}): {body}"));
    }

    resp.json()
        .await
        .map_err(|e| format!("invalid device code response: {e}"))
}

/// Poll for device flow token.
pub async fn device_code_poll(
    client: &reqwest::Client,
    token_url: &str,
    client_id: &str,
    device_code: &str,
    interval: Duration,
    timeout: Duration,
) -> Result<OAuthToken, String> {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > timeout {
            return Err("device code flow timed out".into());
        }

        tokio::time::sleep(interval).await;

        let resp = client
            .post(token_url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id),
                ("device_code", device_code),
            ])
            .send()
            .await
            .map_err(|e| format!("device poll failed: {e}"))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("invalid poll response: {e}"))?;

        if let Some(error) = body["error"].as_str() {
            match error {
                "authorization_pending" => continue,
                "slow_down" => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
                "expired_token" => return Err("device code expired".into()),
                "access_denied" => return Err("user denied authorization".into()),
                _ => return Err(format!("device flow error: {error}")),
            }
        }

        return Ok(OAuthToken::from_response(
            body["access_token"]
                .as_str()
                .ok_or("missing access_token")?
                .to_string(),
            body["refresh_token"].as_str().map(|s| s.to_string()),
            body["token_type"]
                .as_str()
                .unwrap_or("Bearer")
                .to_string(),
            body["expires_in"].as_u64(),
            vec![],
        ));
    }
}

/// Ensure a valid token, refreshing if needed.
pub async fn ensure_token(
    client: &reqwest::Client,
    provider: &str,
    token_url: &str,
    client_id: &str,
) -> Result<OAuthToken, String> {
    if let Some(token) = load_token(provider) {
        if !token.is_expired() {
            return Ok(token);
        }
        // Try refresh
        if let Some(ref rt) = token.refresh_token {
            match refresh_token(client, token_url, client_id, rt).await {
                Ok(new_token) => {
                    save_token(provider, &new_token)?;
                    return Ok(new_token);
                }
                Err(e) => {
                    tracing::warn!("token refresh failed for {provider}: {e}");
                }
            }
        }
    }
    Err(format!(
        "no valid token for {provider}, re-authentication required"
    ))
}

/// Base64url encode (no padding).
fn base64url_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_has_correct_format() {
        let pkce = PkceChallenge::generate();
        // Verifier should be 43 chars (32 bytes base64url-encoded)
        assert_eq!(pkce.verifier.len(), 43);
        // Challenge should be 43 chars (32 bytes SHA-256 → base64url)
        assert_eq!(pkce.challenge.len(), 43);
        // Must not contain padding or unsafe chars
        assert!(!pkce.verifier.contains('='));
        assert!(!pkce.verifier.contains('+'));
        assert!(!pkce.verifier.contains('/'));
        assert!(!pkce.challenge.contains('='));
    }

    #[test]
    fn token_expiry_detection() {
        let fresh = OAuthToken::from_response(
            "tok".into(),
            None,
            "Bearer".into(),
            Some(3600),
            vec![],
        );
        assert!(!fresh.is_expired());

        let expired = OAuthToken {
            access_token: "tok".into(),
            refresh_token: None,
            token_type: "Bearer".into(),
            expires_at: Some(0), // Epoch → definitely expired
            scopes: vec![],
        };
        assert!(expired.is_expired());

        let no_expiry = OAuthToken {
            access_token: "tok".into(),
            refresh_token: None,
            token_type: "Bearer".into(),
            expires_at: None,
            scopes: vec![],
        };
        assert!(!no_expiry.is_expired());
    }

    #[test]
    fn token_serialization_roundtrip() {
        let tok = OAuthToken::from_response(
            "access123".into(),
            Some("refresh456".into()),
            "Bearer".into(),
            Some(3600),
            vec!["read".into(), "write".into()],
        );
        let json = serde_json::to_string(&tok).unwrap();
        let deserialized: OAuthToken = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.access_token, "access123");
        assert_eq!(deserialized.refresh_token, Some("refresh456".into()));
        assert_eq!(deserialized.scopes.len(), 2);
    }
}
